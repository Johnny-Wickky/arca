//! A small, bounded, self-describing-free binary codec.
//!
//! Layout: little-endian fixed-width integers, `bool` as one byte (0/1),
//! strings and sequences as a `u32` length followed by the items, options as
//! a tag byte. The decoder treats its input as hostile (it comes out of a
//! sandbox that may have been compromised): every length is checked against
//! the remaining input *and* against [`DecodeLimits`] before any allocation,
//! strings must be valid UTF-8, unknown enum tags are errors and nesting depth
//! is bounded. A sequence of small wire items can decode into much larger values
//! (a one-byte enum variant into a struct of dozens of bytes), so the decoder also keeps
//! a total budget of the bytes it allocates: every sequence is charged `count * size_of`
//! of its element, every string and byte string its length.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// Decoding failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// Input ended prematurely.
    UnexpectedEnd,
    /// A length exceeds the remaining input or a configured limit.
    LengthLimit,
    /// A string is not valid UTF-8.
    InvalidUtf8,
    /// An enum or option tag is unknown.
    InvalidTag(u16),
    /// A boolean byte is neither 0 nor 1.
    InvalidBool,
    /// Nesting is deeper than allowed.
    DepthLimit,
    /// Bytes remain after the top-level value.
    TrailingBytes,
    /// The encoder was asked to write a value too large for the format.
    EncodeOverflow,
    /// Decoding the input would allocate more than the decoder's total budget.
    TotalLimit,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("unexpected end of input"),
            Self::LengthLimit => f.write_str("length exceeds limit"),
            Self::InvalidUtf8 => f.write_str("invalid UTF-8"),
            Self::InvalidTag(t) => write!(f, "invalid tag {t}"),
            Self::InvalidBool => f.write_str("invalid boolean"),
            Self::DepthLimit => f.write_str("nesting too deep"),
            Self::TrailingBytes => f.write_str("trailing bytes"),
            Self::EncodeOverflow => f.write_str("value too large to encode"),
            Self::TotalLimit => f.write_str("decoded size exceeds the total budget"),
        }
    }
}

impl core::error::Error for WireError {}

/// Bounds applied by the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Maximum length of one string in bytes.
    pub max_string_bytes: usize,
    /// Maximum number of items in one sequence.
    pub max_items: usize,
    /// Maximum nesting depth of composite values.
    pub max_depth: usize,
}

/// The decoder's default total allocation budget (see [`Reader::with_total_budget`]).
pub const DEFAULT_TOTAL_BYTES: usize = 256 << 20;

impl Default for DecodeLimits {
    fn default() -> Self {
        Self { max_string_bytes: 16 << 20, max_items: 1 << 20, max_depth: 16 }
    }
}

/// Encoder.
#[derive(Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
    overflow: bool,
}

impl Writer {
    /// Creates an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes one byte.
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    /// Writes a `u16`.
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Writes a `u32`.
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Writes a `u64`.
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Writes an `i64`.
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Writes a boolean.
    pub fn bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }
    /// Writes a length prefix.
    pub fn len(&mut self, n: usize) {
        match u32::try_from(n) {
            Ok(v) => self.u32(v),
            Err(_) => {
                self.overflow = true;
                self.u32(0);
            }
        }
    }
    /// Writes a string.
    pub fn str(&mut self, s: &str) {
        self.len(s.len());
        self.buf.extend_from_slice(s.as_bytes());
    }
    /// Writes a byte string.
    pub fn bytes(&mut self, b: &[u8]) {
        self.len(b.len());
        self.buf.extend_from_slice(b);
    }
    /// Writes any [`Wire`] value.
    pub fn put<T: Wire>(&mut self, v: &T) {
        v.encode(self);
    }
    /// Finishes encoding.
    ///
    /// # Errors
    /// [`WireError::EncodeOverflow`] if any length did not fit in `u32`.
    pub fn finish(self) -> Result<Vec<u8>, WireError> {
        if self.overflow { Err(WireError::EncodeOverflow) } else { Ok(self.buf) }
    }
}

