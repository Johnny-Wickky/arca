//! Strict, non-extracting ZIP reader (APPNOTE 6.3.x subset used by OPC and ODF).
//!
//! # Defences
//!
//! | attack                                   | defence |
//! |------------------------------------------|---------|
//! | Zip-Slip / path traversal                | [`name::validate_member_name`] rejects `..`, absolute, drive, backslash, control and bidi names |
//! | duplicate / case-colliding names         | rejected: two readers must never disagree on which member is "the" part |
//! | symbolic links                           | Unix symlink members are rejected |
//! | overlapping-entry bombs (Fifield 2019)   | local-header data ranges must be disjoint and lie before the central directory |
//! | declared-ratio bombs                     | `uncompressed / compressed` above [`ZipLimits::max_ratio`] is refused *before* inflating |
//! | lying size fields                        | inflation is capped at the declared size; any surplus or shortfall is an error |
//! | recursive bombs                          | every inflate is charged to a shared [`InflateBudget`] |
//! | local/central header confusion           | name, method and encryption flag must agree between both headers |
//! | central-directory smuggling / prefixes   | `cd_offset + cd_size` must end exactly at the end-of-central-directory record |
//! | a second, hidden central directory       | no other end-of-central-directory (or ZIP64) signature may follow the start of the chosen central directory: a decoy record at the end of the file with the real one inside an entry comment is refused |
//! | trailing bytes inside a deflate range    | the deflate stream must consume exactly the compressed size the headers declare |
//! | data descriptors that disagree           | CRC and sizes of the descriptor are read and compared with the central directory |
//! | ZIP64 markers in local headers           | the sizes in the local ZIP64 extra field are compared too |
//! | Unicode path extra fields                | a Unicode Path (0x7075) naming a member differently is refused |
//! | corrupted data                           | CRC-32 of every member is verified |

pub mod name;
pub mod writer;

use core::fmt;
use std::collections::BTreeMap;

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};

use crate::budget::{BudgetExceeded, InflateBudget};
use crate::bytes::{slice, to_usize, u16_le, u32_le, u64_le};
use name::{NameRejection, fold_name, validate_member_name};

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_ZIP64_EOCD: u32 = 0x0606_4b50;
const SIG_ZIP64_LOCATOR: u32 = 0x0706_4b50;
const SIG_DATA_DESCRIPTOR: u32 = 0x0807_4b50;

const EOCD_LEN: usize = 22;
const CENTRAL_LEN: usize = 46;
const LOCAL_LEN: usize = 30;
const ZIP64_LOCATOR_LEN: usize = 20;
const ZIP64_EOCD_MIN_LEN: usize = 56;

/// General purpose flag: entry is encrypted.
const FLAG_ENCRYPTED: u16 = 0x0001;
/// General purpose flag: sizes are in a trailing data descriptor.
const FLAG_DATA_DESCRIPTOR: u16 = 0x0008;
/// General purpose flag: strong encryption.
const FLAG_STRONG_ENCRYPTION: u16 = 0x0040;

/// Compression method "stored".
pub const METHOD_STORED: u16 = 0;
/// Compression method "deflate".
pub const METHOD_DEFLATE: u16 = 8;

/// Resource limits for archive parsing and member extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipLimits {
    /// Maximum number of central directory entries.
    pub max_entries: usize,
    /// Maximum decompressed size of a single member.
    pub max_entry_size: u64,
    /// Maximum `uncompressed / compressed` ratio of a member.
    pub max_ratio: u64,
    /// Members whose declared size is at most this many bytes are exempt from
    /// the ratio test (tiny, highly repetitive XML parts are legitimate).
    pub ratio_grace_bytes: u64,
    /// Maximum member name length in bytes.
    pub max_name_len: usize,
}

impl Default for ZipLimits {
    fn default() -> Self {
        Self {
            max_entries: 50_000,
            max_entry_size: 256 << 20,
            max_ratio: 100,
            ratio_grace_bytes: 1 << 20,
            max_name_len: 1024,
        }
    }
}

