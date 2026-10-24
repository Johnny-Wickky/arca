// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! AES-128, AES-192 and AES-256 (FIPS 197) with the CBC and ECB modes of operation that
//! Office and PDF use. Table-driven but without secret-dependent branching beyond table
//! lookups; there is no claim of side-channel resistance — the key is the reader's own.

/// The lookup tables, computed by the compiler. Everything in here runs at compile time, where
/// an out-of-range index would be a build error rather than a run-time panic.
#[allow(clippy::indexing_slicing, clippy::cast_possible_truncation)]
mod tables {
    pub(super) const fn xtime(x: u8) -> u8 {
        (x << 1) ^ if x & 0x80 != 0 { 0x1b } else { 0 }
    }

    const fn gmul(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0u8;
        let mut i = 0;
        while i < 8 {
            if b & 1 != 0 {
                p ^= a;
            }
            a = xtime(a);
            b >>= 1;
            i += 1;
        }
        p
    }

    /// The S-box, computed from the field inverse and the affine map instead of being typed in.
    pub(super) const SBOX: [u8; 256] = {
        let mut sbox = [0u8; 256];
        let mut inv = [0u8; 256];
        let mut x = 1usize;
        while x < 256 {
            // The inverse by exhaustive search; evaluated once, at compile time.
            let mut y = 1usize;
            while y < 256 {
                if gmul(x as u8, y as u8) == 1 {
                    inv[x] = y as u8;
                    break;
                }
                y += 1;
            }
            x += 1;
        }
        let mut i = 0usize;
        while i < 256 {
            let b = inv[i];
            let s = b
                ^ b.rotate_left(1)
                ^ b.rotate_left(2)
                ^ b.rotate_left(3)
                ^ b.rotate_left(4)
                ^ 0x63;
            sbox[i] = s;
            i += 1;
        }
        sbox
    };

    pub(super) const INV_SBOX: [u8; 256] = {
        let mut inv = [0u8; 256];
        let mut i = 0usize;
        while i < 256 {
            inv[SBOX[i] as usize] = i as u8;
            i += 1;
        }
        inv
    };

    const fn mul_table(m: u8) -> [u8; 256] {
        let mut t = [0u8; 256];
        let mut i = 0usize;
        while i < 256 {
            t[i] = gmul(i as u8, m);
            i += 1;
        }
        t
    }

    pub(super) const MUL2: [u8; 256] = mul_table(2);
    pub(super) const MUL3: [u8; 256] = mul_table(3);
    pub(super) const MUL9: [u8; 256] = mul_table(9);
    pub(super) const MUL11: [u8; 256] = mul_table(11);
    pub(super) const MUL13: [u8; 256] = mul_table(13);
    pub(super) const MUL14: [u8; 256] = mul_table(14);
}

use tables::{INV_SBOX, MUL2, MUL3, MUL9, MUL11, MUL13, MUL14, SBOX, xtime};

use zeroize::Zeroizing;

fn at(t: &[u8; 256], i: u8) -> u8 {
    t.get(usize::from(i)).copied().unwrap_or(0)
}

/// An expanded AES key.
///
/// The key schedule is key material: it is wiped when the value is dropped, it cannot be
/// cloned (every copy would be one more place to wipe) and its `Debug` output shows nothing
/// of it.
pub struct Aes {
    /// Round keys, 16 bytes each.
    round_keys: Zeroizing<Vec<[u8; 16]>>,
}

impl core::fmt::Debug for Aes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Aes").field("rounds", &self.round_keys.len().saturating_sub(1)).finish()
    }
}