/// Decoder.
#[derive(Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    depth: usize,
    limits: DecodeLimits,
    /// Bytes the decoded values have been charged so far, and the most they may be charged.
    spent: usize,
    budget: usize,
}

impl<'a> Reader<'a> {
    /// Creates a decoder with the given limits and a total budget of
    /// [`DEFAULT_TOTAL_BYTES`] (or `max_string_bytes`, if larger: a payload that is one
    /// big byte string must fit).
    #[must_use]
    pub const fn new(data: &'a [u8], limits: DecodeLimits) -> Self {
        let budget = if limits.max_string_bytes > DEFAULT_TOTAL_BYTES {
            limits.max_string_bytes
        } else {
            DEFAULT_TOTAL_BYTES
        };
        Self { data, pos: 0, depth: 0, limits, spent: 0, budget }
    }

    /// Replaces the total budget of bytes the decoded value may occupy.
    #[must_use]
    pub const fn with_total_budget(mut self, bytes: usize) -> Self {
        self.budget = bytes;
        self
    }

    /// Charges `bytes` of memory to the total budget; call it before allocating them.
    ///
    /// # Errors
    /// [`WireError::TotalLimit`] if the budget would be exceeded (nothing is charged then).
    pub fn charge(&mut self, bytes: usize) -> Result<(), WireError> {
        let total = self.spent.checked_add(bytes).ok_or(WireError::TotalLimit)?;
        if total > self.budget {
            return Err(WireError::TotalLimit);
        }
        self.spent = total;
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(n).ok_or(WireError::LengthLimit)?;
        let s = self.data.get(self.pos..end).ok_or(WireError::UnexpectedEnd)?;
        self.pos = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        self.take(N)?.try_into().map_err(|_| WireError::UnexpectedEnd)
    }

    /// Reads one byte.
    ///
    /// # Errors
    /// [`WireError::UnexpectedEnd`].
    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.array::<1>()?[0])
    }
    /// Reads a `u16`.
    ///
    /// # Errors
    /// [`WireError::UnexpectedEnd`].
    pub fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    /// Reads a `u32`.
    ///
    /// # Errors
    /// [`WireError::UnexpectedEnd`].
    pub fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    /// Reads a `u64`.
    ///
    /// # Errors
    /// [`WireError::UnexpectedEnd`].
    pub fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    /// Reads an `i64`.
    ///
    /// # Errors
    /// [`WireError::UnexpectedEnd`].
    pub fn i64(&mut self) -> Result<i64, WireError> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    /// Reads a boolean.
    ///
    /// # Errors
    /// [`WireError::InvalidBool`] for bytes other than 0 and 1.
    pub fn bool(&mut self) -> Result<bool, WireError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::InvalidBool),
        }
    }
    /// Reads a sequence length, bounded by `max_items` and the remaining input
    /// (every item occupies at least one byte).
    ///
    /// # Errors
    /// [`WireError::LengthLimit`].
    pub fn seq_len(&mut self) -> Result<usize, WireError> {
        let n = usize::try_from(self.u32()?).map_err(|_| WireError::LengthLimit)?;
        if n > self.limits.max_items || n > self.remaining() {
            return Err(WireError::LengthLimit);
        }
        Ok(n)
    }
    /// Reads a string.
    ///
    /// # Errors
    /// Length or UTF-8 violations.
    pub fn str(&mut self) -> Result<String, WireError> {
        let n = usize::try_from(self.u32()?).map_err(|_| WireError::LengthLimit)?;
        if n > self.limits.max_string_bytes {
            return Err(WireError::LengthLimit);
        }
        let raw = self.take(n)?;
        let text = core::str::from_utf8(raw).map_err(|_| WireError::InvalidUtf8)?;
        self.charge(n)?;
        Ok(String::from(text))
    }
    /// Reads a byte string.
    ///
    /// # Errors
    /// Length violations.
    pub fn bytes(&mut self) -> Result<Vec<u8>, WireError> {
        let n = usize::try_from(self.u32()?).map_err(|_| WireError::LengthLimit)?;
        if n > self.limits.max_string_bytes {
            return Err(WireError::LengthLimit);
        }
        let raw = self.take(n)?;
        self.charge(n)?;
        Ok(raw.to_vec())
    }
    /// Reads any [`Wire`] value.
    ///
    /// # Errors
    /// Propagates the value's decoding errors.
    pub fn get<T: Wire>(&mut self) -> Result<T, WireError> {
        T::decode(self)
    }
    /// Enters a nested composite value.
    ///
    /// # Errors
    /// [`WireError::DepthLimit`].
    pub fn enter(&mut self) -> Result<(), WireError> {
        self.depth = self.depth.checked_add(1).ok_or(WireError::DepthLimit)?;
        if self.depth > self.limits.max_depth {
            return Err(WireError::DepthLimit);
        }
        Ok(())
    }
    /// Leaves a nested composite value.
    pub fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
    /// Bytes not yet consumed.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    /// Ensures the whole input was consumed.
    ///
    /// # Errors
    /// [`WireError::TrailingBytes`].
    pub fn finish(&self) -> Result<(), WireError> {
        if self.remaining() == 0 { Ok(()) } else { Err(WireError::TrailingBytes) }
    }
}

