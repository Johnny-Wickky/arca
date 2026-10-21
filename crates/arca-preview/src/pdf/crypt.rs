//! The PDF standard security handler (ISO 32000-1 §7.6, ISO 32000-2 §7.6): working out the file
//! key from a password and decrypting strings and streams with it.
//!
//! Revisions 2–4 derive the key with MD5 and encrypt with RC4 or AES-128; revisions 5 and 6
//! (AES-256) with SHA-256 — and, for revision 6, a hash that also runs SHA-384/512 and AES.

use std::rc::Rc;

use arca_container::crypto::aes::Aes;
use arca_container::crypto::md5::Md5;
use arca_container::crypto::rc4::Rc4;
use arca_container::crypto::{Hash, constant_time_eq};

use super::lex::{Dict, Obj, Stream};
use crate::decrypt::DecryptError;

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// How one kind of data (strings or streams) is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Method {
    None,
    Rc4,
    AesV2,
    AesV3,
}

/// The key of an opened document and how it is applied.
#[derive(Debug)]
pub(super) struct Crypt {
    key: Vec<u8>,
    strings: Method,
    streams: Method,
    encrypt_metadata: bool,
    /// The object that holds the encryption dictionary (never decrypted itself).
    pub(super) encrypt_obj: Option<u32>,
}

fn malformed(why: &str) -> DecryptError {
    DecryptError::Malformed(format!("PDF: {why}"))
}

fn md5(parts: &[&[u8]]) -> [u8; 16] {
    let mut h = Md5::new();
    parts.iter().for_each(|p| h.update(p));
    h.finalize()
}

/// The password as the revision 2–4 algorithms take it: Latin-1, cut or padded to 32 bytes.
fn padded(password: &[u8]) -> [u8; 32] {
    let latin1: Vec<u8> = String::from_utf8_lossy(password)
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect();
    let mut out = PAD;
    for (slot, b) in out.iter_mut().zip(&latin1) {
        *slot = *b;
    }
    if latin1.len() < 32 {
        for (slot, p) in out.iter_mut().skip(latin1.len()).zip(PAD.iter()) {
            *slot = *p;
        }
    }
    out
}

struct Params<'a> {
    r: i64,
    key_len: usize,
    o: &'a [u8],
    u: &'a [u8],
    oe: &'a [u8],
    ue: &'a [u8],
    p: u32,
    id0: &'a [u8],
    encrypt_metadata: bool,
}

/// Algorithm 2: the file key from a padded password.
fn key_from_padded(pw: &[u8; 32], par: &Params<'_>) -> Vec<u8> {
    let p = par.p.to_le_bytes();
    let ff = [0xFFu8; 4];
    let mut parts: Vec<&[u8]> = vec![pw, par.o.get(..32).unwrap_or(par.o), &p, par.id0];
    if par.r >= 4 && !par.encrypt_metadata {
        parts.push(&ff);
    }
    let mut h = md5(&parts).to_vec();
    let n = if par.r == 2 { 5 } else { par.key_len.clamp(5, 16) };
    if par.r >= 3 {
        for _ in 0..50 {
            h = md5(&[h.get(..n).unwrap_or(&h)]).to_vec();
        }
    }
    h.truncate(n);
    h
}

/// Algorithms 4 and 5: the value `U` should have for `key`.
fn user_check(key: &[u8], par: &Params<'_>) -> Vec<u8> {
    if par.r == 2 {
        let mut d = PAD.to_vec();
        Rc4::new(key).apply(&mut d);
        return d;
    }
    let mut d = md5(&[&PAD, par.id0]).to_vec();
    Rc4::new(key).apply(&mut d);
    for i in 1..=19u8 {
        let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        Rc4::new(&k).apply(&mut d);
    }
    d
}

