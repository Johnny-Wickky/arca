//! Password-protected OOXML packages (MS-OFFCRYPTO).
//!
//! The package is a compound file with an `EncryptionInfo` stream, which says how the key is
//! made from the password, and an `EncryptedPackage` stream, which holds the 8-byte plain size
//! and the encrypted ZIP.

use arca_container::cfb::CompoundFile;
use arca_container::crypto::aes::Aes;
use arca_container::crypto::{Hash, constant_time_eq};

use super::{DecryptError, MAX_SPIN};
use crate::PreviewPolicy;
use crate::xml::{self, Flow, Handler, Tag, XmlError};

/// The largest package that is decrypted.
const MAX_PACKAGE: usize = 256 << 20;

fn malformed(why: &str) -> DecryptError {
    DecryptError::Malformed(why.to_owned())
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at.checked_add(4)?)?.try_into().ok().map(u32::from_le_bytes)
}

fn utf16(password: &str) -> Vec<u8> {
    password.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Decrypts an encrypted OOXML package to the ZIP it holds.
pub(crate) fn decrypt(data: &[u8], password: &str) -> Result<Vec<u8>, DecryptError> {
    let policy = PreviewPolicy::default();
    let cf = CompoundFile::parse(data, policy.cfb)
        .map_err(|e| DecryptError::Malformed(format!("compound file: {e}")))?;
    let (Some(info), Some(package)) = (cf.find("EncryptionInfo"), cf.find("EncryptedPackage"))
    else {
        return Err(DecryptError::NotEncrypted);
    };
    let info = cf.read_stream(info).map_err(|e| DecryptError::Malformed(e.to_string()))?;
    let package = cf.read_stream(package).map_err(|e| DecryptError::Malformed(e.to_string()))?;
    let (major, minor) = (
        info.get(0..2).and_then(|b| b.try_into().ok()).map(u16::from_le_bytes),
        info.get(2..4).and_then(|b| b.try_into().ok()).map(u16::from_le_bytes),
    );
    match (major, minor) {
        (Some(4), Some(4)) => agile(&info, &package, password),
        (Some(3 | 4), Some(2)) => standard(&info, &package, password),
        (Some(a), Some(b)) => Err(DecryptError::Unsupported(format!("encryption version {a}.{b}"))),
        _ => Err(malformed("EncryptionInfo is truncated")),
    }
}

// ---------------------------------------------------------------------------------------------
// Standard encryption (Office 2007)

fn standard(info: &[u8], package: &[u8], password: &str) -> Result<Vec<u8>, DecryptError> {
    let header_size = usize::try_from(u32_at(info, 8).ok_or_else(|| malformed("no header"))?)
        .unwrap_or(usize::MAX);
    let header = info
        .get(12..12usize.saturating_add(header_size))
        .filter(|h| h.len() >= 32)
        .ok_or_else(|| malformed("the encryption header is truncated"))?;
    let alg_id = u32_at(header, 8).unwrap_or(0);
    let hash_id = u32_at(header, 12).unwrap_or(0);
    let key_bits = usize::try_from(u32_at(header, 16).unwrap_or(0)).unwrap_or(0);
    if !matches!(alg_id, 0x660E..=0x6610) {
        return Err(DecryptError::Unsupported(format!("cipher {alg_id:#x} (only AES)")));
    }
    if hash_id != 0x8004 {
        return Err(DecryptError::Unsupported(format!("hash {hash_id:#x} (only SHA-1)")));
    }
    if !matches!(key_bits, 128 | 192 | 256) {
        return Err(malformed("the key size is not an AES size"));
    }
    let verifier =
        info.get(12usize.saturating_add(header_size)..).ok_or_else(|| malformed("no verifier"))?;
    let salt_size = usize::try_from(u32_at(verifier, 0).unwrap_or(0)).unwrap_or(0);
    if salt_size != 16 {
        return Err(malformed("the salt is not 16 bytes"));
    }
    let salt = verifier.get(4..20).ok_or_else(|| malformed("the salt is truncated"))?;
    let enc_verifier =
        verifier.get(20..36).ok_or_else(|| malformed("the verifier is truncated"))?;
    let hash_size = usize::try_from(u32_at(verifier, 36).unwrap_or(0)).unwrap_or(0);
    let enc_hash =
        verifier.get(40..72).ok_or_else(|| malformed("the verifier hash is truncated"))?;
    if hash_size != 20 {
        return Err(malformed("the verifier hash is not SHA-1 sized"));
    }

    // 50 000 rounds of SHA-1 over the password, then the derivation of MS-OFFCRYPTO 2.3.4.7.
    let pw = utf16(password);
    let mut buf = [0u8; 64];
    let mut n = Hash::Sha1.digest_into(&[salt, &pw], &mut buf);
    for i in 0..50_000u32 {
        let prev = buf;
        n = Hash::Sha1.digest_into(&[&i.to_le_bytes(), prev.get(..n).unwrap_or(&[])], &mut buf);
    }
    let h = buf.get(..n).unwrap_or(&[]).to_vec();
    let h_final = Hash::Sha1.digest(&[&h, &0u32.to_le_bytes()]);
    let pad = |byte: u8| -> Vec<u8> {
        let mut b = vec![byte; 64];
        for (x, y) in b.iter_mut().zip(&h_final) {
            *x ^= y;
        }
        Hash::Sha1.digest(&[&b])
    };
    let mut derived = pad(0x36);
    derived.extend(pad(0x5C));
    derived.truncate(key_bits / 8);
    let aes = Aes::new(&derived).ok_or_else(|| malformed("bad key length"))?;

    let verifier_plain = aes.ecb_decrypt(enc_verifier);
    let expected = Hash::Sha1.digest(&[&verifier_plain]);
    let got = aes.ecb_decrypt(enc_hash);
    if !constant_time_eq(&expected, got.get(..20).unwrap_or(&[])) {
        return Err(DecryptError::WrongPassword);
    }
    let size = package_size(package)?;
    let body = package.get(8..).unwrap_or(&[]);
    let mut out = aes.ecb_decrypt(body);
    out.truncate(size);
    Ok(out)
}

/// The plain size written at the start of `EncryptedPackage`, checked against what follows.
fn package_size(package: &[u8]) -> Result<usize, DecryptError> {
    let size: [u8; 8] = package
        .get(..8)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| malformed("EncryptedPackage is truncated"))?;
    let size = usize::try_from(u64::from_le_bytes(size)).unwrap_or(usize::MAX);
    if size > MAX_PACKAGE || size > package.len() {
        return Err(malformed("the package size is not believable"));
    }
    Ok(size)
}

