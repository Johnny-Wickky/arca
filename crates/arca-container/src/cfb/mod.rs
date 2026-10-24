//! Hardened reader for the OLE2 Compound File Binary format ([MS-CFB]).
//!
//! OLE2 is the container of `.doc`, `.xls`, `.ppt`, `.vsd`, `.msg`, of
//! `vbaProject.bin` and of embedded OLE objects. Historically it is the most
//! exploited container in office software (sector-chain loops, overlapping
//! FAT/directory sectors, directory-tree cycles, huge declared sizes).
//!
//! # Defences
//!
//! * the header is validated field by field (version ↔ sector size, mini
//!   sector size, mini stream cutoff, byte order);
//! * every sector index is range-checked against the actual file length;
//! * every FAT / mini-FAT / DIFAT chain walk carries a visited set, so cycles
//!   are detected instead of looping forever;
//! * the directory red-black tree is walked iteratively with a visited set
//!   and a depth limit; unreachable ("orphaned") entries are reported because
//!   they are a known hiding place for payloads;
//! * stream sizes are bounded by [`CfbLimits`] and by the chain actually
//!   present in the file — a stream can never be read past its chain;
//! * every sector has at most one owner (FAT, DIFAT, directory, mini-FAT, mini
//!   stream or one stream): a sector shared between two streams is a "cross-link"
//!   and refused, because it lets a few kilobytes of file be read as gigabytes;
//! * the bytes handed out by [`CompoundFile::read_stream`] are metered against
//!   [`CfbLimits::max_total_read`], so re-reading does not multiply the footprint;
//! * entry names may not contain the characters the specification forbids
//!   (`/ \ : !`), hide data behind their terminating NUL or repeat among siblings,
//!   and the memory of all paths is bounded.

pub mod writer;

use core::fmt;
use std::cell::Cell;
use std::collections::{BTreeSet, HashSet};

use crate::bytes::{LeReader, slice, to_usize, u16_le, u32_le};
use crate::text::utf16le_until_nul;

/// File signature `D0 CF 11 E0 A1 B1 1A E1`.
pub const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

const MAXREGSECT: u32 = 0xFFFF_FFFA;
const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
const FREESECT: u32 = 0xFFFF_FFFF;
const NOSTREAM: u32 = 0xFFFF_FFFF;
const HEADER_DIFAT_ENTRIES: usize = 109;
const DIR_ENTRY_LEN: usize = 128;
const MINI_SECTOR_LEN: usize = 64;

/// Limits applied while parsing a compound file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CfbLimits {
    /// Maximum number of directory entries.
    pub max_directory_entries: usize,
    /// Maximum size of any single stream that may be read.
    pub max_stream_size: u64,
    /// Maximum storage nesting depth.
    pub max_depth: usize,
    /// Maximum number of bytes [`CompoundFile::read_stream`] hands out over the life of one
    /// parsed file (the mini stream read while parsing is bounded by the file itself).
    pub max_total_read: u64,
}

impl Default for CfbLimits {
    fn default() -> Self {
        Self {
            max_directory_entries: 65_536,
            max_stream_size: 256 << 20,
            max_depth: 32,
            max_total_read: 1 << 30,
        }
    }
}

/// Upper bound of the memory held by the `path` strings of all entries.
const MAX_TOTAL_PATH_BYTES: usize = 8 << 20;