/// Algorithm 7: the padded user password hidden in `O`, given the owner password.
fn user_password_from_owner(owner_pw: &[u8; 32], par: &Params<'_>) -> Vec<u8> {
    let mut h = md5(&[owner_pw]).to_vec();
    if par.r >= 3 {
        for _ in 0..50 {
            h = md5(&[&h]).to_vec();
        }
    }
    let n = if par.r == 2 { 5 } else { par.key_len.clamp(5, 16) };
    let key = h.get(..n).unwrap_or(&h).to_vec();
    let mut d = par.o.get(..32).unwrap_or(par.o).to_vec();
    if par.r == 2 {
        Rc4::new(&key).apply(&mut d);
    } else {
        for i in (0..=19u8).rev() {
            let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
            Rc4::new(&k).apply(&mut d);
        }
    }
    d
}

/// The revision 6 hash (ISO 32000-2 algorithm 2.B); revision 5 is plain SHA-256.
fn hash_r56(r: i64, password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut k = Hash::Sha256.digest(&[password, salt, udata]);
    if r < 6 {
        return k;
    }
    let mut round = 0usize;
    loop {
        let unit: Vec<u8> = [password, &k, udata].concat();
        let k1: Vec<u8> = unit.iter().cycle().take(unit.len() * 64).copied().collect();
        let e = Aes::new(k.get(..16).unwrap_or(&[]))
            .and_then(|a| a.cbc_encrypt(k.get(16..32).unwrap_or(&[]), &k1))
            .unwrap_or_default();
        let sum: u32 = e.iter().take(16).map(|&b| u32::from(b)).sum();
        k = match sum % 3 {
            0 => Hash::Sha256.digest(&[&e]),
            1 => Hash::Sha384.digest(&[&e]),
            _ => Hash::Sha512.digest(&[&e]),
        };
        round += 1;
        let last = usize::from(e.last().copied().unwrap_or(0));
        if round >= 64 && last <= round - 32 {
            break;
        }
        if round > 4_096 {
            break;
        }
    }
    k.truncate(32);
    k
}

fn method_of(d: &Dict, cf: &Dict, name: &str) -> Method {
    let key = d.get(name).and_then(Obj::as_name).unwrap_or("Identity");
    if key == "Identity" {
        return Method::None;
    }
    let cm = cf
        .get(key)
        .and_then(|o| o.as_dict().cloned())
        .and_then(|f| f.get("CFM").and_then(Obj::as_name).map(str::to_owned));
    match cm.as_deref() {
        Some("V2") => Method::Rc4,
        Some("AESV2") => Method::AesV2,
        Some("AESV3") => Method::AesV3,
        _ => Method::None,
    }
}

impl Crypt {
    /// Sets up decryption from the encryption dictionary and the first element of the file
    /// identifier. `Err(WrongPassword)` if the password opens neither the user nor the owner
    /// side.
    pub(super) fn new(
        enc: &Dict,
        cf: &Dict,
        id0: &[u8],
        password: &[u8],
        encrypt_obj: Option<u32>,
    ) -> Result<Self, DecryptError> {
        if enc.get("Filter").and_then(Obj::as_name) != Some("Standard") {
            return Err(DecryptError::Unsupported("security handler other than Standard".into()));
        }
        let int = |k: &str| enc.get(k).and_then(Obj::as_int).unwrap_or(0);
        let (v, r) = (int("V"), int("R"));
        if !matches!(v, 1 | 2 | 4 | 5) || !(2..=6).contains(&r) {
            return Err(DecryptError::Unsupported(format!("encryption V{v} R{r}")));
        }
        let bytes = |k: &str| enc.get(k).and_then(Obj::as_str).unwrap_or(&[]).to_vec();
        let (o, u, oe, ue) = (bytes("O"), bytes("U"), bytes("OE"), bytes("UE"));
        let encrypt_metadata = !matches!(enc.get("EncryptMetadata"), Some(Obj::Bool(false)));
        let key_len = match v {
            1 => 5,
            4 => 16,
            5 => 32,
            _ => usize::try_from(int("Length").clamp(40, 128) / 8).unwrap_or(5),
        };
        let par = Params {
            r,
            key_len,
            o: &o,
            u: &u,
            oe: &oe,
            ue: &ue,
            p: u32::try_from(int("P") & 0xFFFF_FFFF).unwrap_or(0),
            id0,
            encrypt_metadata,
        };
        let (strings, streams) = match v {
            4 | 5 => (method_of(enc, cf, "StrF"), method_of(enc, cf, "StmF")),
            _ => (Method::Rc4, Method::Rc4),
        };
        let key = if r >= 5 { key_r56(&par, password)? } else { key_r234(&par, password)? };
        Ok(Self { key, strings, streams, encrypt_metadata, encrypt_obj })
    }