/// Fatal structural problems. Any of these means the archive is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipError {
    /// No end-of-central-directory record was found.
    NotZip,
    /// A structure extends past the end of the input.
    Truncated(&'static str),
    /// The end-of-central-directory record is inconsistent.
    InvalidEndOfCentralDirectory(&'static str),
    /// Spanned / multi-disk archives are not supported.
    MultiDisk,
    /// The central directory does not end exactly at the EOCD record.
    CentralDirectoryMismatch,
    /// The number of central directory entries exceeds the limit.
    TooManyEntries {
        /// Declared count.
        count: u64,
        /// Configured limit.
        limit: usize,
    },
    /// A central directory header is malformed.
    InvalidCentralHeader {
        /// Index of the entry.
        index: usize,
    },
    /// A member name was rejected.
    InvalidName {
        /// Display-safe preview of the raw name.
        preview: String,
        /// Reason for the rejection.
        reason: NameRejection,
    },
    /// Two members share the same (case-folded) name.
    DuplicateName(String),
    /// A member is a symbolic link.
    Symlink(String),
    /// A local header is missing or disagrees with the central directory.
    LocalHeaderMismatch(String),
    /// A member's data lies outside the archive body.
    EntryOutOfBounds(String),
    /// Two members' data ranges overlap.
    OverlappingEntries(String, String),
    /// A member is encrypted.
    Encrypted(String),
    /// A member uses a compression method other than stored/deflate.
    UnsupportedCompression {
        /// Member name.
        name: String,
        /// Method identifier.
        method: u16,
    },
    /// A member's declared size exceeds [`ZipLimits::max_entry_size`].
    EntryTooLarge {
        /// Member name.
        name: String,
        /// Declared uncompressed size.
        size: u64,
    },
    /// A member's compression ratio exceeds [`ZipLimits::max_ratio`].
    CompressionBomb {
        /// Member name.
        name: String,
        /// Declared uncompressed size.
        uncompressed: u64,
        /// Compressed size.
        compressed: u64,
    },
    /// The shared decompression budget is exhausted.
    Budget(BudgetExceeded),
    /// The member decompressed to a different size than declared.
    SizeMismatch(String),
    /// The member's CRC-32 does not match.
    CrcMismatch(String),
    /// The DEFLATE stream is corrupt.
    Inflate(String),
    /// More than one end-of-central-directory record could be taken for the real one.
    AmbiguousEndOfCentralDirectory,
    /// The DEFLATE stream of a member ends before (or runs past) its declared compressed size.
    CompressedSizeMismatch(String),
    /// An Info-ZIP Unicode Path extra field names a member differently from its header.
    UnicodePathMismatch(String),
}

impl fmt::Display for ZipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotZip => f.write_str("not a ZIP archive"),
            Self::Truncated(what) => write!(f, "truncated archive: {what}"),
            Self::InvalidEndOfCentralDirectory(why) => {
                write!(f, "invalid end of central directory: {why}")
            }
            Self::MultiDisk => f.write_str("multi-disk archives are not supported"),
            Self::CentralDirectoryMismatch => {
                f.write_str("central directory position is inconsistent")
            }
            Self::TooManyEntries { count, limit } => {
                write!(f, "{count} entries exceed the limit of {limit}")
            }
            Self::InvalidCentralHeader { index } => {
                write!(f, "malformed central directory header #{index}")
            }
            Self::InvalidName { preview, reason } => {
                write!(f, "rejected member name {preview:?}: {reason}")
            }
            Self::DuplicateName(n) => write!(f, "duplicate member name {n:?}"),
            Self::Symlink(n) => write!(f, "symbolic link member {n:?}"),
            Self::LocalHeaderMismatch(n) => {
                write!(f, "local header of {n:?} disagrees with central directory")
            }
            Self::EntryOutOfBounds(n) => write!(f, "data of {n:?} lies outside the archive body"),
            Self::OverlappingEntries(a, b) => write!(f, "members {a:?} and {b:?} overlap"),
            Self::Encrypted(n) => write!(f, "member {n:?} is encrypted"),
            Self::UnsupportedCompression { name, method } => {
                write!(f, "member {name:?} uses unsupported compression method {method}")
            }
            Self::EntryTooLarge { name, size } => {
                write!(f, "member {name:?} declares {size} bytes, above the limit")
            }
            Self::CompressionBomb { name, uncompressed, compressed } => write!(
                f,
                "member {name:?} expands {compressed} -> {uncompressed} bytes (compression bomb)"
            ),
            Self::Budget(e) => write!(f, "{e}"),
            Self::SizeMismatch(n) => {
                write!(f, "member {n:?} does not decompress to its declared size")
            }
            Self::CrcMismatch(n) => write!(f, "CRC-32 mismatch in member {n:?}"),
            Self::Inflate(n) => write!(f, "corrupt DEFLATE stream in member {n:?}"),
            Self::AmbiguousEndOfCentralDirectory => {
                f.write_str("more than one end-of-central-directory record is present")
            }
            Self::CompressedSizeMismatch(n) => {
                write!(f, "member {n:?}: DEFLATE stream does not fill its compressed size")
            }
            Self::UnicodePathMismatch(n) => {
                write!(f, "member {n:?}: Unicode path extra field names it differently")
            }
        }
    }
}

impl core::error::Error for ZipError {}

impl From<BudgetExceeded> for ZipError {
    fn from(e: BudgetExceeded) -> Self {
        Self::Budget(e)
    }
}

