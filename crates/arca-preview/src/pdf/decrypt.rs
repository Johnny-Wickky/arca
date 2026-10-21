//! Opening an encrypted PDF: the key is worked out from the password ([`super::crypt`]), and the
//! whole file is written out again without its encryption — every object with its strings and
//! stream data decrypted, object streams unpacked — so that the X-ray, the preview and the
//! sterile copy see the same file they would see without a password.

use std::rc::Rc;

use super::clean::{hex_string, name, real};
use super::crypt::Crypt;
use super::file::PdfFile;
use super::lex::{Dict, MAX_DEPTH, Obj};
use crate::decrypt::{DecryptError, Decrypted};
use crate::{PreviewError, PreviewPolicy};

/// Objects written at most.
const MAX_OBJECTS: usize = 4_000_000;

fn crypt_for(pdf: &mut PdfFile<'_>, password: &[u8]) -> Result<Crypt, DecryptError> {
    let trailer = pdf.trailer.clone();
    let encrypt_obj = match trailer.get("Encrypt") {
        Some(Obj::Ref(n, _)) => Some(*n),
        _ => None,
    };
    let enc = pdf.get(&trailer, "Encrypt");
    let Some(enc) = enc.as_dict().cloned() else {
        return Err(DecryptError::Malformed("PDF: the encryption dictionary is missing".into()));
    };
    let cf = pdf.get(&enc, "CF").as_dict().cloned().unwrap_or_default();
    let id = pdf.get_array(&trailer, "ID");
    let id0 = id.first().and_then(|o| o.as_str().map(<[u8]>::to_vec)).unwrap_or_default();
    Crypt::new(&enc, &cf, &id0, password, encrypt_obj)
}

pub(crate) fn decrypt(data: &[u8], password: &[u8]) -> Result<Decrypted, DecryptError> {
    let policy = PreviewPolicy::default();
    let mut pdf = PdfFile::open_with(data, &policy, true).map_err(|e| match e {
        PreviewError::Malformed(why) => DecryptError::Malformed(why),
        other => DecryptError::Malformed(other.to_string()),
    })?;
    if !pdf.encrypted() {
        return Err(DecryptError::NotEncrypted);
    }
    // A file protected only against editing or printing opens with the empty password.
    let (crypt, without_password) = match crypt_for(&mut pdf, b"") {
        Ok(c) => (c, true),
        Err(DecryptError::WrongPassword) if !password.is_empty() => {
            (crypt_for(&mut pdf, password)?, false)
        }
        Err(e) => return Err(e),
    };
    pdf.set_crypt(Rc::new(crypt));
    let bytes = write_plain(&mut pdf)?;
    Ok(Decrypted { bytes, without_password })
}

fn write_obj(pdf: &PdfFile<'_>, o: &Obj, out: &mut Vec<u8>, depth: usize) {
    if depth > MAX_DEPTH {
        out.extend_from_slice(b"null");
        return;
    }
    match o {
        Obj::Null | Obj::Stream(_) => out.extend_from_slice(b"null"),
        Obj::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Obj::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Obj::Real(r) => out.extend_from_slice(real(*r).as_bytes()),
        Obj::Str(s) => hex_string(s, out),
        Obj::Name(n) => name(n, out),
        Obj::Arr(items) => {
            out.push(b'[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_obj(pdf, it, out, depth + 1);
            }
            out.push(b']');
        }
        Obj::Dict(d) => write_dict(pdf, d, out, depth, false),
        Obj::Ref(n, _) => out.extend_from_slice(format!("{n} 0 R").as_bytes()),
    }
}

fn write_dict(pdf: &PdfFile<'_>, d: &Dict, out: &mut Vec<u8>, depth: usize, stream: bool) {
    out.extend_from_slice(b"<<");
    for (k, v) in d {
        // The length is rewritten; a `Crypt` filter has done its work.
        if stream && matches!(&**k, "Length") {
            continue;
        }
        let v = if stream { without_crypt_filter(k, v) } else { Some(v.clone()) };
        let Some(v) = v else { continue };
        out.push(b' ');
        name(k, out);
        out.push(b' ');
        write_obj(pdf, &v, out, depth + 1);
    }
    out.extend_from_slice(b" >>");
}

/// `/Filter` and `/DecodeParms` without a leading `Crypt` entry.
fn without_crypt_filter(key: &str, v: &Obj) -> Option<Obj> {
    match (key, v) {
        ("Filter", Obj::Name(n)) if &**n == "Crypt" => None,
        ("Filter", Obj::Arr(a)) if a.first().and_then(Obj::as_name) == Some("Crypt") => {
            let rest: Vec<Obj> = a.iter().skip(1).cloned().collect();
            (!rest.is_empty()).then(|| Obj::Arr(rest.into()))
        }
        _ => Some(v.clone()),
    }
}