// ---------------------------------------------------------------------------------------------
// Agile encryption (Office 2010 and later)

#[derive(Debug, Default)]
struct KeyData {
    salt: Vec<u8>,
    block_size: usize,
    key_bits: usize,
    hash: Option<Hash>,
    cipher_ok: bool,
    chaining_ok: bool,
}

#[derive(Debug, Default)]
struct PasswordKey {
    data: KeyData,
    spin: u32,
    verifier_input: Vec<u8>,
    verifier_hash: Vec<u8>,
    key_value: Vec<u8>,
}

#[derive(Default)]
struct AgileInfo {
    key_data: Option<KeyData>,
    password: Option<PasswordKey>,
}

fn base64(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xFF).unwrap_or(0));
        }
    }
    out
}

fn hash_named(name: &str) -> Option<Hash> {
    match name {
        "SHA1" => Some(Hash::Sha1),
        "SHA256" => Some(Hash::Sha256),
        "SHA384" => Some(Hash::Sha384),
        "SHA512" => Some(Hash::Sha512),
        _ => None,
    }
}

fn number(tag: &Tag<'_>, name: &str) -> usize {
    tag.attr(name).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

fn key_data_of(tag: &Tag<'_>) -> KeyData {
    KeyData {
        salt: tag.attr("saltValue").map(|v| base64(&v)).unwrap_or_default(),
        block_size: number(tag, "blockSize"),
        key_bits: number(tag, "keyBits"),
        hash: tag.attr("hashAlgorithm").and_then(|v| hash_named(v.trim())),
        cipher_ok: tag.attr("cipherAlgorithm").is_some_and(|v| v.trim() == "AES"),
        chaining_ok: tag.attr("cipherChaining").is_some_and(|v| v.trim() == "ChainingModeCBC"),
    }
}

impl Handler for AgileInfo {
    fn start(&mut self, tag: &Tag<'_>, _empty: bool) -> Flow {
        match tag.local() {
            "keyData" if self.key_data.is_none() => self.key_data = Some(key_data_of(tag)),
            "encryptedKey" if self.password.is_none() => {
                let b = |n: &str| tag.attr(n).map(|v| base64(&v)).unwrap_or_default();
                self.password = Some(PasswordKey {
                    data: key_data_of(tag),
                    spin: u32::try_from(number(tag, "spinCount")).unwrap_or(u32::MAX),
                    verifier_input: b("encryptedVerifierHashInput"),
                    verifier_hash: b("encryptedVerifierHashValue"),
                    key_value: b("encryptedKeyValue"),
                });
            }
            _ => {}
        }
        Flow::Continue
    }

    fn end(&mut self, _local: &str) {}

    fn text(&mut self, _raw: &str, _cdata: bool) -> Result<(), XmlError> {
        Ok(())
    }
}

/// `key` fitted to `len` bytes: cut, or padded with `0x36`.
fn fit(mut key: Vec<u8>, len: usize) -> Vec<u8> {
    key.resize(len, 0x36);
    key
}

fn agile(info: &[u8], package: &[u8], password: &str) -> Result<Vec<u8>, DecryptError> {
    let xml_bytes = info.get(8..).ok_or_else(|| malformed("no descriptor"))?;
    let text = String::from_utf8_lossy(xml_bytes);
    let text = text.trim_start_matches('\u{FEFF}');
    let mut parsed = AgileInfo::default();
    xml::drive(text, &PreviewPolicy::default().xml, &mut parsed)
        .map_err(|e| DecryptError::Malformed(format!("descriptor: {e}")))?;
    let (Some(kd), Some(pk)) = (parsed.key_data, parsed.password) else {
        return Err(DecryptError::Unsupported("no password key encryptor".into()));
    };
    for d in [&kd, &pk.data] {
        if !d.cipher_ok || !d.chaining_ok {
            return Err(DecryptError::Unsupported("cipher other than AES-CBC".into()));
        }
        if !matches!(d.key_bits, 128 | 192 | 256) || d.block_size != 16 {
            return Err(malformed("unusual key or block size"));
        }
    }
    let (Some(kd_hash), Some(pk_hash)) = (kd.hash, pk.data.hash) else {
        return Err(DecryptError::Unsupported("hash algorithm".into()));
    };
    if pk.spin > MAX_SPIN {
        return Err(DecryptError::TooExpensive);
    }
    if pk.data.salt.len() != 16 || kd.salt.len() != 16 {
        return Err(malformed("unusual salt size"));
    }

    // The password hash, stretched.
    let pw = utf16(password);
    let mut buf = [0u8; 64];
    let mut n = pk_hash.digest_into(&[&pk.data.salt, &pw], &mut buf);
    for i in 0..pk.spin {
        let prev = buf;
        n = pk_hash.digest_into(&[&i.to_le_bytes(), prev.get(..n).unwrap_or(&[])], &mut buf);
    }
    let h = buf.get(..n).unwrap_or(&[]).to_vec();
    let key_len = pk.data.key_bits / 8;
    let derive = |block: [u8; 8]| fit(pk_hash.digest(&[&h, &block]), key_len);
    let decrypt = |block: [u8; 8], data: &[u8]| -> Result<Vec<u8>, DecryptError> {
        let aes = Aes::new(&derive(block)).ok_or_else(|| malformed("bad key length"))?;
        aes.cbc_decrypt(&pk.data.salt, data).ok_or_else(|| malformed("bad block"))
    };
    let verifier = decrypt([0xfe, 0xa7, 0xd2, 0x76, 0x3b, 0x4b, 0x9e, 0x79], &pk.verifier_input)?;
    let expected = pk_hash.digest(&[verifier.get(..16).unwrap_or(&verifier)]);
    let stored = decrypt([0xd7, 0xaa, 0x0f, 0x6d, 0x30, 0x61, 0x34, 0x4e], &pk.verifier_hash)?;
    if !constant_time_eq(&expected, stored.get(..expected.len()).unwrap_or(&[])) {
        return Err(DecryptError::WrongPassword);
    }
    let mut key = decrypt([0x14, 0x6e, 0x0b, 0xe7, 0xab, 0xac, 0xd0, 0xd6], &pk.key_value)?;
    key.truncate(kd.key_bits / 8);
    let aes = Aes::new(&key).ok_or_else(|| malformed("bad package key"))?;

    // The package, in segments of 4096 bytes with an IV each.
    let size = package_size(package)?;
    let mut out = Vec::with_capacity(size);
    for (i, segment) in package.get(8..).unwrap_or(&[]).chunks(4096).enumerate() {
        let index = u32::try_from(i).map_err(|_| malformed("too many segments"))?;
        let iv = kd_hash.digest(&[&kd.salt, &index.to_le_bytes()]);
        let plain = aes
            .cbc_decrypt(iv.get(..16).unwrap_or(&iv), segment)
            .ok_or_else(|| malformed("bad segment"))?;
        out.extend_from_slice(&plain);
        if out.len() >= size {
            break;
        }
    }
    out.truncate(size);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewPolicy;
    use crate::testkit::ok;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/").to_owned() + name,
        )
        .expect("a fixture")
    }

    fn texts(data: &[u8]) -> String {
        let p = ok(data);
        format!("{:?}", p.sections)
    }

    #[test]
    fn agile_and_standard_packages_open_with_their_passwords() {
        for (file, plain, password) in [
            ("enc-agile.docx", "report.docx", "пароль-123"),
            ("enc-agile.xlsx", "sheet.xlsx", "s3cret"),
            ("enc-standard.docx", "report.docx", "Password1234_"),
        ] {
            let got = decrypt(&fixture(file), password).expect(file);
            assert_eq!(got, fixture(plain), "{file}");
            assert!(texts(&got).contains("Квартальный отчёт") || texts(&got).contains("Яблоки"));
        }
    }

    #[test]
    fn a_wrong_password_is_recognised_and_nothing_leaks() {
        for file in ["enc-agile.docx", "enc-standard.docx"] {
            let r = decrypt(&fixture(file), "not it");
            assert_eq!(r, Err(DecryptError::WrongPassword), "{file}");
            assert_eq!(decrypt(&fixture(file), ""), Err(DecryptError::WrongPassword), "{file}");
        }
    }

    #[test]
    fn the_package_reports_itself_as_encrypted() {
        assert!(super::super::is_encrypted_package(&fixture("enc-agile.docx")));
        assert!(!super::super::is_encrypted_package(&fixture("report.docx")));
        assert!(!super::super::is_encrypted_package(&fixture("report.doc")));
        assert_eq!(decrypt(&fixture("report.doc"), "x"), Err(DecryptError::NotEncrypted));
        let _ = PreviewPolicy::default();
    }

    #[test]
    fn damaged_encryption_data_is_refused_not_trusted() {
        let data = fixture("enc-agile.docx");
        let mut rng = crate::testkit::Lcg::new(11);
        for _ in 0..crate::testkit::fuzz_iterations(60) {
            let mut m = data.clone();
            rng.mutate(&mut m, 8);
            // Whatever happens, it ends in an error or in some bytes — never a panic, and the
            // expensive part is bounded.
            let _ = decrypt(&m, "пароль-123");
        }
        // A spin count beyond the cap is refused before any hashing.
        let mut cf_bytes = data;
        let needle = b"spinCount=\"100000\"";
        // The descriptor is stored uncompressed in a stream: patch it where it can be found.
        let at = cf_bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("the descriptor is stored in clear");
        cf_bytes[at..at + needle.len()].copy_from_slice(b"spinCount=\"999999\"");
        assert_eq!(decrypt(&cf_bytes, "пароль-123"), Err(DecryptError::TooExpensive));
    }
}