/// Fatal structural errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfbError {
    /// The signature is missing.
    NotCompoundFile,
    /// The header is invalid.
    InvalidHeader(&'static str),
    /// A sector index points outside the file.
    SectorOutOfRange(u32),
    /// A sector chain loops.
    ChainCycle(u32),
    /// A chain is shorter than the stream it describes.
    ChainTooShort,
    /// The directory is malformed.
    InvalidDirectory(&'static str),
    /// The directory tree contains a cycle.
    DirectoryCycle,
    /// Too many directory entries.
    TooManyEntries(usize),
    /// A stream exceeds [`CfbLimits::max_stream_size`].
    StreamTooLarge(u64),
    /// No entry with the given index exists or it is not a stream.
    NotAStream,
    /// A sector belongs to two owners (for example to two streams).
    CrossLinked(u32),
    /// The streams read so far exceed [`CfbLimits::max_total_read`].
    ReadBudgetExceeded,
}

impl fmt::Display for CfbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotCompoundFile => f.write_str("not an OLE2 compound file"),
            Self::InvalidHeader(why) => write!(f, "invalid compound file header: {why}"),
            Self::SectorOutOfRange(s) => write!(f, "sector {s:#x} is outside the file"),
            Self::ChainCycle(s) => write!(f, "sector chain cycle at {s:#x}"),
            Self::ChainTooShort => f.write_str("sector chain shorter than declared stream size"),
            Self::InvalidDirectory(why) => write!(f, "invalid directory: {why}"),
            Self::DirectoryCycle => f.write_str("directory tree contains a cycle"),
            Self::TooManyEntries(n) => write!(f, "{n} directory entries exceed the limit"),
            Self::StreamTooLarge(n) => write!(f, "stream of {n} bytes exceeds the limit"),
            Self::NotAStream => f.write_str("entry is not a stream"),
            Self::CrossLinked(s) => write!(f, "sector {s:#x} is shared by two owners"),
            Self::ReadBudgetExceeded => f.write_str("total stream read budget exceeded"),
        }
    }
}

impl core::error::Error for CfbError {}

/// Non-fatal irregularities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfbAnomaly {
    /// Directory entries not reachable from the root (possible hidden data).
    OrphanedEntries(usize),
    /// A version 3 file stores garbage in the high 32 bits of a stream size.
    SizeHighBitsIgnored(String),
    /// The file has bytes beyond its last addressable sector.
    TrailingData(u64),
    /// A stream's chain is longer than needed for its size.
    ChainLongerThanSize(String),
}

/// Kind of a directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// The root storage.
    Root,
    /// A storage (directory).
    Storage,
    /// A stream (file).
    Stream,
}

/// A directory entry reachable from the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Entry name as stored (may contain control characters such as `\u{5}`).
    pub name: String,
    /// Full path from the root, `/`-separated, root excluded.
    pub path: String,
    /// Entry kind.
    pub kind: EntryKind,
    /// Class identifier (meaningful for storages / OLE objects).
    pub clsid: [u8; 16],
    /// Declared stream size.
    pub size: u64,
    /// Index of the parent storage, `None` for the root.
    pub parent: Option<usize>,
    /// Indices of direct children (storages only).
    pub children: Vec<usize>,
    start_sector: u32,
}

/// A parsed compound file.
#[derive(Debug, Clone)]
pub struct CompoundFile<'a> {
    data: &'a [u8],
    sector_size: usize,
    sector_count: u32,
    mini_cutoff: u64,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    mini_stream: Vec<u8>,
    entries: Vec<DirEntry>,
    anomalies: Vec<CfbAnomaly>,
    limits: CfbLimits,
    read_so_far: Cell<u64>,
}

/// Returns `true` if `data` begins with the OLE2 signature.
#[must_use]
pub fn has_cfb_signature(data: &[u8]) -> bool {
    data.get(..8) == Some(&SIGNATURE[..])
}