fn write_plain(pdf: &mut PdfFile<'_>) -> Result<Vec<u8>, DecryptError> {
    let encrypt_obj = match pdf.trailer.get("Encrypt") {
        Some(Obj::Ref(n, _)) => Some(*n),
        _ => None,
    };
    let numbers = pdf.object_numbers();
    if numbers.len() > MAX_OBJECTS {
        return Err(DecryptError::Malformed("PDF: too many objects".into()));
    }
    let mut out: Vec<u8> = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<(u32, usize)> = Vec::new();
    for n in numbers {
        if Some(n) == encrypt_obj {
            continue;
        }
        let obj = pdf.object(n);
        let mut body = Vec::new();
        match &obj {
            Obj::Null => continue,
            Obj::Stream(s) => {
                let ty = s.dict.get("Type").and_then(Obj::as_name).unwrap_or("");
                // Object streams are unpacked into objects of their own, cross-reference
                // streams are replaced by a table.
                if matches!(ty, "XRef" | "ObjStm") {
                    continue;
                }
                let raw = pdf.raw(s).into_owned();
                write_dict(pdf, &s.dict, &mut body, 1, true);
                // The dictionary was closed by `write_dict`; reopen it for the length.
                body.truncate(body.len().saturating_sub(3));
                body.extend_from_slice(format!(" /Length {} >>\nstream\n", raw.len()).as_bytes());
                body.extend_from_slice(&raw);
                body.extend_from_slice(b"\nendstream");
            }
            other => write_obj(pdf, other, &mut body, 0),
        }
        offsets.push((n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let size = offsets.iter().map(|(n, _)| *n).max().unwrap_or(0).saturating_add(1);
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    let by_number: std::collections::BTreeMap<u32, usize> = offsets.into_iter().collect();
    for n in 0..size {
        match by_number.get(&n) {
            Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    let mut trailer = format!("trailer\n<< /Size {size}");
    for key in ["Root", "Info"] {
        if let Some(Obj::Ref(n, _)) = pdf.trailer.get(key) {
            trailer.push_str(&format!(" /{key} {n} 0 R"));
        }
    }
    out.extend_from_slice(trailer.as_bytes());
    let id = pdf.trailer.get("ID").cloned();
    if let Some(id) = id {
        out.extend_from_slice(b" /ID ");
        write_obj(pdf, &id, &mut out, 0);
    }
    out.extend_from_slice(format!(" >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PreviewPolicy, preview};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/").to_owned() + name,
        )
        .expect("a fixture")
    }

    fn words(pdf: &[u8]) -> String {
        let p = preview(pdf, &PreviewPolicy::default()).expect("a preview");
        p.sections
            .iter()
            .flat_map(crate::testkit::paragraphs)
            .map(|x| x.text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_revision_opens_with_its_password_and_reads_like_the_original() {
        let plain = words(&fixture("report.pdf"));
        assert!(plain.contains("Квартальный отчёт"), "{plain}");
        for (file, password) in [
            ("enc-rc4-40.pdf", "user-40"),
            ("enc-rc4-128.pdf", "user-128"),
            ("enc-aes-128.pdf", "pässword"),
            ("enc-aes-256.pdf", "user-256"),
            // The owner password opens it as well.
            ("enc-rc4-128.pdf", "owner"),
            ("enc-aes-128.pdf", "owner"),
            ("enc-aes-256.pdf", "owner"),
        ] {
            let data = fixture(file);
            assert_eq!(preview(&data, &PreviewPolicy::default()), Err(PreviewError::Encrypted));
            let got = decrypt(&data, password.as_bytes()).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(!got.without_password, "{file}");
            assert_eq!(words(&got.bytes), plain, "{file}");
            assert!(!got.bytes.windows(8).any(|w| w == b"/Encrypt"), "{file}");
        }
    }

    #[test]
    fn wrong_passwords_are_refused() {
        for file in ["enc-rc4-40.pdf", "enc-rc4-128.pdf", "enc-aes-128.pdf", "enc-aes-256.pdf"] {
            let data = fixture(file);
            assert_eq!(decrypt(&data, b"nope").err(), Some(DecryptError::WrongPassword), "{file}");
            assert_eq!(decrypt(&data, b"").err(), Some(DecryptError::WrongPassword), "{file}");
        }
    }

    #[test]
    fn a_file_with_an_empty_user_password_opens_by_itself() {
        let data = fixture("enc-open.pdf");
        let got = decrypt(&data, b"").expect("opens");
        assert!(got.without_password);
        assert!(words(&got.bytes).contains("Квартальный отчёт"));
        // Whatever is passed in addition changes nothing.
        assert_eq!(decrypt(&data, b"anything").map(|d| d.bytes), Ok(got.bytes));
    }

    #[test]
    fn plain_files_are_not_encrypted_and_junk_is_not_a_panic() {
        assert_eq!(decrypt(&fixture("report.pdf"), b"x").err(), Some(DecryptError::NotEncrypted));
        let data = fixture("enc-rc4-128.pdf");
        let mut rng = crate::testkit::Lcg::new(5);
        for _ in 0..crate::testkit::fuzz_iterations(120) {
            let mut m = data.clone();
            rng.mutate(&mut m, 6);
            let _ = decrypt(&m, b"user-128");
        }
        for cut in [0usize, 10, 100, 1000, 5000] {
            let _ = decrypt(data.get(..cut).unwrap_or(&data), b"user-128");
        }
        // The AES-256 key derivation is slow in a debug build: a few mutations only.
        let data = fixture("enc-aes-256.pdf");
        for _ in 0..crate::testkit::fuzz_iterations(4) {
            let mut m = data.clone();
            rng.mutate(&mut m, 6);
            let _ = decrypt(&m, b"user-256");
        }
    }
}