/// Non-fatal irregularities, reported to the threat X-ray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipAnomaly {
    /// Bytes before the first local header (self-extractor or polyglot).
    PrefixData {
        /// Length of the prefix.
        bytes: u64,
    },
    /// Bytes after the end-of-central-directory record.
    TrailingData {
        /// Length of the trailer.
        bytes: u64,
    },
    /// Bytes between members that no header accounts for.
    UnreferencedData {
        /// Total size of the gaps.
        bytes: u64,
    },
    /// The archive, or one of its members, carries a comment.
    ArchiveComment {
        /// Comment length.
        bytes: u64,
    },
    /// The archive uses ZIP64 records.
    Zip64,
    /// A member carries an "Info-ZIP Unicode Path" extra field that a
    /// different reader might prefer over the header name.
    UnicodePathExtra(String),
    /// A member is encrypted (it is listed but cannot be read).
    EncryptedMember(String),
    /// A member uses a compression method other than stored/deflate.
    UnusualCompression {
        /// Member name.
        name: String,
        /// Method identifier.
        method: u16,
    },
}

/// One validated archive member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    name: String,
    is_dir: bool,
    method: u16,
    flags: u16,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    local_header_offset: u64,
    data_offset: u64,
    /// Length of the data descriptor behind the data (`0` if the entry has none).
    descriptor_len: u64,
}

impl ZipEntry {
    /// Validated member name (relative, `/`-separated).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `true` for directory placeholders (name ends with `/`).
    #[must_use]
    pub const fn is_dir(&self) -> bool {
        self.is_dir
    }

    /// Compression method identifier.
    #[must_use]
    pub const fn method(&self) -> u16 {
        self.method
    }

    /// `true` if the member is encrypted.
    #[must_use]
    pub const fn is_encrypted(&self) -> bool {
        self.flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0
    }

    /// Declared decompressed size.
    #[must_use]
    pub const fn uncompressed_size(&self) -> u64 {
        self.uncompressed_size
    }

    /// Size of the stored data.
    #[must_use]
    pub const fn compressed_size(&self) -> u64 {
        self.compressed_size
    }

    /// Declared CRC-32.
    #[must_use]
    pub const fn crc32(&self) -> u32 {
        self.crc32
    }

    /// Lowercased final path component's extension, if any.
    #[must_use]
    pub fn extension(&self) -> Option<String> {
        let file = self.name.rsplit('/').next()?;
        let (_, ext) = file.rsplit_once('.')?;
        Some(ext.to_ascii_lowercase())
    }
}

/// A parsed, validated archive. Member data is only touched by [`ZipArchive::read`].
#[derive(Debug, Clone)]
pub struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
    index: BTreeMap<String, usize>,
    anomalies: Vec<ZipAnomaly>,
    limits: ZipLimits,
}

struct Eocd {
    entries: u64,
    cd_size: u64,
    cd_offset: u64,
    /// Offset at which the central directory must end (EOCD or ZIP64 EOCD record).
    cd_end_expected: u64,
    zip64: bool,
    /// Offset of the chosen end-of-central-directory record.
    pos: usize,
}

impl<'a> ZipArchive<'a> {
    /// Parses and validates the archive structure. No member is decompressed.
    ///
    /// # Errors
    /// Returns a [`ZipError`] for any structural violation (see module docs).
    pub fn parse(data: &'a [u8], limits: ZipLimits) -> Result<Self, ZipError> {
        let mut anomalies = Vec::new();
        let eocd = find_eocd(data, &mut anomalies)?;
        if eocd.zip64 {
            anomalies.push(ZipAnomaly::Zip64);
        }
        if eocd.entries > u64::try_from(limits.max_entries).unwrap_or(u64::MAX) {
            return Err(ZipError::TooManyEntries {
                count: eocd.entries,
                limit: limits.max_entries,
            });
        }
        let cd_end =
            eocd.cd_offset.checked_add(eocd.cd_size).ok_or(ZipError::CentralDirectoryMismatch)?;
        if cd_end != eocd.cd_end_expected {
            return Err(ZipError::CentralDirectoryMismatch);
        }
        let cd_start = to_usize(eocd.cd_offset).ok_or(ZipError::CentralDirectoryMismatch)?;
        let cd_end_usize = to_usize(cd_end).ok_or(ZipError::CentralDirectoryMismatch)?;
        check_single_eocd(data, cd_start, &eocd)?;

        let mut entries: Vec<ZipEntry> = Vec::new();
        let mut index = BTreeMap::new();
        let mut pos = cd_start;
        let mut i = 0usize;
        while pos < cd_end_usize {
            if u64::try_from(i).unwrap_or(u64::MAX) >= eocd.entries {
                // More headers than declared: a classic smuggling trick.
                return Err(ZipError::InvalidCentralHeader { index: i });
            }
            let (entry, next) =
                parse_central_header(data, pos, cd_end_usize, i, &limits, &mut anomalies)?;
            let key = fold_name(&entry.name);
            if index.insert(key, entries.len()).is_some() {
                return Err(ZipError::DuplicateName(entry.name));
            }
            entries.push(entry);
            pos = next;
            i = i.saturating_add(1);
        }
        if pos != cd_end_usize || u64::try_from(entries.len()).unwrap_or(u64::MAX) != eocd.entries {
            return Err(ZipError::CentralDirectoryMismatch);
        }

        for entry in &mut entries {
            verify_local_header(data, entry, eocd.cd_offset)?;
        }
        check_layout(&entries, eocd.cd_offset, &mut anomalies)?;

        Ok(Self { data, entries, index, anomalies, limits })
    }