impl<'a> CompoundFile<'a> {
    /// Parses the header, allocation tables and directory.
    ///
    /// # Errors
    /// Returns a [`CfbError`] for any structural violation.
    pub fn parse(data: &'a [u8], limits: CfbLimits) -> Result<Self, CfbError> {
        if !has_cfb_signature(data) {
            return Err(CfbError::NotCompoundFile);
        }
        if data.len() < 512 {
            return Err(CfbError::InvalidHeader("file shorter than header"));
        }
        let h16 = |o: usize| u16_le(data, o).ok_or(CfbError::InvalidHeader("truncated"));
        let h32 = |o: usize| u32_le(data, o).ok_or(CfbError::InvalidHeader("truncated"));

        let major = h16(26)?;
        if h16(28)? != 0xFFFE {
            return Err(CfbError::InvalidHeader("byte order mark"));
        }
        let sector_shift = h16(30)?;
        let sector_size = match (major, sector_shift) {
            (3, 9) => 512usize,
            (4, 12) => 4096usize,
            _ => return Err(CfbError::InvalidHeader("version / sector size mismatch")),
        };
        if h16(32)? != 6 {
            return Err(CfbError::InvalidHeader("mini sector size"));
        }
        let num_dir_sectors = h32(40)?;
        if major == 3 && num_dir_sectors != 0 {
            return Err(CfbError::InvalidHeader("directory sector count must be 0 in v3"));
        }
        let num_fat_sectors = h32(44)?;
        let first_dir_sector = h32(48)?;
        let mini_cutoff = h32(56)?;
        if mini_cutoff != 4096 {
            return Err(CfbError::InvalidHeader("mini stream cutoff"));
        }
        let first_mini_fat = h32(60)?;
        let num_mini_fat = h32(64)?;
        let first_difat = h32(68)?;
        let num_difat = h32(72)?;

        let body = data.len().saturating_sub(sector_size);
        let full_sectors = body / sector_size;
        let partial = body % sector_size != 0;
        let sector_count = u32::try_from(full_sectors.saturating_add(usize::from(partial)))
            .map_err(|_| CfbError::InvalidHeader("file too large"))?;
        if sector_count > MAXREGSECT {
            return Err(CfbError::InvalidHeader("file too large"));
        }

        let mut cf = Self {
            data,
            sector_size,
            sector_count,
            mini_cutoff: u64::from(mini_cutoff),
            fat: Vec::new(),
            mini_fat: Vec::new(),
            mini_stream: Vec::new(),
            entries: Vec::new(),
            anomalies: Vec::new(),
            limits,
            read_so_far: Cell::new(0),
        };

        // --- DIFAT -------------------------------------------------------
        if num_fat_sectors > sector_count {
            return Err(CfbError::InvalidHeader("FAT sector count exceeds file size"));
        }
        let mut fat_sectors: Vec<u32> = Vec::new();
        let mut difat_sectors: Vec<u32> = Vec::new();
        for i in 0..HEADER_DIFAT_ENTRIES {
            let s = h32(76 + i * 4)?;
            if s != FREESECT {
                fat_sectors.push(s);
            }
        }
        let mut difat_sector = first_difat;
        let mut visited = HashSet::new();
        let per_difat = sector_size / 4 - 1;
        let mut difat_seen = 0u32;
        while difat_sector != ENDOFCHAIN && difat_sector != FREESECT {
            if difat_seen >= num_difat {
                return Err(CfbError::InvalidHeader("DIFAT chain longer than declared"));
            }
            if !visited.insert(difat_sector) {
                return Err(CfbError::ChainCycle(difat_sector));
            }
            difat_seen = difat_seen.saturating_add(1);
            difat_sectors.push(difat_sector);
            let sec = cf.sector(difat_sector)?;
            for i in 0..per_difat {
                let s = u32_le(sec, i * 4).ok_or(CfbError::SectorOutOfRange(difat_sector))?;
                if s != FREESECT {
                    fat_sectors.push(s);
                }
            }
            difat_sector =
                u32_le(sec, per_difat * 4).ok_or(CfbError::SectorOutOfRange(difat_sector))?;
        }
        let needed =
            to_usize(u64::from(num_fat_sectors)).ok_or(CfbError::InvalidHeader("FAT size"))?;
        if fat_sectors.len() < needed {
            return Err(CfbError::InvalidHeader("DIFAT lists fewer FAT sectors than declared"));
        }
        fat_sectors.truncate(needed);

        // --- FAT ---------------------------------------------------------
        let mut seen_fat = HashSet::new();
        let mut fat = Vec::with_capacity(needed.saturating_mul(sector_size / 4));
        for &s in &fat_sectors {
            if !seen_fat.insert(s) {
                return Err(CfbError::InvalidHeader("FAT sector listed twice"));
            }
            let sec = cf.sector(s)?;
            fat.extend(words(sec));
        }
        cf.fat = fat;

        // --- Directory ---------------------------------------------------
        let dir_chain = cf.chain(first_dir_sector, &cf.fat, cf.sector_count)?;
        // Count the entries before copying the sectors: the limit is about the chain, not
        // about what an attacker managed to make us allocate.
        let dir_entries = dir_chain.len().saturating_mul(sector_size / DIR_ENTRY_LEN);
        if dir_entries > limits.max_directory_entries {
            return Err(CfbError::TooManyEntries(dir_entries));
        }
        let mut dir_bytes = Vec::with_capacity(dir_chain.len().saturating_mul(sector_size));
        for &s in &dir_chain {
            dir_bytes.extend_from_slice(cf.sector(s)?);
        }
        let raw_entries = parse_directory(&dir_bytes, major, &limits, &mut cf.anomalies)?;

        // --- Mini FAT ----------------------------------------------------
        let mut mini_fat_chain: Vec<u32> = Vec::new();
        if num_mini_fat > 0 && first_mini_fat != ENDOFCHAIN {
            let chain = cf.chain(first_mini_fat, &cf.fat, cf.sector_count)?;
            if chain.len() < to_usize(u64::from(num_mini_fat)).unwrap_or(usize::MAX) {
                return Err(CfbError::ChainTooShort);
            }
            let mut mini_fat = Vec::with_capacity(chain.len().saturating_mul(sector_size / 4));
            for &s in &chain {
                let sec = cf.sector(s)?;
                mini_fat.extend(words(sec));
            }
            cf.mini_fat = mini_fat;
            mini_fat_chain = chain;
        }

        // --- Tree --------------------------------------------------------
        cf.entries = build_tree(raw_entries, &limits, &mut cf.anomalies)?;

        // --- Mini stream (owned by the root entry) ------------------------
        if let Some(root) = cf.entries.first()
            && root.size > 0
        {
            if root.size > limits.max_stream_size {
                return Err(CfbError::StreamTooLarge(root.size));
            }
            let start = root.start_sector;
            let size = root.size;
            cf.mini_stream = cf.read_regular(start, size)?;
        }

        cf.check_ownership(&[&fat_sectors, &difat_sectors, &dir_chain, &mini_fat_chain])?;

        let addressable =
            u64::from(sector_count).saturating_add(1).saturating_mul(sector_size as u64);
        if (data.len() as u64) > addressable {
            cf.anomalies.push(CfbAnomaly::TrailingData(data.len() as u64 - addressable));
        }
        Ok(cf)
    }

