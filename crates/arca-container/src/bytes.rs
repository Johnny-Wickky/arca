//! Bounds-checked little-endian primitives.
//!
//! Every binary structure in this repository is decoded through these
//! helpers. They never panic: out-of-range reads yield `None`, which callers
//! convert into a typed "truncated" error.

/// Reads a `u16` (little-endian) at `offset`.
#[must_use]
pub fn u16_le(data: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let raw: [u8; 2] = data.get(offset..end)?.try_into().ok()?;
    Some(u16::from_le_bytes(raw))
}

/// Reads a `u32` (little-endian) at `offset`.
#[must_use]
pub fn u32_le(data: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let raw: [u8; 4] = data.get(offset..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

/// Reads a `u64` (little-endian) at `offset`.
#[must_use]
pub fn u64_le(data: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let raw: [u8; 8] = data.get(offset..end)?.try_into().ok()?;
    Some(u64::from_le_bytes(raw))
}

/// Returns `len` bytes starting at `offset`.
#[must_use]
pub fn slice(data: &[u8], offset: usize, len: usize) -> Option<&[u8]> {
    data.get(offset..offset.checked_add(len)?)
}

/// Converts a `u64` file quantity to `usize`, failing on 32-bit overflow.
#[must_use]
pub fn to_usize(v: u64) -> Option<usize> {
    usize::try_from(v).ok()
}

/// Sequential little-endian reader over a byte slice.
///
/// All methods return `None` when the requested bytes are not available; the
/// position is only advanced on success.
#[derive(Debug, Clone)]
pub struct LeReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> LeReader<'a> {
    /// Creates a reader positioned at the start of `data`.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Creates a reader positioned at `pos`.
    #[must_use]
    pub const fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    /// Current position.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Bytes remaining after the current position.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// `true` when no bytes remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Reads one byte.
    pub fn u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos = self.pos.checked_add(1)?;
        Some(b)
    }

    /// Reads a little-endian `u16`.
    pub fn u16(&mut self) -> Option<u16> {
        let v = u16_le(self.data, self.pos)?;
        self.pos = self.pos.checked_add(2)?;
        Some(v)
    }

    /// Reads a little-endian `u32`.
    pub fn u32(&mut self) -> Option<u32> {
        let v = u32_le(self.data, self.pos)?;
        self.pos = self.pos.checked_add(4)?;
        Some(v)
    }

    /// Reads a little-endian `u64`.
    pub fn u64(&mut self) -> Option<u64> {
        let v = u64_le(self.data, self.pos)?;
        self.pos = self.pos.checked_add(8)?;
        Some(v)
    }

    /// Reads `len` raw bytes.
    pub fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let s = slice(self.data, self.pos, len)?;
        self.pos = self.pos.checked_add(len)?;
        Some(s)
    }

    /// Skips `len` bytes.
    pub fn skip(&mut self, len: usize) -> Option<()> {
        self.bytes(len).map(|_| ())
    }

    /// Returns the unread remainder without advancing.
    #[must_use]
    pub fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_are_bounds_checked() {
        let d = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(u16_le(&d, 0), Some(0x0201));
        assert_eq!(u32_le(&d, 4), Some(0x0807_0605));
        assert_eq!(u64_le(&d, 0), Some(0x0807_0605_0403_0201));
        assert_eq!(u16_le(&d, 7), None);
        assert_eq!(u32_le(&d, usize::MAX - 1), None);
        assert_eq!(slice(&d, 6, 3), None);
    }

    #[test]
    fn reader_does_not_advance_on_failure() {
        let d = [1u8, 2, 3];
        let mut r = LeReader::new(&d);
        assert_eq!(r.u16(), Some(0x0201));
        assert_eq!(r.u16(), None);
        assert_eq!(r.position(), 2);
        assert_eq!(r.u8(), Some(3));
        assert!(r.is_empty());
    }
}