    /// All members in central-directory order.
    #[must_use]
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Non-fatal irregularities found while parsing.
    #[must_use]
    pub fn anomalies(&self) -> &[ZipAnomaly] {
        &self.anomalies
    }

    /// Limits this archive was parsed with.
    #[must_use]
    pub const fn limits(&self) -> &ZipLimits {
        &self.limits
    }

    /// Looks a member up by name (ASCII case-insensitive, as OPC requires).
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&ZipEntry> {
        let key = fold_name(name.trim_start_matches('/'));
        self.index.get(&key).and_then(|&i| self.entries.get(i))
    }

    /// Decompresses a member, enforcing all limits and verifying its CRC-32.
    ///
    /// # Errors
    /// See [`ZipError`]; the budget is only charged on success paths.
    pub fn read(&self, entry: &ZipEntry, budget: &mut InflateBudget) -> Result<Vec<u8>, ZipError> {
        let name = || entry.name.clone();
        if entry.is_encrypted() {
            return Err(ZipError::Encrypted(name()));
        }
        if entry.is_dir {
            return Ok(Vec::new());
        }
        if entry.uncompressed_size > self.limits.max_entry_size {
            return Err(ZipError::EntryTooLarge { name: name(), size: entry.uncompressed_size });
        }
        if entry.uncompressed_size > self.limits.ratio_grace_bytes
            && entry.uncompressed_size
                > entry.compressed_size.max(1).saturating_mul(self.limits.max_ratio)
        {
            return Err(ZipError::CompressionBomb {
                name: name(),
                uncompressed: entry.uncompressed_size,
                compressed: entry.compressed_size,
            });
        }
        let start =
            to_usize(entry.data_offset).ok_or_else(|| ZipError::EntryOutOfBounds(name()))?;
        let len =
            to_usize(entry.compressed_size).ok_or_else(|| ZipError::EntryOutOfBounds(name()))?;
        let raw = slice(self.data, start, len).ok_or_else(|| ZipError::EntryOutOfBounds(name()))?;
        let expected = to_usize(entry.uncompressed_size).ok_or_else(|| {
            ZipError::EntryTooLarge { name: name(), size: entry.uncompressed_size }
        })?;

        budget.charge(entry.uncompressed_size, entry.compressed_size)?;

        let out = match entry.method {
            METHOD_STORED => {
                if raw.len() != expected {
                    return Err(ZipError::SizeMismatch(name()));
                }
                raw.to_vec()
            }
            METHOD_DEFLATE => inflate_exact(raw, expected).map_err(|e| match e {
                InflateFailure::Memory => {
                    ZipError::EntryTooLarge { name: name(), size: entry.uncompressed_size }
                }
                // The stream produces more (or fewer) bytes than the header admits.
                InflateFailure::Size => ZipError::SizeMismatch(name()),
                InflateFailure::Corrupt => ZipError::Inflate(name()),
                // Bytes inside the member's range that the stream never reads are data
                // another reader may interpret differently.
                InflateFailure::Trailing => ZipError::CompressedSizeMismatch(name()),
            })?,
            method => return Err(ZipError::UnsupportedCompression { name: name(), method }),
        };
        if out.len() != expected {
            return Err(ZipError::SizeMismatch(name()));
        }
        if arca_digest::crc32(&out) != entry.crc32 {
            return Err(ZipError::CrcMismatch(name()));
        }
        Ok(out)
    }

    /// Convenience: looks up and reads a member by name.
    ///
    /// # Errors
    /// See [`ZipArchive::read`]. Returns `Ok(None)` if the member is absent.
    pub fn read_by_name(
        &self,
        name: &str,
        budget: &mut InflateBudget,
    ) -> Result<Option<Vec<u8>>, ZipError> {
        match self.entry(name) {
            Some(e) => self.read(e, budget).map(Some),
            None => Ok(None),
        }
    }
}

/// Why [`inflate_exact`] failed.
enum InflateFailure {
    /// The output buffer could not be allocated.
    Memory,
    /// The stream does not decompress to exactly the declared size.
    Size,
    /// The stream is truncated or not valid DEFLATE.
    Corrupt,
    /// The stream ends before the end of the compressed range.
    Trailing,
}

/// Inflates a raw DEFLATE stream into a buffer of *exactly* `expected` bytes.
///
/// The declared size has already been validated against the entry, ratio and
/// archive budgets, so the whole output buffer is allocated once, up front.
/// This avoids the grow-and-copy cycle of an incrementally resized vector
/// (which doubles peak memory and, inside the interpreter, the fuel spent on
/// zero-filling and copying), and lets an over-long stream fail the moment it
/// writes its `expected + 1`-th byte.
fn inflate_exact(raw: &[u8], expected: usize) -> Result<Vec<u8>, InflateFailure> {
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(expected).map_err(|_| InflateFailure::Memory)?;
    out.resize(expected, 0);
    let mut state = Box::<DecompressorOxide>::default();
    let (status, consumed, written) = decompress(
        &mut state,
        raw,
        &mut out,
        0,
        inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF,
    );
    match status {
        TINFLStatus::Done if written == expected && consumed == raw.len() => Ok(out),
        TINFLStatus::Done if written == expected => Err(InflateFailure::Trailing),
        TINFLStatus::Done | TINFLStatus::HasMoreOutput => Err(InflateFailure::Size),
        _ => Err(InflateFailure::Corrupt),
    }
}