    fn object_key(&self, num: u32, generation: u16, method: Method) -> Vec<u8> {
        if method == Method::AesV3 {
            return self.key.clone();
        }
        let n = num.to_le_bytes();
        let g = generation.to_le_bytes();
        let salt: &[u8] = if method == Method::AesV2 { b"sAlT" } else { b"" };
        let h = md5(&[&self.key, n.get(..3).unwrap_or(&n), &g, salt]);
        h.get(..(self.key.len() + 5).min(16)).unwrap_or(&h).to_vec()
    }

    fn apply(&self, method: Method, data: &[u8], num: u32, generation: u16) -> Vec<u8> {
        match method {
            Method::None => data.to_vec(),
            Method::Rc4 => {
                let mut d = data.to_vec();
                Rc4::new(&self.object_key(num, generation, method)).apply(&mut d);
                d
            }
            Method::AesV2 | Method::AesV3 => {
                let (Some(iv), Some(body)) = (data.get(..16), data.get(16..)) else {
                    return Vec::new();
                };
                let Some(aes) = Aes::new(&self.object_key(num, generation, method)) else {
                    return Vec::new();
                };
                let plain = aes.cbc_decrypt(iv, body).unwrap_or_default();
                arca_container::crypto::aes::unpad_pkcs7(plain.clone()).unwrap_or(plain)
            }
        }
    }

    /// Decrypts a string of object `num generation`.
    pub(super) fn string(&self, s: &[u8], num: u32, generation: u16) -> Vec<u8> {
        self.apply(self.strings, s, num, generation)
    }

    /// Whether the data of this stream is encrypted at all.
    fn stream_encrypted(&self, dict: &Dict) -> bool {
        let ty = dict.get("Type").and_then(Obj::as_name).unwrap_or("");
        if ty == "XRef" || (ty == "Metadata" && !self.encrypt_metadata) {
            return false;
        }
        // A stream with its own `Crypt` filter says which crypt filter applies; only the
        // identity filter leaves the data alone.
        let filters: Vec<&str> = match dict.get("Filter") {
            Some(Obj::Name(n)) => vec![&**n],
            Some(Obj::Arr(a)) => a.iter().filter_map(Obj::as_name).collect(),
            _ => Vec::new(),
        };
        if filters.first() == Some(&"Crypt") {
            let parms = match dict.get("DecodeParms") {
                Some(Obj::Arr(a)) => a.first().cloned(),
                other => other.cloned(),
            };
            let name = parms
                .as_ref()
                .and_then(Obj::as_dict)
                .and_then(|d| d.get("Name"))
                .and_then(Obj::as_name)
                .unwrap_or("Identity");
            return name != "Identity";
        }
        true
    }

    /// The decrypted data of stream `s` whose raw bytes are `raw`.
    pub(super) fn stream(&self, s: &Stream, raw: &[u8]) -> Vec<u8> {
        if !self.stream_encrypted(&s.dict) {
            return raw.to_vec();
        }
        self.apply(self.streams, raw, s.num, s.generation)
    }