    /// All reachable entries; index 0 is the root.
    #[must_use]
    pub fn entries(&self) -> &[DirEntry] {
        &self.entries
    }

    /// Non-fatal irregularities.
    #[must_use]
    pub fn anomalies(&self) -> &[CfbAnomaly] {
        &self.anomalies
    }

    /// Finds an entry by `/`-separated path (case-insensitive, root excluded).
    #[must_use]
    pub fn find(&self, path: &str) -> Option<usize> {
        let wanted = path.trim_matches('/');
        self.entries.iter().position(|e| e.kind != EntryKind::Root && eq_ci(&e.path, wanted))
    }

    /// Finds a direct child of `storage` by name (case-insensitive).
    #[must_use]
    pub fn child(&self, storage: usize, name: &str) -> Option<usize> {
        let parent = self.entries.get(storage)?;
        parent
            .children
            .iter()
            .copied()
            .find(|&c| self.entries.get(c).is_some_and(|e| eq_ci(&e.name, name)))
    }

    /// Reads a stream entry completely.
    ///
    /// # Errors
    /// Fails if the entry is not a stream, too large or its chain is broken.
    pub fn read_stream(&self, index: usize) -> Result<Vec<u8>, CfbError> {
        let e = self.entries.get(index).ok_or(CfbError::NotAStream)?;
        if e.kind != EntryKind::Stream {
            return Err(CfbError::NotAStream);
        }
        if e.size > self.limits.max_stream_size {
            return Err(CfbError::StreamTooLarge(e.size));
        }
        let total = self.read_so_far.get().saturating_add(e.size);
        if total > self.limits.max_total_read {
            return Err(CfbError::ReadBudgetExceeded);
        }
        self.read_so_far.set(total);
        if e.size < self.mini_cutoff {
            self.read_mini(e.start_sector, e.size)
        } else {
            self.read_regular(e.start_sector, e.size)
        }
    }