/// Returns `true` if `data` starts with a ZIP local header or EOCD signature.
#[must_use]
pub fn has_zip_signature(data: &[u8]) -> bool {
    matches!(u32_le(data, 0), Some(SIG_LOCAL | SIG_EOCD))
}

fn find_eocd(data: &[u8], anomalies: &mut Vec<ZipAnomaly>) -> Result<Eocd, ZipError> {
    if data.len() < EOCD_LEN {
        return Err(ZipError::NotZip);
    }
    let last = data.len() - EOCD_LEN;
    let first = last.saturating_sub(usize::from(u16::MAX));
    let mut found = None;
    for pos in (first..=last).rev() {
        if u32_le(data, pos) != Some(SIG_EOCD) {
            continue;
        }
        let Some(comment_len) = u16_le(data, pos + 20) else { continue };
        let end = pos.saturating_add(EOCD_LEN).saturating_add(usize::from(comment_len));
        if end <= data.len() {
            found = Some((pos, comment_len, data.len() - end));
            break;
        }
    }
    let (pos, comment_len, trailing) = found.ok_or(ZipError::NotZip)?;
    if trailing > 0 {
        anomalies.push(ZipAnomaly::TrailingData { bytes: trailing as u64 });
    }
    if comment_len > 0 {
        anomalies.push(ZipAnomaly::ArchiveComment { bytes: u64::from(comment_len) });
    }

    let field16 =
        |off: usize| u16_le(data, pos + off).ok_or(ZipError::Truncated("end of central directory"));
    let field32 =
        |off: usize| u32_le(data, pos + off).ok_or(ZipError::Truncated("end of central directory"));
    let disk = field16(4)?;
    let cd_disk = field16(6)?;
    let entries_disk = field16(8)?;
    let entries_total = field16(10)?;
    let cd_size = field32(12)?;
    let cd_offset = field32(16)?;

    let needs_zip64 = entries_total == u16::MAX || cd_size == u32::MAX || cd_offset == u32::MAX;
    if !needs_zip64 {
        if disk != 0 || cd_disk != 0 || entries_disk != entries_total {
            return Err(ZipError::MultiDisk);
        }
        return Ok(Eocd {
            entries: u64::from(entries_total),
            cd_size: u64::from(cd_size),
            cd_offset: u64::from(cd_offset),
            cd_end_expected: pos as u64,
            zip64: false,
            pos,
        });
    }

    let loc = pos.checked_sub(ZIP64_LOCATOR_LEN).ok_or(ZipError::Truncated("zip64 locator"))?;
    if u32_le(data, loc) != Some(SIG_ZIP64_LOCATOR) {
        return Err(ZipError::InvalidEndOfCentralDirectory("missing zip64 locator"));
    }
    let loc_disk = u32_le(data, loc + 4).ok_or(ZipError::Truncated("zip64 locator"))?;
    let rec_off = u64_le(data, loc + 8).ok_or(ZipError::Truncated("zip64 locator"))?;
    let disks = u32_le(data, loc + 16).ok_or(ZipError::Truncated("zip64 locator"))?;
    if loc_disk != 0 || disks > 1 {
        return Err(ZipError::MultiDisk);
    }
    let rec = to_usize(rec_off).ok_or(ZipError::Truncated("zip64 record"))?;
    if rec.checked_add(ZIP64_EOCD_MIN_LEN).is_none_or(|end| end > loc) {
        return Err(ZipError::InvalidEndOfCentralDirectory("zip64 record overlaps locator"));
    }
    if u32_le(data, rec) != Some(SIG_ZIP64_EOCD) {
        return Err(ZipError::InvalidEndOfCentralDirectory("bad zip64 record signature"));
    }
    let rec_size = u64_le(data, rec + 4).ok_or(ZipError::Truncated("zip64 record"))?;
    // The record must end exactly at the locator.
    if rec_off.checked_add(12).and_then(|v| v.checked_add(rec_size)) != Some(loc as u64) {
        return Err(ZipError::InvalidEndOfCentralDirectory("zip64 record size mismatch"));
    }
    let r32 = |off: usize| u32_le(data, rec + off).ok_or(ZipError::Truncated("zip64 record"));
    let r64 = |off: usize| u64_le(data, rec + off).ok_or(ZipError::Truncated("zip64 record"));
    if r32(16)? != 0 || r32(20)? != 0 {
        return Err(ZipError::MultiDisk);
    }
    let entries_disk64 = r64(24)?;
    let entries_total64 = r64(32)?;
    if entries_disk64 != entries_total64 {
        return Err(ZipError::MultiDisk);
    }
    Ok(Eocd {
        entries: entries_total64,
        cd_size: r64(40)?,
        cd_offset: r64(48)?,
        cd_end_expected: rec_off,
        zip64: true,
        pos,
    })
}

