//! A bounds-checked cursor over a byte slice: every read answers `None` past the end.

/// Reads integers and slices from `data` without ever indexing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) const fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub(crate) const fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub(crate) fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or(&[])
    }

    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    pub(crate) fn u16_be(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_be_bytes([*s.first()?, *s.get(1)?]))
    }

    pub(crate) fn u16_le(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
    }

    pub(crate) fn u32_be(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_be_bytes([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?]))
    }

    pub(crate) fn u32_le(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?]))
    }

    pub(crate) fn i32_le(&mut self) -> Option<i32> {
        self.u32_le().map(|v| i32::from_le_bytes(v.to_le_bytes()))
    }
}

/// `usize` from a `u32` (always lossless on the 32- and 64-bit targets we build for).
pub(crate) fn to_usize(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}
