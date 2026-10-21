// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! RC4. Broken as a stream cipher; present because PDF revisions 2–4 and the legacy Office
//! formats use it, and only to read what its owner already holds the key to.

/// The RC4 keystream state.
#[derive(Debug, Clone)]
pub struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    /// Key schedule. (An empty key is meaningless; it gives the unkeyed permutation.)
    #[must_use]
    pub fn new(key: &[u8]) -> Self {
        let mut s = [0u8; 256];
        for (i, slot) in s.iter_mut().enumerate() {
            *slot = u8::try_from(i).unwrap_or(0);
        }
        let mut j = 0u8;
        if !key.is_empty() {
            for i in 0..256usize {
                let k = key.get(i % key.len()).copied().unwrap_or(0);
                j = j.wrapping_add(s.get(i).copied().unwrap_or(0)).wrapping_add(k);
                s.swap(i, usize::from(j));
            }
        }
        Self { s, i: 0, j: 0 }
    }

    /// Encrypts or decrypts `data` in place (the same operation).
    pub fn apply(&mut self, data: &mut [u8]) {
        for byte in data {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s.get(usize::from(self.i)).copied().unwrap_or(0));
            self.s.swap(usize::from(self.i), usize::from(self.j));
            let t = self
                .s
                .get(usize::from(self.i))
                .copied()
                .unwrap_or(0)
                .wrapping_add(self.s.get(usize::from(self.j)).copied().unwrap_or(0));
            *byte ^= self.s.get(usize::from(t)).copied().unwrap_or(0);
        }
    }
}

/// `data` run through RC4 with `key`.
#[must_use]
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    Rc4::new(key).apply(&mut out);
    out
}
