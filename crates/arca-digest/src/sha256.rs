//! SHA-256 as specified in FIPS 180-4.

use core::fmt;

const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const H0: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// A 256-bit SHA-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

/// Error returned when parsing a hexadecimal digest fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestParseError {
    /// The input was not exactly 64 hexadecimal characters.
    InvalidLength,
    /// The input contained a character outside `[0-9a-fA-F]`.
    InvalidCharacter,
}

impl fmt::Display for DigestParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => f.write_str("digest must be exactly 64 hexadecimal characters"),
            Self::InvalidCharacter => f.write_str("digest contains a non-hexadecimal character"),
        }
    }
}

impl core::error::Error for DigestParseError {}

impl Digest {
    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parses a lowercase or uppercase hexadecimal digest.
    ///
    /// # Errors
    /// Returns [`DigestParseError`] if `hex` is not 64 hexadecimal characters.
    pub fn from_hex(hex: &str) -> Result<Self, DigestParseError> {
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return Err(DigestParseError::InvalidLength);
        }
        let mut out = [0u8; 32];
        for (dst, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
            let (hi, lo) = match pair {
                [h, l] => (nibble(*h)?, nibble(*l)?),
                _ => return Err(DigestParseError::InvalidLength),
            };
            *dst = (hi << 4) | lo;
        }
        Ok(Self(out))
    }

    /// Writes the lowercase hexadecimal representation into `out`.
    pub fn write_hex(&self, out: &mut impl fmt::Write) -> fmt::Result {
        for byte in self.0 {
            write!(out, "{byte:02x}")?;
        }
        Ok(())
    }
}

fn nibble(c: u8) -> Result<u8, DigestParseError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(DigestParseError::InvalidCharacter),
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_hex(f)
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Digest(")?;
        self.write_hex(f)?;
        f.write_str(")")
    }
}

/// Incremental SHA-256 hasher.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    block_len: usize,
    total_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print intermediate state: it may be derived from secret input.
        f.debug_struct("Sha256").finish_non_exhaustive()
    }
}

impl Sha256 {
    /// Creates a hasher in its initial state.
    #[must_use]
    pub const fn new() -> Self {
        Self { state: H0, block: [0u8; 64], block_len: 0, total_len: 0 }
    }

    /// Feeds `data` into the hasher.
    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);

        if self.block_len > 0 {
            let free = 64usize.saturating_sub(self.block_len);
            let take = free.min(data.len());
            let (head, tail) = data.split_at(take);
            if let Some(dst) =
                self.block.get_mut(self.block_len..self.block_len.saturating_add(take))
            {
                dst.copy_from_slice(head);
            }
            self.block_len = self.block_len.saturating_add(take);
            data = tail;
            if self.block_len < 64 {
                // `data` is exhausted: everything went into the partial block.
                return;
            }
            let block = self.block;
            compress(&mut self.state, &block);
            self.block_len = 0;
        }

        let mut chunks = data.chunks_exact(64);
        for chunk in &mut chunks {
            let mut block = [0u8; 64];
            block.copy_from_slice(chunk);
            compress(&mut self.state, &block);
        }
        let rest = chunks.remainder();
        if let Some(dst) = self.block.get_mut(..rest.len()) {
            dst.copy_from_slice(rest);
            self.block_len = rest.len();
        }
    }

    /// Consumes the hasher and returns the digest.
    #[must_use]
    pub fn finalize(mut self) -> Digest {
        let bit_len = self.total_len.wrapping_mul(8);
        // Padding: 0x80, zeros, then the 64-bit big-endian message length.
        let mut pad = [0u8; 72];
        pad[0] = 0x80;
        let pad_len = if self.block_len < 56 { 56 - self.block_len } else { 120 - self.block_len };
        let total_len = self.total_len;
        if let Some(p) = pad.get(..pad_len) {
            self.update(p);
        }
        self.update(&bit_len.to_be_bytes());
        // `update` adds the padding to `total_len`; restore for hygiene.
        self.total_len = total_len;

        let mut out = [0u8; 32];
        for (dst, word) in out.chunks_exact_mut(4).zip(self.state.iter()) {
            dst.copy_from_slice(&word.to_be_bytes());
        }
        Digest(out)
    }
}

/// One-shot SHA-256 of `data`.
#[must_use]
pub fn sha256(data: &[u8]) -> Digest {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

#[allow(
    clippy::indexing_slicing,
    reason = "fixed-size arrays indexed by loop counters bounded by the array length; \
              the bounds are compile-time facts of the algorithm"
)]
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (dst, src) in w.iter_mut().zip(block.chunks_exact(4)) {
        *dst = u32::from_be_bytes([src[0], src[1], src[2], src[3]]);
    }
    for t in 16..64 {
        let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
        let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
        w[t] = w[t - 16].wrapping_add(s0).wrapping_add(w[t - 7]).wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (k, wt) in K.iter().zip(w.iter()) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(*k).wrapping_add(*wt);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    fn hex(d: &Digest) -> String {
        let mut s = String::new();
        d.write_hex(&mut s).unwrap();
        s
    }

    #[test]
    fn fips_180_2_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn million_a() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&sha256(&data)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn incremental_matches_one_shot_for_all_split_points() {
        let data: Vec<u8> = (0..300u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let expected = sha256(&data);
        for split in 0..data.len() {
            let mut h = Sha256::new();
            let (a, b) = data.split_at(split);
            h.update(a);
            h.update(b);
            assert_eq!(h.finalize(), expected, "split at {split}");
        }
    }

    #[test]
    fn hex_roundtrip_and_errors() {
        let d = sha256(b"abc");
        let parsed = Digest::from_hex(&hex(&d)).unwrap();
        assert_eq!(parsed, d);
        assert_eq!(Digest::from_hex("abc"), Err(DigestParseError::InvalidLength));
        let bad = "zz".repeat(32);
        assert_eq!(Digest::from_hex(&bad), Err(DigestParseError::InvalidCharacter));
    }
}