/// Refuses an archive in which another end-of-central-directory record could be the real one.
///
/// The chosen record is the last one in the file whose comment fits. A reader that scans
/// from the front, or that stops at a record inside a comment, would take a different
/// central directory; so would a reader that honours a ZIP64 record this one ignores.
/// Everything from the start of the central directory to the end of the file is therefore
/// searched for further record signatures (member data before the central directory is not:
/// a stored nested archive legitimately contains them). The ZIP64 record and locator that
/// belong to the chosen record are expected where its locator points.
fn check_single_eocd(data: &[u8], cd_start: usize, eocd: &Eocd) -> Result<(), ZipError> {
    let loc = eocd.pos.checked_sub(ZIP64_LOCATOR_LEN);
    let rec = loc
        .filter(|&l| u32_le(data, l) == Some(SIG_ZIP64_LOCATOR))
        .and_then(|l| u64_le(data, l + 8))
        .and_then(to_usize);
    let tail = data.get(cd_start..).ok_or(ZipError::CentralDirectoryMismatch)?;
    for (i, window) in tail.windows(4).enumerate() {
        let at = cd_start.saturating_add(i);
        let ambiguous = match u32_le(window, 0) {
            Some(SIG_EOCD) => at != eocd.pos && looks_like_eocd(data, at),
            Some(SIG_ZIP64_EOCD) => Some(at) != rec && u64_le(data, at + 4).is_some(),
            Some(SIG_ZIP64_LOCATOR) => Some(at) != loc && u32_le(data, at + 4) == Some(0),
            _ => false,
        };
        if ambiguous {
            return Err(ZipError::AmbiguousEndOfCentralDirectory);
        }
    }
    Ok(())
}

/// `true` if the bytes at `at` are shaped like an end-of-central-directory record: whole,
/// on disk 0, with a central directory on disk 0. (Four bytes that merely equal the
/// signature, inside a CRC or an offset, do not make a second archive.)
fn looks_like_eocd(data: &[u8], at: usize) -> bool {
    slice(data, at, EOCD_LEN).is_some()
        && u16_le(data, at + 4) == Some(0)
        && u16_le(data, at + 6) == Some(0)
}

/// The name an Info-ZIP Unicode Path extra field (0x7075) carries, if it is well formed and
/// belongs to `header_name` (its CRC-32 is the one of the raw header name).
fn unicode_path<'e>(body: &'e [u8], header_name: &[u8]) -> Option<&'e [u8]> {
    let (&version, rest) = body.split_first()?;
    let crc = u32_le(rest, 0)?;
    (version == 1 && crc == arca_digest::crc32(header_name)).then(|| rest.get(4..)).flatten()
}