impl Aes {
    /// Expands a 16-, 24- or 32-byte key. `None` for any other length.
    #[must_use]
    pub fn new(key: &[u8]) -> Option<Self> {
        let nk = match key.len() {
            16 => 4usize,
            24 => 6,
            32 => 8,
            _ => return None,
        };
        let rounds = nk + 6;
        let mut w: Zeroizing<Vec<[u8; 4]>> = Zeroizing::new(
            key.chunks_exact(4).filter_map(|c| <[u8; 4]>::try_from(c).ok()).collect(),
        );
        let mut rcon = 1u8;
        for i in nk..4 * (rounds + 1) {
            let mut t = *w.get(i - 1)?;
            if i % nk == 0 {
                let [a, b, c, d] = t;
                t = [at(&SBOX, b) ^ rcon, at(&SBOX, c), at(&SBOX, d), at(&SBOX, a)];
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|x| at(&SBOX, x));
            }
            let prev = *w.get(i - nk)?;
            let mut next = prev;
            for (n, x) in next.iter_mut().zip(t) {
                *n ^= x;
            }
            w.push(next);
        }
        let round_keys = w
            .chunks_exact(4)
            .map(|c| {
                let mut k = [0u8; 16];
                for (dst, word) in k.chunks_exact_mut(4).zip(c) {
                    dst.copy_from_slice(word);
                }
                k
            })
            .collect();
        Some(Self { round_keys: Zeroizing::new(round_keys) })
    }

    fn xor_key(&self, state: &mut [u8; 16], round: usize) {
        if let Some(k) = self.round_keys.get(round) {
            for (s, b) in state.iter_mut().zip(k) {
                *s ^= b;
            }
        }
    }

    /// Encrypts one block.
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        let rounds = self.round_keys.len().saturating_sub(1);
        self.xor_key(block, 0);
        for round in 1..=rounds {
            // SubBytes and ShiftRows: byte `r` of column `c` comes from column `c + r`.
            let mut s = [0u8; 16];
            for (c, col) in s.chunks_exact_mut(4).enumerate() {
                for (r, d) in col.iter_mut().enumerate() {
                    *d = at(&SBOX, block.get(((c + r) % 4) * 4 + r).copied().unwrap_or(0));
                }
            }
            if round != rounds {
                for col in s.chunks_exact_mut(4) {
                    if let Ok(v) = <[u8; 4]>::try_from(&*col) {
                        col.copy_from_slice(&[
                            at(&MUL2, v[0]) ^ at(&MUL3, v[1]) ^ v[2] ^ v[3],
                            v[0] ^ at(&MUL2, v[1]) ^ at(&MUL3, v[2]) ^ v[3],
                            v[0] ^ v[1] ^ at(&MUL2, v[2]) ^ at(&MUL3, v[3]),
                            at(&MUL3, v[0]) ^ v[1] ^ v[2] ^ at(&MUL2, v[3]),
                        ]);
                    }
                }
            }
            *block = s;
            self.xor_key(block, round);
        }
    }

    /// Decrypts one block.
    pub fn decrypt_block(&self, block: &mut [u8; 16]) {
        let rounds = self.round_keys.len().saturating_sub(1);
        self.xor_key(block, rounds);
        for round in (0..rounds).rev() {
            // InvShiftRows and InvSubBytes.
            let mut s = [0u8; 16];
            for (c, col) in block.chunks_exact(4).enumerate() {
                for (r, &b) in col.iter().enumerate() {
                    if let Some(d) = s.get_mut(((c + r) % 4) * 4 + r) {
                        *d = at(&INV_SBOX, b);
                    }
                }
            }
            *block = s;
            self.xor_key(block, round);
            if round != 0 {
                for col in block.chunks_exact_mut(4) {
                    if let Ok(v) = <[u8; 4]>::try_from(&*col) {
                        col.copy_from_slice(&[
                            at(&MUL14, v[0])
                                ^ at(&MUL11, v[1])
                                ^ at(&MUL13, v[2])
                                ^ at(&MUL9, v[3]),
                            at(&MUL9, v[0])
                                ^ at(&MUL14, v[1])
                                ^ at(&MUL11, v[2])
                                ^ at(&MUL13, v[3]),
                            at(&MUL13, v[0])
                                ^ at(&MUL9, v[1])
                                ^ at(&MUL14, v[2])
                                ^ at(&MUL11, v[3]),
                            at(&MUL11, v[0])
                                ^ at(&MUL13, v[1])
                                ^ at(&MUL9, v[2])
                                ^ at(&MUL14, v[3]),
                        ]);
                    }
                }
            }
        }
    }

    /// CBC decryption of whole blocks. `None` if the IV is not 16 bytes or the data is not a
    /// multiple of the block size: a partial block is a damaged (or truncated, or padded-by-
    /// someone) ciphertext, and decrypting "what fits" would hand the caller a plaintext that
    /// no reader would produce.
    #[must_use]
    pub fn cbc_decrypt(&self, iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        let mut prev: [u8; 16] = iv.try_into().ok()?;
        if !data.len().is_multiple_of(16) {
            return None;
        }
        let mut out = Vec::with_capacity(data.len() / 16 * 16);
        for chunk in data.chunks_exact(16) {
            let mut block: [u8; 16] = chunk.try_into().ok()?;
            let cipher = block;
            self.decrypt_block(&mut block);
            for (b, p) in block.iter_mut().zip(prev) {
                *b ^= p;
            }
            out.extend_from_slice(&block);
            prev = cipher;
        }
        Some(out)
    }

    /// CBC encryption of whole blocks (the data length must be a multiple of 16).
    #[must_use]
    pub fn cbc_encrypt(&self, iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        let mut prev: [u8; 16] = iv.try_into().ok()?;
        if !data.len().is_multiple_of(16) {
            return None;
        }
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks_exact(16) {
            let mut block: [u8; 16] = chunk.try_into().ok()?;
            for (b, p) in block.iter_mut().zip(prev) {
                *b ^= p;
            }
            self.encrypt_block(&mut block);
            out.extend_from_slice(&block);
            prev = block;
        }
        Some(out)
    }

    /// ECB decryption of whole blocks.
    #[must_use]
    pub fn ecb_decrypt(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() / 16 * 16);
        for chunk in data.chunks_exact(16) {
            if let Ok(mut block) = <[u8; 16]>::try_from(chunk) {
                self.decrypt_block(&mut block);
                out.extend_from_slice(&block);
            }
        }
        out
    }
}

/// Removes PKCS#7 padding; `None` if it is not valid.
///
/// Every one of the (up to) 16 trailing bytes is examined whatever the padding byte says, and
/// all the ways padding can be wrong end in the same `None`: a caller that decrypts attacker-
/// chosen data and reports "bad padding" must not tell the attacker which check failed.
#[must_use]
pub fn unpad_pkcs7(mut data: Vec<u8>) -> Option<Vec<u8>> {
    let last = *data.last()?;
    let n = usize::from(last);
    let mut bad = u8::from(n == 0 || n > 16 || n > data.len());
    let tail = data.get(data.len().saturating_sub(16)..)?;
    for (i, &b) in tail.iter().rev().enumerate() {
        bad |= u8::from(i < n) & u8::from(b != last);
    }
    if bad != 0 {
        return None;
    }
    data.truncate(data.len() - n);
    Some(data)
}
