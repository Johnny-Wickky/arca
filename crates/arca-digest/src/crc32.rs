//! CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`), as used by ZIP.

/// Lookup table generated at compile time.
const TABLE: [u32; 256] = build_table();

#[allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "const context: the loop bound `i < 256` makes both the cast and the index provably valid"
)]
const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        // `i < 256`, the conversion is lossless.
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// Incremental CRC-32 hasher.
#[derive(Debug, Clone, Copy)]
pub struct Crc32 {
    state: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// Creates a hasher in its initial state.
    #[must_use]
    pub const fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    /// Feeds `data` into the hasher.
    pub fn update(&mut self, data: &[u8]) {
        let mut c = self.state;
        for &byte in data {
            let low = c.to_le_bytes()[0];
            let idx = usize::from(low ^ byte);
            // `idx` is a `u8` widened to usize, therefore always < 256.
            let entry = TABLE.get(idx).copied().unwrap_or(0);
            c = entry ^ (c >> 8);
        }
        self.state = c;
    }

    /// Returns the final checksum.
    #[must_use]
    pub const fn finalize(self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }
}

/// One-shot CRC-32 of `data`.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut h = Crc32::new();
    h.update(data);
    h.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    #[test]
    fn incremental_equals_one_shot() {
        let data = b"Arca incremental crc check";
        let mut h = Crc32::new();
        for chunk in data.chunks(5) {
            h.update(chunk);
        }
        assert_eq!(h.finalize(), crc32(data));
    }
}