    /// Refuses a file in which one sector has two owners.
    ///
    /// `system` lists the sectors of the FAT, DIFAT, directory and mini-FAT; the mini stream
    /// and every stream (regular or mini) are walked here too, each over exactly the links its
    /// declared size needs. A broken chain is not an error of this check: reading the stream
    /// reports it, and the sectors seen before the break stay claimed.
    fn check_ownership(&self, system: &[&[u32]]) -> Result<(), CfbError> {
        let mut owned = vec![false; to_usize(u64::from(self.sector_count)).unwrap_or(0)];
        let mut claim = |sectors: &[u32]| -> Result<(), CfbError> {
            for &s in sectors {
                let slot = owned.get_mut(s as usize).ok_or(CfbError::SectorOutOfRange(s))?;
                if core::mem::replace(slot, true) {
                    return Err(CfbError::CrossLinked(s));
                }
            }
            Ok(())
        };
        for sectors in system {
            claim(sectors)?;
        }
        if let Some(root) = self.entries.first()
            && root.size > 0
            && let Some(chain) = self.regular_chain(root.start_sector, root.size)
        {
            claim(&chain)?;
        }
        let mini_count = self.mini_stream.len() / MINI_SECTOR_LEN;
        let mut mini_owned = vec![false; mini_count];
        for e in &self.entries {
            if e.kind != EntryKind::Stream || e.size == 0 || e.size > self.limits.max_stream_size {
                continue;
            }
            if e.size < self.mini_cutoff {
                let needed = to_usize(e.size).unwrap_or(usize::MAX).div_ceil(MINI_SECTOR_LEN);
                let max = u32::try_from(mini_count).unwrap_or(u32::MAX);
                let (chain, _) = self.walk(e.start_sector, &self.mini_fat, max, needed);
                for s in chain {
                    let slot =
                        mini_owned.get_mut(s as usize).ok_or(CfbError::SectorOutOfRange(s))?;
                    if core::mem::replace(slot, true) {
                        return Err(CfbError::CrossLinked(s));
                    }
                }
            } else if let Some(chain) = self.regular_chain(e.start_sector, e.size) {
                claim(&chain)?;
            }
        }
        Ok(())
    }

    /// The links of a regular (FAT) chain a stream of `size` bytes needs; a broken chain
    /// yields the links before the break.
    fn regular_chain(&self, start: u32, size: u64) -> Option<Vec<u32>> {
        let needed = to_usize(size)?.div_ceil(self.sector_size);
        Some(self.walk(start, &self.fat, self.sector_count, needed).0)
    }