    /// Whether streams are encrypted by this document (so a stream's bytes differ from the file's).
    pub(super) fn touches_streams(&self, s: &Stream) -> bool {
        self.streams != Method::None && self.stream_encrypted(&s.dict)
    }

    /// `o` with every string decrypted as part of object `num generation`.
    pub(super) fn object(&self, o: &Obj, num: u32, generation: u16) -> Obj {
        match o {
            Obj::Str(s) => Obj::Str(Rc::from(self.string(s, num, generation))),
            Obj::Arr(a) => Obj::Arr(a.iter().map(|x| self.object(x, num, generation)).collect()),
            Obj::Dict(d) => Obj::Dict(Rc::new(self.dict(d, num, generation))),
            Obj::Stream(s) => Obj::Stream(Rc::new(Stream {
                dict: self.dict(&s.dict, num, generation),
                start: s.start,
                end: s.end,
                num: s.num,
                generation: s.generation,
            })),
            other => other.clone(),
        }
    }

    fn dict(&self, d: &Dict, num: u32, generation: u16) -> Dict {
        d.iter().map(|(k, v)| (k.clone(), self.object(v, num, generation))).collect()
    }
}

fn key_r234(par: &Params<'_>, password: &[u8]) -> Result<Vec<u8>, DecryptError> {
    if par.u.len() < 32 || par.o.len() < 32 {
        return Err(malformed("O or U is shorter than 32 bytes"));
    }
    let matches = |key: &[u8]| {
        let check = user_check(key, par);
        let n = if par.r == 2 { 32 } else { 16 };
        constant_time_eq(check.get(..n).unwrap_or(&[]), par.u.get(..n).unwrap_or(&[]))
    };
    let pw = padded(password);
    let key = key_from_padded(&pw, par);
    if matches(&key) {
        return Ok(key);
    }
    // Perhaps it is the owner password.
    let user_pw = user_password_from_owner(&pw, par);
    let mut padded_user = [0u8; 32];
    for (slot, b) in padded_user.iter_mut().zip(&user_pw) {
        *slot = *b;
    }
    let key = key_from_padded(&padded_user, par);
    if matches(&key) { Ok(key) } else { Err(DecryptError::WrongPassword) }
}

fn key_r56(par: &Params<'_>, password: &[u8]) -> Result<Vec<u8>, DecryptError> {
    if par.u.len() < 48 || par.o.len() < 48 || par.ue.len() < 32 || par.oe.len() < 32 {
        return Err(malformed("O, U, OE or UE is too short"));
    }
    // Passwords are UTF-8, at most 127 bytes.
    let pw = password.get(..password.len().min(127)).unwrap_or(password);
    let file_key = |ik: &[u8], wrapped: &[u8]| -> Result<Vec<u8>, DecryptError> {
        let aes = Aes::new(ik).ok_or_else(|| malformed("bad intermediate key"))?;
        aes.cbc_decrypt(&[0; 16], wrapped.get(..32).unwrap_or(wrapped))
            .ok_or_else(|| malformed("bad key block"))
    };
    let u48 = par.u.get(..48).unwrap_or(par.u);
    // User password.
    if constant_time_eq(
        &hash_r56(par.r, pw, par.u.get(32..40).unwrap_or(&[]), &[]),
        par.u.get(..32).unwrap_or(&[]),
    ) {
        let ik = hash_r56(par.r, pw, par.u.get(40..48).unwrap_or(&[]), &[]);
        return file_key(&ik, par.ue);
    }
    // Owner password.
    if constant_time_eq(
        &hash_r56(par.r, pw, par.o.get(32..40).unwrap_or(&[]), u48),
        par.o.get(..32).unwrap_or(&[]),
    ) {
        let ik = hash_r56(par.r, pw, par.o.get(40..48).unwrap_or(&[]), u48);
        return file_key(&ik, par.oe);
    }
    Err(DecryptError::WrongPassword)
}
