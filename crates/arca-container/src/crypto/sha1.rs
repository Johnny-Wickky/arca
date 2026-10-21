// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! SHA-1 (FIPS 180-4). Not collision resistant; kept for the key derivation of the formats that
//! prescribe it.

/// Incremental SHA-1.
#[derive(Debug, Clone)]
pub struct Sha1 {
    state: [u32; 5],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    /// A fresh hasher.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    /// Feeds more data.
    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        while !data.is_empty() {
            let take = (64 - self.filled).min(data.len());
            let (head, rest) = data.split_at(take);
            if let Some(dst) = self.block.get_mut(self.filled..self.filled + take) {
                dst.copy_from_slice(head);
            }
            self.filled += take;
            data = rest;
            if self.filled == 64 {
                let block = self.block;
                compress(&mut self.state, &block);
                self.filled = 0;
            }
        }
    }

    /// The digest.
    #[must_use]
    pub fn finalize(mut self) -> [u8; 20] {
        let bits = self.length.wrapping_mul(8);
        let at = self.filled;
        if let Some(b) = self.block.get_mut(at) {
            *b = 0x80;
        }
        if let Some(rest) = self.block.get_mut(at + 1..) {
            rest.fill(0);
        }
        if at + 1 > 56 {
            let block = self.block;
            compress(&mut self.state, &block);
            self.block = [0; 64];
        }
        if let Some(len) = self.block.get_mut(56..) {
            len.copy_from_slice(&bits.to_be_bytes());
        }
        let block = self.block;
        compress(&mut self.state, &block);
        let mut out = [0u8; 20];
        for (chunk, word) in out.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// One block; see the note on SHA-512's `compress` about the indices.
#[allow(clippy::indexing_slicing)]
fn compress(state: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 16];
    for (slot, c) in w.iter_mut().zip(block.chunks_exact(4)) {
        *slot = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for i in 0..80 {
        let wi = if i < 16 {
            w[i]
        } else {
            let v =
                (w[(i + 13) & 15] ^ w[(i + 8) & 15] ^ w[(i + 2) & 15] ^ w[i & 15]).rotate_left(1);
            w[i & 15] = v;
            v
        };
        let (f, k) = match i / 20 {
            0 => ((b & c) | (!b & d), 0x5a82_7999),
            1 => (b ^ c ^ d, 0x6ed9_eba1),
            2 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
            _ => (b ^ c ^ d, 0xca62_c1d6),
        };
        let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
        *s = s.wrapping_add(v);
    }
}

/// SHA-1 of `data`.
#[must_use]
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(data);
    h.finalize()
}