/// Types that can cross the sandbox boundary.
pub trait Wire: Sized {
    /// Encodes `self`.
    fn encode(&self, w: &mut Writer);
    /// Decodes a value.
    ///
    /// # Errors
    /// Any [`WireError`].
    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError>;
}

macro_rules! wire_int {
    ($($t:ident),*) => {$(
        impl Wire for $t {
            fn encode(&self, w: &mut Writer) { w.$t(*self); }
            fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> { r.$t() }
        }
    )*};
}
wire_int!(u8, u16, u32, u64, i64);

/// A single-precision number crosses as its bit pattern.
impl Wire for f32 {
    fn encode(&self, w: &mut Writer) {
        w.u32(self.to_bits());
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        r.u32().map(f32::from_bits)
    }
}

impl Wire for bool {
    fn encode(&self, w: &mut Writer) {
        w.bool(*self);
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        r.bool()
    }
}

impl Wire for String {
    fn encode(&self, w: &mut Writer) {
        w.str(self);
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        r.str()
    }
}

/// Items a sequence reserves before any of them has been read.
const FIRST_CHUNK_ITEMS: usize = 1 << 16;

impl<T: Wire> Wire for Vec<T> {
    fn encode(&self, w: &mut Writer) {
        w.len(self.len());
        for item in self {
            item.encode(w);
        }
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        r.enter()?;
        let n = r.seq_len()?;
        // `n` is bounded by the remaining input, but an item of one wire byte can be a value
        // of many bytes in memory: the sequence pays for what it will occupy, up front, and
        // reserves only a first chunk (the rest grows as items really arrive).
        r.charge(n.checked_mul(size_of::<T>()).ok_or(WireError::TotalLimit)?)?;
        let mut out = Vec::new();
        out.try_reserve(n.min(FIRST_CHUNK_ITEMS)).map_err(|_| WireError::LengthLimit)?;
        for _ in 0..n {
            out.push(T::decode(r)?);
        }
        r.leave();
        Ok(out)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn encode(&self, w: &mut Writer) {
        match self {
            None => w.u8(0),
            Some(v) => {
                w.u8(1);
                v.encode(w);
            }
        }
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        match r.u8()? {
            0 => Ok(None),
            1 => Ok(Some(T::decode(r)?)),
            t => Err(WireError::InvalidTag(u16::from(t))),
        }
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn encode(&self, w: &mut Writer) {
        self.0.encode(w);
        self.1.encode(w);
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok((A::decode(r)?, B::decode(r)?))
    }
}

/// Encodes a value to bytes.
///
/// # Errors
/// [`WireError::EncodeOverflow`].
pub fn to_bytes<T: Wire>(v: &T) -> Result<Vec<u8>, WireError> {
    let mut w = Writer::new();
    v.encode(&mut w);
    w.finish()
}

/// Decodes a complete value from bytes.
///
/// # Errors
/// Any [`WireError`], including trailing bytes.
pub fn from_bytes<T: Wire>(data: &[u8], limits: DecodeLimits) -> Result<T, WireError> {
    let mut r = Reader::new(data, limits);
    let v = T::decode(&mut r)?;
    r.finish()?;
    Ok(v)
}

/// Implements [`Wire`] for a fieldless enum with explicit `u8` discriminants.
#[macro_export]
macro_rules! wire_enum {
    ($ty:ty { $($variant:ident = $tag:literal),* $(,)? }) => {
        impl $crate::wire::Wire for $ty {
            fn encode(&self, w: &mut $crate::wire::Writer) {
                w.u8(match self { $(Self::$variant => $tag,)* });
            }
            fn decode(r: &mut $crate::wire::Reader<'_>) -> Result<Self, $crate::wire::WireError> {
                match r.u8()? {
                    $($tag => Ok(Self::$variant),)*
                    t => Err($crate::wire::WireError::InvalidTag(u16::from(t))),
                }
            }
        }
    };
}

/// Implements [`Wire`] for a struct by encoding the listed fields in order.
#[macro_export]
macro_rules! wire_struct {
    ($ty:ident { $($field:ident),* $(,)? }) => {
        impl $crate::wire::Wire for $ty {
            fn encode(&self, w: &mut $crate::wire::Writer) {
                $( $crate::wire::Wire::encode(&self.$field, w); )*
            }
            fn decode(r: &mut $crate::wire::Reader<'_>) -> Result<Self, $crate::wire::WireError> {
                r.enter()?;
                let v = Self { $( $field: $crate::wire::Wire::decode(r)?, )* };
                r.leave();
                Ok(v)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum Color {
        Red,
        Green,
    }
    crate::wire_enum!(Color { Red = 0, Green = 1 });

    #[derive(Debug, PartialEq, Eq)]
    struct Item {
        name: String,
        tags: Vec<String>,
        color: Color,
        size: Option<u64>,
        flag: bool,
    }
    crate::wire_struct!(Item { name, tags, color, size, flag });

    fn sample() -> Vec<Item> {
        vec![
            Item {
                name: "Привет".into(),
                tags: vec!["a".into(), "".into()],
                color: Color::Green,
                size: Some(7),
                flag: true,
            },
            Item {
                name: String::new(),
                tags: Vec::new(),
                color: Color::Red,
                size: None,
                flag: false,
            },
        ]
    }

    #[test]
    fn roundtrip() {
        let v = sample();
        let bytes = to_bytes(&v).unwrap();
        let back: Vec<Item> = from_bytes(&bytes, DecodeLimits::default()).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn every_truncation_fails_cleanly() {
        let bytes = to_bytes(&sample()).unwrap();
        for cut in 0..bytes.len() {
            assert!(from_bytes::<Vec<Item>>(&bytes[..cut], DecodeLimits::default()).is_err());
        }
    }

    #[test]
    fn hostile_lengths_do_not_allocate() {
        // A sequence claiming 4 billion items in an 8-byte input.
        let evil = [0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0];
        assert_eq!(
            from_bytes::<Vec<u64>>(&evil, DecodeLimits::default()),
            Err(WireError::LengthLimit)
        );
        let evil_str = [0xFF, 0xFF, 0xFF, 0x7F];
        assert_eq!(
            from_bytes::<String>(&evil_str, DecodeLimits::default()),
            Err(WireError::LengthLimit)
        );
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert_eq!(from_bytes::<bool>(&[2], DecodeLimits::default()), Err(WireError::InvalidBool));
        assert_eq!(
            from_bytes::<Color>(&[9], DecodeLimits::default()),
            Err(WireError::InvalidTag(9))
        );
        assert_eq!(
            from_bytes::<String>(&[2, 0, 0, 0, 0xC3, 0x28], DecodeLimits::default()),
            Err(WireError::InvalidUtf8)
        );
        assert_eq!(
            from_bytes::<u8>(&[1, 2], DecodeLimits::default()),
            Err(WireError::TrailingBytes)
        );
    }

    /// One byte on the wire, 72 bytes in memory: the amplification the budget exists for.
    #[derive(Debug, PartialEq, Eq)]
    struct Fat {
        tag: u8,
        _pad: [u64; 8],
    }
    impl Wire for Fat {
        fn encode(&self, w: &mut Writer) {
            w.u8(self.tag);
        }
        fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
            Ok(Self { tag: r.u8()?, _pad: [0; 8] })
        }
    }

    #[test]
    fn a_sequence_of_small_items_cannot_amplify_past_the_total_budget() {
        // 8 Mi items declared and present (one byte each): 72 bytes apiece is 576 MiB.
        let mut evil = alloc::vec![0u8; 4 + (8 << 20)];
        evil[..4].copy_from_slice(&(8u32 << 20).to_le_bytes());
        let limits = DecodeLimits { max_items: 1 << 24, ..DecodeLimits::default() };
        assert_eq!(from_bytes::<Vec<Fat>>(&evil, limits), Err(WireError::TotalLimit));
    }

    #[test]
    fn the_total_budget_can_be_set_and_charges_strings_too() {
        let fats = to_bytes(&(0..1000).map(|_| 1u8).collect::<Vec<u8>>()).unwrap();
        // 1000 items * 72 bytes = 72_000.
        let decode = |budget: usize| {
            let mut r = Reader::new(&fats, DecodeLimits::default()).with_total_budget(budget);
            Vec::<Fat>::decode(&mut r)
        };
        assert!(decode(72_000).is_ok());
        assert_eq!(decode(71_999), Err(WireError::TotalLimit));
        // Strings are charged by length, across the whole value.
        let strings = to_bytes(&alloc::vec![String::from("0123456789"); 100]).unwrap();
        let header = 24 * 100;
        let mut r = Reader::new(&strings, DecodeLimits::default()).with_total_budget(header + 999);
        assert_eq!(Vec::<String>::decode(&mut r), Err(WireError::TotalLimit));
        let mut r = Reader::new(&strings, DecodeLimits::default()).with_total_budget(header + 1000);
        assert_eq!(Vec::<String>::decode(&mut r).unwrap().len(), 100);
        // A failed charge changes nothing.
        let mut r = Reader::new(&[], DecodeLimits::default()).with_total_budget(10);
        assert_eq!(r.charge(11), Err(WireError::TotalLimit));
        assert_eq!(r.charge(10), Ok(()));
        assert_eq!(r.charge(1), Err(WireError::TotalLimit));
    }

    #[test]
    fn nested_sequences_pay_for_every_header() {
        // 20_000 empty inner vectors cost 4 wire bytes each but 24 bytes of header in memory.
        let nested: Vec<Vec<u8>> = (0..20_000).map(|_| Vec::new()).collect();
        let bytes = to_bytes(&nested).unwrap();
        let header = size_of::<Vec<u8>>();
        let mut r =
            Reader::new(&bytes, DecodeLimits::default()).with_total_budget(20_000 * header - 1);
        assert_eq!(Vec::<Vec<u8>>::decode(&mut r), Err(WireError::TotalLimit));
        let mut r = Reader::new(&bytes, DecodeLimits::default()).with_total_budget(20_000 * header);
        assert_eq!(Vec::<Vec<u8>>::decode(&mut r).unwrap().len(), 20_000);
    }

    #[test]
    fn depth_limit() {
        let nested: Vec<Vec<Vec<u8>>> = vec![vec![vec![1]]];
        let bytes = to_bytes(&nested).unwrap();
        let limits = DecodeLimits { max_depth: 2, ..DecodeLimits::default() };
        assert_eq!(from_bytes::<Vec<Vec<Vec<u8>>>>(&bytes, limits), Err(WireError::DepthLimit));
        assert!(from_bytes::<Vec<Vec<Vec<u8>>>>(&bytes, DecodeLimits::default()).is_ok());
    }
}
