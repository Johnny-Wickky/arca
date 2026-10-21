//! Builds small PDF files for tests.

use miniz_oxide::deflate::compress_to_vec_zlib;

/// A PDF under construction. Object 1 must be the catalog.
#[derive(Debug, Clone, Default)]
pub(crate) struct PdfBuilder {
    /// `(number, body, is_stream)`.
    objs: Vec<(u32, Vec<u8>, bool)>,
    trailer_extra: String,
    lie_length: bool,
}

impl PdfBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Adds (or replaces) a non-stream object whose body is `body`.
    pub(crate) fn add_raw(&mut self, num: u32, body: &str) {
        self.objs.retain(|(n, _, _)| *n != num);
        self.objs.push((num, body.as_bytes().to_vec(), false));
    }

    /// Adds (or replaces) a stream object. `dict_extra` is placed inside the dictionary.
    pub(crate) fn add_stream(&mut self, num: u32, dict_extra: &str, data: &[u8]) {
        self.objs.retain(|(n, _, _)| *n != num);
        let mut body = format!("<< {dict_extra} /Length {} >>\nstream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.objs.push((num, body, true));
    }

    /// Like [`PdfBuilder::add_stream`] with the data Flate-compressed.
    pub(crate) fn add_flate_stream(&mut self, num: u32, dict_extra: &str, data: &[u8]) {
        let z = compress_to_vec_zlib(data, 6);
        self.add_stream(num, &format!("{dict_extra} /Filter /FlateDecode"), &z);
    }

    /// One page with a Helvetica font `F1` and the given content stream.
    pub(crate) fn with_content(content: &str) -> Self {
        let mut b = Self::new();
        b.add_raw(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.add_raw(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.add_raw(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        b.add_stream(4, "", content.as_bytes());
        b.add_raw(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b
    }

    /// One page showing `text`.
    pub(crate) fn simple_text(text: &str) -> Self {
        Self::with_content(&format!("BT /F1 12 Tf 72 700 Td ({text}) Tj ET"))
    }

    /// Declares a wrong `/Length` for every stream.
    pub(crate) fn with_length_lie(mut self) -> Self {
        self.lie_length = true;
        self
    }

    /// Adds `entry` (for example `/Encrypt 9 0 R`) to the trailer.
    pub(crate) fn with_trailer_entry(mut self, entry: &str) -> Self {
        self.trailer_extra.push(' ');
        self.trailer_extra.push_str(entry);
        self
    }

    fn sorted(&self) -> Vec<(u32, Vec<u8>, bool)> {
        let mut v = self.objs.clone();
        v.sort_by_key(|(n, _, _)| *n);
        v
    }

    fn body_of(&self, body: &[u8]) -> Vec<u8> {
        if !self.lie_length {
            return body.to_vec();
        }
        // Replace the declared length by a wrong one.
        let text = String::from_utf8_lossy(body).into_owned();
        match text.find("/Length ") {
            Some(p) => {
                let rest = text.get(p + 8..).unwrap_or("");
                let digits = rest.chars().take_while(char::is_ascii_digit).count();
                let mut out = text.get(..p + 8).unwrap_or("").to_owned();
                out.push('3');
                out.push_str(rest.get(digits..).unwrap_or(""));
                out.into_bytes()
            }
            None => body.to_vec(),
        }
    }

    /// Classic layout: plain objects, `xref` table, `trailer`.
    pub(crate) fn build(&self) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let objs = self.sorted();
        let max = objs.iter().map(|(n, _, _)| *n).max().unwrap_or(0);
        let mut offsets = std::collections::BTreeMap::new();
        for (n, body, _) in &objs {
            offsets.insert(*n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(&self.body_of(body));
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max + 1).as_bytes());
        for n in 1..=max {
            match offsets.get(&n) {
                Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 00000 f \n"),
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R{} >>\nstartxref\n{xref}\n%%EOF\n",
                max + 1,
                self.trailer_extra
            )
            .as_bytes(),
        );
        out
    }

    /// PDF 1.5 layout: non-stream objects in an object stream, cross-reference stream with a
    /// PNG predictor.
    pub(crate) fn build_modern(&self) -> Vec<u8> {
        let objs = self.sorted();
        let max = objs.iter().map(|(n, _, _)| *n).max().unwrap_or(0);
        let objstm_num = max + 1;
        let xref_num = max + 2;
        let packed: Vec<&(u32, Vec<u8>, bool)> = objs.iter().filter(|(_, _, s)| !s).collect();
        // Object stream.
        let mut header = String::new();
        let mut body: Vec<u8> = Vec::new();
        for (n, b, _) in &packed {
            header.push_str(&format!("{n} {} ", body.len()));
            body.extend_from_slice(b);
            body.push(b'\n');
        }
        let first = header.len();
        let mut stm_data = header.into_bytes();
        stm_data.extend_from_slice(&body);
        let mut b = Self::new();
        b.add_flate_stream(
            objstm_num,
            &format!("/Type /ObjStm /N {} /First {first}", packed.len()),
            &stm_data,
        );
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut entries: std::collections::BTreeMap<u32, (u8, u64, u64)> =
            std::collections::BTreeMap::new();
        entries.insert(0, (0, 0, 65535));
        for (n, body, is_stream) in &objs {
            if *is_stream {
                entries.insert(*n, (1, out.len() as u64, 0));
                out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
                out.extend_from_slice(&self.body_of(body));
                out.extend_from_slice(b"\nendobj\n");
            }
        }
        for (i, (n, _, _)) in packed.iter().enumerate() {
            entries.insert(*n, (2, u64::from(objstm_num), i as u64));
        }
        entries.insert(objstm_num, (1, out.len() as u64, 0));
        if let Some((_, body, _)) = b.objs.first() {
            out.extend_from_slice(format!("{objstm_num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_off = out.len();
        entries.insert(xref_num, (1, xref_off as u64, 0));
        // Rows of 1 + 4 + 2 bytes, PNG "Up" predictor.
        let mut rows: Vec<Vec<u8>> = Vec::new();
        for n in 0..=xref_num {
            let (t, a, c) = entries.get(&n).copied().unwrap_or((0, 0, 0));
            let mut r = vec![t];
            r.extend_from_slice(&u32::try_from(a).unwrap_or(0).to_be_bytes());
            r.extend_from_slice(&u16::try_from(c).unwrap_or(0).to_be_bytes());
            rows.push(r);
        }
        let mut raw = Vec::new();
        let mut prev = vec![0u8; 7];
        for r in &rows {
            raw.push(2);
            raw.extend(r.iter().zip(&prev).map(|(a, b)| a.wrapping_sub(*b)));
            prev = r.clone();
        }
        let z = compress_to_vec_zlib(&raw, 6);
        let dict = format!(
            "<< /Type /XRef /Size {} /W [1 4 2] /Root 1 0 R{} /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 7 >> /Length {} >>",
            xref_num + 1,
            self.trailer_extra,
            z.len()
        );
        out.extend_from_slice(format!("{xref_num} 0 obj\n{dict}\nstream\n").as_bytes());
        out.extend_from_slice(&z);
        out.extend_from_slice(
            format!("\nendstream\nendobj\nstartxref\n{xref_off}\n%%EOF\n").as_bytes(),
        );
        out
    }
}