fn parse_central_header(
    data: &[u8],
    pos: usize,
    cd_end: usize,
    index: usize,
    limits: &ZipLimits,
    anomalies: &mut Vec<ZipAnomaly>,
) -> Result<(ZipEntry, usize), ZipError> {
    let bad = || ZipError::InvalidCentralHeader { index };
    if u32_le(data, pos) != Some(SIG_CENTRAL) {
        return Err(bad());
    }
    let header = slice(data, pos, CENTRAL_LEN).ok_or(bad())?;
    let h16 = |off: usize| u16_le(header, off).ok_or(bad());
    let h32 = |off: usize| u32_le(header, off).ok_or(bad());

    let version_made_by = h16(4)?;
    let flags = h16(8)?;
    let method = h16(10)?;
    let crc32 = h32(16)?;
    let mut compressed = u64::from(h32(20)?);
    let mut uncompressed = u64::from(h32(24)?);
    let name_len = usize::from(h16(28)?);
    let extra_len = usize::from(h16(30)?);
    let comment_len = usize::from(h16(32)?);
    let disk_start = h16(34)?;
    let external_attrs = h32(38)?;
    let mut local_offset = u64::from(h32(42)?);

    let var_start = pos.checked_add(CENTRAL_LEN).ok_or(bad())?;
    let next = var_start
        .checked_add(name_len)
        .and_then(|v| v.checked_add(extra_len))
        .and_then(|v| v.checked_add(comment_len))
        .ok_or(bad())?;
    if next > cd_end {
        return Err(bad());
    }
    let raw_name = slice(data, var_start, name_len).ok_or(bad())?;
    let extra = slice(data, var_start + name_len, extra_len).ok_or(bad())?;
    if comment_len > 0 {
        anomalies.push(ZipAnomaly::ArchiveComment { bytes: comment_len as u64 });
    }

    let name = validate_member_name(raw_name, limits.max_name_len).map_err(|reason| {
        ZipError::InvalidName {
            preview: crate::text::display_safe(&String::from_utf8_lossy(raw_name), 80),
            reason,
        }
    })?;
    let name = name.to_owned();

    // ZIP64 extended information extra field.
    let mut disk = u32::from(disk_start);
    let mut has_unicode_path = false;
    let mut x = 0usize;
    while x.checked_add(4).is_some_and(|e| e <= extra.len()) {
        let id = u16_le(extra, x).ok_or(bad())?;
        let size = usize::from(u16_le(extra, x + 2).ok_or(bad())?);
        let body = slice(extra, x + 4, size).ok_or(bad())?;
        match id {
            0x0001 => {
                let mut r = crate::bytes::LeReader::new(body);
                if uncompressed == u64::from(u32::MAX) {
                    uncompressed = r.u64().ok_or(bad())?;
                }
                if compressed == u64::from(u32::MAX) {
                    compressed = r.u64().ok_or(bad())?;
                }
                if local_offset == u64::from(u32::MAX) {
                    local_offset = r.u64().ok_or(bad())?;
                }
                if disk_start == u16::MAX {
                    disk = r.u32().ok_or(bad())?;
                }
            }
            0x7075 => {
                has_unicode_path = true;
                // A name that is not the header name is a second name for the member.
                if unicode_path(body, raw_name).is_some_and(|u| u != raw_name) {
                    return Err(ZipError::UnicodePathMismatch(name));
                }
            }
            // Info-ZIP Unicode Comment: like the comment field, informational only.
            0x6375 => anomalies.push(ZipAnomaly::ArchiveComment { bytes: size as u64 }),
            _ => {}
        }
        x = x + 4 + size;
    }
    if disk != 0 {
        return Err(ZipError::MultiDisk);
    }
    if has_unicode_path {
        anomalies.push(ZipAnomaly::UnicodePathExtra(name.clone()));
    }

    // Unix host (3) with S_IFLNK file type.
    if version_made_by >> 8 == 3 && (external_attrs >> 16) & 0o170_000 == 0o120_000 {
        return Err(ZipError::Symlink(name));
    }

    let is_dir = name.ends_with('/');
    // A directory has no content; an empty DEFLATE stream is two bytes.
    if is_dir && (uncompressed != 0 || compressed > 2) {
        return Err(ZipError::LocalHeaderMismatch(name));
    }
    if flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0 {
        anomalies.push(ZipAnomaly::EncryptedMember(name.clone()));
    }
    if method != METHOD_STORED && method != METHOD_DEFLATE {
        anomalies.push(ZipAnomaly::UnusualCompression { name: name.clone(), method });
    }

    Ok((
        ZipEntry {
            name,
            is_dir,
            method,
            flags,
            crc32,
            compressed_size: compressed,
            uncompressed_size: uncompressed,
            local_header_offset: local_offset,
            data_offset: 0,
            descriptor_len: 0,
        },
        next,
    ))
}

fn verify_local_header(data: &[u8], entry: &mut ZipEntry, cd_offset: u64) -> Result<(), ZipError> {
    let mismatch = || ZipError::LocalHeaderMismatch(entry.name.clone());
    let pos = to_usize(entry.local_header_offset).ok_or_else(mismatch)?;
    if u32_le(data, pos) != Some(SIG_LOCAL) {
        return Err(mismatch());
    }
    let header = slice(data, pos, LOCAL_LEN).ok_or_else(mismatch)?;
    let flags = u16_le(header, 6).ok_or_else(mismatch)?;
    let method = u16_le(header, 8).ok_or_else(mismatch)?;
    let name_len = usize::from(u16_le(header, 26).ok_or_else(mismatch)?);
    let extra_len = usize::from(u16_le(header, 28).ok_or_else(mismatch)?);
    let name = slice(data, pos + LOCAL_LEN, name_len).ok_or_else(mismatch)?;
    let extra = slice(data, pos + LOCAL_LEN + name_len, extra_len).ok_or_else(mismatch)?;
    if name != entry.name.as_bytes() || method != entry.method {
        return Err(mismatch());
    }
    let enc_mask = FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION;
    if flags & enc_mask != entry.flags & enc_mask {
        return Err(mismatch());
    }

    // The local extra field: ZIP64 sizes (when the 32-bit fields are markers) and a Unicode
    // path that must not name the member differently from its header.
    let lcrc = u32_le(header, 14).ok_or_else(mismatch)?;
    let lcomp = u32_le(header, 18).ok_or_else(mismatch)?;
    let lun = u32_le(header, 22).ok_or_else(mismatch)?;
    let mut zip64_sizes: Option<(Option<u64>, Option<u64>)> = None;
    let mut x = 0usize;
    while x.checked_add(4).is_some_and(|e| e <= extra.len()) {
        let id = u16_le(extra, x).ok_or_else(mismatch)?;
        let size = usize::from(u16_le(extra, x + 2).ok_or_else(mismatch)?);
        let body = slice(extra, x + 4, size).ok_or_else(mismatch)?;
        match id {
            0x0001 => {
                let mut r = crate::bytes::LeReader::new(body);
                let un = (lun == u32::MAX).then(|| r.u64()).flatten();
                let comp = (lcomp == u32::MAX).then(|| r.u64()).flatten();
                zip64_sizes = Some((un, comp));
            }
            0x7075 if unicode_path(body, name).is_some_and(|u| u != name) => {
                return Err(ZipError::UnicodePathMismatch(entry.name.clone()));
            }
            _ => {}
        }
        x = x + 4 + size;
    }

    let local_agrees = {
        let size_agrees = |marker: bool, local: u32, declared: u64, from_extra: Option<u64>| {
            if marker { from_extra == Some(declared) } else { u64::from(local) == declared }
        };
        lcrc == entry.crc32
            && size_agrees(
                lcomp == u32::MAX,
                lcomp,
                entry.compressed_size,
                zip64_sizes.and_then(|z| z.1),
            )
            && size_agrees(
                lun == u32::MAX,
                lun,
                entry.uncompressed_size,
                zip64_sizes.and_then(|z| z.0),
            )
    };

    let data_offset = (pos as u64)
        .checked_add(LOCAL_LEN as u64)
        .and_then(|v| v.checked_add(name_len as u64))
        .and_then(|v| v.checked_add(extra_len as u64))
        .ok_or_else(mismatch)?;
    let data_end = data_offset
        .checked_add(entry.compressed_size)
        .ok_or_else(|| ZipError::EntryOutOfBounds(entry.name.clone()))?;
    if data_end > cd_offset {
        return Err(ZipError::EntryOutOfBounds(entry.name.clone()));
    }

    if flags & FLAG_DATA_DESCRIPTOR == 0 {
        // Without a data descriptor the local CRC and sizes must agree too.
        if !local_agrees {
            return Err(mismatch());
        }
    } else {
        // The sizes follow the data. Read them and hold them against the central directory;
        // a writer that also filled the local header may omit the descriptor.
        match data_descriptor_len(data, data_end, cd_offset, entry) {
            Some(len) => entry.descriptor_len = len,
            None if local_agrees => {}
            None => return Err(mismatch()),
        }
    }
    entry.data_offset = data_offset;
    Ok(())
}

