// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! MD5 (RFC 1321). Broken as a collision-resistant hash and kept only because the PDF standard
//! security handler derives its keys with it; nothing here relies on collision resistance.

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// `floor(2^32 × abs(sin(i + 1)))`.
const K: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// Incremental MD5.
#[derive(Debug, Clone)]
pub struct Md5 {
    state: [u32; 4],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    /// A fresh hasher.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    /// Feeds more data.
    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        while !data.is_empty() {
            let room = 64 - self.filled;
            let take = room.min(data.len());
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
    pub fn finalize(mut self) -> [u8; 16] {
        let bits = self.length.wrapping_mul(8);
        let zeros = (55usize.wrapping_sub(self.filled)) % 64;
        let mut tail = Vec::with_capacity(1 + zeros + 8);
        tail.push(0x80);
        tail.resize(1 + zeros, 0);
        tail.extend_from_slice(&bits.to_le_bytes());
        self.update(&tail);
        let mut out = [0u8; 16];
        for (chunk, word) in out.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}

fn compress(state: &mut [u32; 4], block: &[u8; 64]) {
    let mut m = [0u32; 16];
    for (w, c) in m.iter_mut().zip(block.chunks_exact(4)) {
        *w = u32::from_le_bytes([
            c.first().copied().unwrap_or(0),
            c.get(1).copied().unwrap_or(0),
            c.get(2).copied().unwrap_or(0),
            c.get(3).copied().unwrap_or(0),
        ]);
    }
    let [mut a, mut b, mut c, mut d] = *state;
    for i in 0..64 {
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let sum = a
            .wrapping_add(f)
            .wrapping_add(K.get(i).copied().unwrap_or(0))
            .wrapping_add(m.get(g).copied().unwrap_or(0));
        a = d;
        d = c;
        c = b;
        b = b.wrapping_add(sum.rotate_left(S.get(i).copied().unwrap_or(0)));
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d]) {
        *s = s.wrapping_add(v);
    }
}

/// MD5 of `data`.
#[must_use]
pub fn md5(data: &[u8]) -> [u8; 16] {
    let mut h = Md5::new();
    h.update(data);
    h.finalize()
}