    fn sector(&self, index: u32) -> Result<&'a [u8], CfbError> {
        if index >= self.sector_count {
            return Err(CfbError::SectorOutOfRange(index));
        }
        let start = (index as usize)
            .checked_add(1)
            .and_then(|i| i.checked_mul(self.sector_size))
            .ok_or(CfbError::SectorOutOfRange(index))?;
        let end = start.saturating_add(self.sector_size).min(self.data.len());
        self.data.get(start..end).ok_or(CfbError::SectorOutOfRange(index))
    }

    fn chain(&self, start: u32, table: &[u32], max_index: u32) -> Result<Vec<u32>, CfbError> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = start;
        while cur != ENDOFCHAIN {
            if cur > MAXREGSECT || cur >= max_index {
                return Err(CfbError::SectorOutOfRange(cur));
            }
            if !seen.insert(cur) {
                return Err(CfbError::ChainCycle(cur));
            }
            out.push(cur);
            cur = *table.get(cur as usize).ok_or(CfbError::SectorOutOfRange(cur))?;
        }
        Ok(out)
    }

    fn read_regular(&self, start: u32, size: u64) -> Result<Vec<u8>, CfbError> {
        let size_usize = to_usize(size).ok_or(CfbError::StreamTooLarge(size))?;
        if size_usize == 0 {
            return Ok(Vec::new());
        }
        let needed = size_usize.div_ceil(self.sector_size);
        let chain = self.chain_bounded(start, &self.fat, self.sector_count, needed)?;
        let mut out = Vec::with_capacity(size_usize);
        for s in chain {
            let sec = self.sector(s)?;
            let take = (size_usize - out.len()).min(sec.len());
            out.extend_from_slice(sec.get(..take).unwrap_or(&[]));
        }
        if out.len() != size_usize {
            return Err(CfbError::ChainTooShort);
        }
        Ok(out)
    }

    fn read_mini(&self, start: u32, size: u64) -> Result<Vec<u8>, CfbError> {
        let size_usize = to_usize(size).ok_or(CfbError::StreamTooLarge(size))?;
        if size_usize == 0 {
            return Ok(Vec::new());
        }
        let mini_count =
            u32::try_from(self.mini_stream.len() / MINI_SECTOR_LEN).unwrap_or(u32::MAX);
        let needed = size_usize.div_ceil(MINI_SECTOR_LEN);
        let chain = self.chain_bounded(start, &self.mini_fat, mini_count, needed)?;
        let mut out = Vec::with_capacity(size_usize);
        for s in chain {
            let off =
                (s as usize).checked_mul(MINI_SECTOR_LEN).ok_or(CfbError::SectorOutOfRange(s))?;
            let sec = slice(&self.mini_stream, off, MINI_SECTOR_LEN)
                .ok_or(CfbError::SectorOutOfRange(s))?;
            let take = (size_usize - out.len()).min(MINI_SECTOR_LEN);
            out.extend_from_slice(sec.get(..take).unwrap_or(&[]));
        }
        if out.len() != size_usize {
            return Err(CfbError::ChainTooShort);
        }
        Ok(out)
    }

    /// Walks a chain but stops after `needed` links (the rest of an overlong
    /// chain is irrelevant for reading and must not cost time).
    fn chain_bounded(
        &self,
        start: u32,
        table: &[u32],
        max_index: u32,
        needed: usize,
    ) -> Result<Vec<u32>, CfbError> {
        match self.walk(start, table, max_index, needed) {
            (chain, Ok(())) => Ok(chain),
            (_, Err(e)) => Err(e),
        }
    }

    /// As [`Self::chain_bounded`], but a broken chain still yields the links before the break.
    fn walk(
        &self,
        start: u32,
        table: &[u32],
        max_index: u32,
        needed: usize,
    ) -> (Vec<u32>, Result<(), CfbError>) {
        let mut out = Vec::with_capacity(needed.min(1 << 20));
        let mut seen = HashSet::new();
        let mut cur = start;
        while out.len() < needed {
            if cur == ENDOFCHAIN {
                return (out, Err(CfbError::ChainTooShort));
            }
            if cur > MAXREGSECT || cur >= max_index {
                return (out, Err(CfbError::SectorOutOfRange(cur)));
            }
            if !seen.insert(cur) {
                return (out, Err(CfbError::ChainCycle(cur)));
            }
            out.push(cur);
            let Some(&next) = table.get(cur as usize) else {
                return (out, Err(CfbError::SectorOutOfRange(cur)));
            };
            cur = next;
        }
        (out, Ok(()))
    }

    /// Iterates `(index, entry)` over every stream.
    pub fn streams(&self) -> impl Iterator<Item = (usize, &DirEntry)> + '_ {
        self.entries.iter().enumerate().filter(|(_, e)| e.kind == EntryKind::Stream)
    }
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