/// Length of the data descriptor at `at` if one that matches the central directory is there.
///
/// The record is `[signature] crc-32, compressed size, uncompressed size`, with 4-byte
/// sizes, or 8-byte sizes in a ZIP64 archive; writers differ in the signature and the width.
fn data_descriptor_len(data: &[u8], at: u64, limit: u64, entry: &ZipEntry) -> Option<u64> {
    let at = to_usize(at)?;
    let matches_at = |base: usize| -> Option<u64> {
        if u32_le(data, base)? != entry.crc32 {
            return None;
        }
        let sizes = base.checked_add(4)?;
        let short = u64::from(u32_le(data, sizes)?) == entry.compressed_size
            && u64::from(u32_le(data, sizes.checked_add(4)?)?) == entry.uncompressed_size;
        if short {
            return Some(12);
        }
        let wide = u64_le(data, sizes)? == entry.compressed_size
            && u64_le(data, sizes.checked_add(8)?)? == entry.uncompressed_size;
        wide.then_some(20)
    };
    let (sig_len, record_len) = if u32_le(data, at) == Some(SIG_DATA_DESCRIPTOR) {
        (4usize, matches_at(at.checked_add(4)?)?)
    } else {
        (0, matches_at(at)?)
    };
    let len = record_len.checked_add(sig_len as u64)?;
    // The record must lie before the central directory.
    (at as u64).checked_add(len).filter(|&end| end <= limit).map(|_| len)
}

fn check_layout(
    entries: &[ZipEntry],
    cd_offset: u64,
    anomalies: &mut Vec<ZipAnomaly>,
) -> Result<(), ZipError> {
    let mut spans: Vec<(u64, u64, &str)> = entries
        .iter()
        .map(|e| {
            (
                e.local_header_offset,
                e.data_offset.saturating_add(e.compressed_size).saturating_add(e.descriptor_len),
                e.name.as_str(),
            )
        })
        .collect();
    spans.sort_unstable_by_key(|s| s.0);

    let mut cursor = 0u64;
    let mut gaps = 0u64;
    let mut prev_name: Option<&str> = None;
    for (i, &(start, end, name)) in spans.iter().enumerate() {
        if start < cursor {
            let other = prev_name.unwrap_or("").to_owned();
            return Err(ZipError::OverlappingEntries(other, name.to_owned()));
        }
        if i == 0 {
            if start > 0 {
                anomalies.push(ZipAnomaly::PrefixData { bytes: start });
            }
        } else {
            // A data descriptor is part of its member's span; anything else is a gap.
            gaps = gaps.saturating_add(start - cursor);
        }
        cursor = end;
        prev_name = Some(name);
    }
    if !spans.is_empty() && cd_offset > cursor {
        gaps = gaps.saturating_add(cd_offset - cursor);
    }
    if gaps > 0 {
        anomalies.push(ZipAnomaly::UnreferencedData { bytes: gaps });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