fn words(sec: &[u8]) -> impl Iterator<Item = u32> + '_ {
    sec.chunks_exact(4).filter_map(|c| <[u8; 4]>::try_from(c).ok()).map(u32::from_le_bytes)
}

struct RawEntry {
    name: String,
    kind: Option<EntryKind>,
    left: u32,
    right: u32,
    child: u32,
    clsid: [u8; 16],
    start: u32,
    size: u64,
}

fn parse_directory(
    bytes: &[u8],
    major: u16,
    limits: &CfbLimits,
    anomalies: &mut Vec<CfbAnomaly>,
) -> Result<Vec<RawEntry>, CfbError> {
    let count = bytes.len() / DIR_ENTRY_LEN;
    if count > limits.max_directory_entries {
        return Err(CfbError::TooManyEntries(count));
    }
    let mut out = Vec::with_capacity(count);
    for raw in bytes.chunks_exact(DIR_ENTRY_LEN) {
        let mut r = LeReader::new(raw);
        let name_bytes = r.bytes(64).ok_or(CfbError::InvalidDirectory("entry"))?;
        let name_len = usize::from(r.u16().ok_or(CfbError::InvalidDirectory("entry"))?);
        let obj_type = r.u8().ok_or(CfbError::InvalidDirectory("entry"))?;
        let _color = r.u8();
        let left = r.u32().ok_or(CfbError::InvalidDirectory("entry"))?;
        let right = r.u32().ok_or(CfbError::InvalidDirectory("entry"))?;
        let child = r.u32().ok_or(CfbError::InvalidDirectory("entry"))?;
        let clsid_bytes = r.bytes(16).ok_or(CfbError::InvalidDirectory("entry"))?;
        let mut clsid = [0u8; 16];
        clsid.copy_from_slice(clsid_bytes);
        let _state = r.u32();
        let _ctime = r.u64();
        let _mtime = r.u64();
        let start = r.u32().ok_or(CfbError::InvalidDirectory("entry"))?;
        let mut size = r.u64().ok_or(CfbError::InvalidDirectory("entry"))?;

        let kind = match obj_type {
            0 => None,
            1 => Some(EntryKind::Storage),
            2 => Some(EntryKind::Stream),
            5 => Some(EntryKind::Root),
            _ => return Err(CfbError::InvalidDirectory("unknown object type")),
        };
        let name = if kind.is_some() {
            if name_len > 64 || name_len % 2 != 0 {
                return Err(CfbError::InvalidDirectory("name length"));
            }
            let declared = name_bytes.get(..name_len).unwrap_or(&[]);
            let name = utf16le_until_nul(declared);
            // Anything but padding after the terminating NUL is data other readers would see
            // as part of the name (or ignore): two different names for one entry.
            let kept = name.encode_utf16().count().saturating_mul(2);
            if declared.get(kept..).is_some_and(|rest| rest.iter().any(|&b| b != 0)) {
                return Err(CfbError::InvalidDirectory("data after the name terminator"));
            }
            if obj_type != 5 && name.contains(['/', '\\', ':', '!']) {
                return Err(CfbError::InvalidDirectory("forbidden character in name"));
            }
            name
        } else {
            String::new()
        };
        if major == 3 && size >> 32 != 0 {
            anomalies.push(CfbAnomaly::SizeHighBitsIgnored(name.clone()));
            size &= 0xFFFF_FFFF;
        }
        out.push(RawEntry { name, kind, left, right, child, clsid, start, size });
    }
    match out.first().and_then(|e| e.kind) {
        Some(EntryKind::Root) => Ok(out),
        _ => Err(CfbError::InvalidDirectory("first entry is not the root")),
    }
}

fn build_tree(
    raw: Vec<RawEntry>,
    limits: &CfbLimits,
    anomalies: &mut Vec<CfbAnomaly>,
) -> Result<Vec<DirEntry>, CfbError> {
    let total_live = raw.iter().filter(|e| e.kind.is_some()).count();
    // Map raw index -> output index.
    let mut out: Vec<DirEntry> = Vec::new();
    let mut map = vec![usize::MAX; raw.len()];
    let mut visited: BTreeSet<u32> = BTreeSet::new();
    let mut sibling_names: HashSet<(usize, String)> = HashSet::new();
    let mut path_bytes = 0usize;

    let root = raw.first().ok_or(CfbError::InvalidDirectory("empty"))?;
    out.push(DirEntry {
        name: root.name.clone(),
        path: String::new(),
        kind: EntryKind::Root,
        clsid: root.clsid,
        size: root.size,
        parent: None,
        children: Vec::new(),
        start_sector: root.start,
    });
    if let Some(m) = map.get_mut(0) {
        *m = 0;
    }
    visited.insert(0);

    // (storage output index, child subtree root raw index, depth)
    let mut work: Vec<(usize, u32, usize)> = vec![(0, root.child, 1)];
    while let Some((parent_out, subtree, depth)) = work.pop() {
        if subtree == NOSTREAM {
            continue;
        }
        if depth > limits.max_depth {
            return Err(CfbError::InvalidDirectory("storage nesting too deep"));
        }
        // Iterative traversal of the sibling tree.
        let mut stack = vec![subtree];
        while let Some(idx) = stack.pop() {
            if idx == NOSTREAM {
                continue;
            }
            if !visited.insert(idx) {
                return Err(CfbError::DirectoryCycle);
            }
            let e = raw.get(idx as usize).ok_or(CfbError::InvalidDirectory("sibling index"))?;
            let kind = match e.kind {
                Some(EntryKind::Root) | None => {
                    return Err(CfbError::InvalidDirectory("invalid entry in tree"));
                }
                Some(k) => k,
            };
            let parent_path = out.get(parent_out).map(|p| p.path.clone()).unwrap_or_default();
            let path = if parent_path.is_empty() {
                e.name.clone()
            } else {
                format!("{parent_path}/{}", e.name)
            };
            // Names compare case-insensitively: two siblings that differ only in case would be
            // one entry to some readers and two to others.
            let folded: String = e.name.chars().flat_map(char::to_lowercase).collect();
            if !sibling_names.insert((parent_out, folded)) {
                return Err(CfbError::InvalidDirectory("duplicate sibling name"));
            }
            path_bytes = path_bytes.saturating_add(path.len());
            if path_bytes > MAX_TOTAL_PATH_BYTES {
                return Err(CfbError::InvalidDirectory("paths too large"));
            }
            let my_out = out.len();
            out.push(DirEntry {
                name: e.name.clone(),
                path,
                kind,
                clsid: e.clsid,
                size: if kind == EntryKind::Stream { e.size } else { 0 },
                parent: Some(parent_out),
                children: Vec::new(),
                start_sector: e.start,
            });
            if let Some(m) = map.get_mut(idx as usize) {
                *m = my_out;
            }
            if let Some(p) = out.get_mut(parent_out) {
                p.children.push(my_out);
            }
            stack.push(e.left);
            stack.push(e.right);
            if kind == EntryKind::Storage {
                work.push((my_out, e.child, depth + 1));
            } else if e.child != NOSTREAM {
                return Err(CfbError::InvalidDirectory("stream with children"));
            }
        }
    }
    let reachable = out.len();
    if reachable < total_live {
        anomalies.push(CfbAnomaly::OrphanedEntries(total_live - reachable));
    }
    Ok(out)
}

/// Formats a CLSID as `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
#[must_use]
pub fn format_clsid(c: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    let d2 = u16::from_le_bytes([c[4], c[5]]);
    let d3 = u16::from_le_bytes([c[6], c[7]]);
    format!(
        "{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        c[8], c[9], c[10], c[11], c[12], c[13], c[14], c[15]
    )
}

/// `true` if the CLSID is all zeroes.
#[must_use]
pub fn clsid_is_null(c: &[u8; 16]) -> bool {
    c.iter().all(|&b| b == 0)
}

#[cfg(test)]
mod tests;
