//! Minimal, deterministic compound file writer (version 3, 512-byte sectors).
//!
//! Used to rebuild sterilised OLE2 containers and to synthesise adversarial
//! fixtures in tests. Output is reproducible: timestamps are zero, all
//! directory entries are black and sibling trees are perfectly balanced.

use std::collections::BTreeMap;

use super::{CfbError, ENDOFCHAIN, FREESECT, NOSTREAM, SIGNATURE};

const SECTOR: usize = 512;
const MINI: usize = 64;
const CUTOFF: usize = 4096;
const FATSECT: u32 = 0xFFFF_FFFD;
const DIFSECT: u32 = 0xFFFF_FFFC;
const PER_SECTOR: usize = SECTOR / 4;

#[derive(Debug, Clone)]
enum Node {
    Storage { clsid: [u8; 16], children: BTreeMap<String, Node> },
    Stream(Vec<u8>),
}

/// Builder for a compound file.
#[derive(Debug, Clone)]
pub struct CfbBuilder {
    root_clsid: [u8; 16],
    root: BTreeMap<String, Node>,
}

impl Default for CfbBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_name(name: &str) -> Result<(), CfbError> {
    let units = name.encode_utf16().count();
    if name.is_empty() || units > 31 || name.contains(['/', '\\', ':', '!']) {
        return Err(CfbError::InvalidDirectory("invalid entry name"));
    }
    Ok(())
}

impl CfbBuilder {
    /// Creates an empty compound file.
    #[must_use]
    pub fn new() -> Self {
        Self { root_clsid: [0; 16], root: BTreeMap::new() }
    }

    /// Sets the CLSID of the root storage.
    pub fn set_root_clsid(&mut self, clsid: [u8; 16]) {
        self.root_clsid = clsid;
    }

    fn storage_mut(&mut self, path: &[&str]) -> Result<&mut BTreeMap<String, Node>, CfbError> {
        let mut cur = &mut self.root;
        for comp in path {
            validate_name(comp)?;
            let node = cur
                .entry((*comp).to_owned())
                .or_insert_with(|| Node::Storage { clsid: [0; 16], children: BTreeMap::new() });
            cur = match node {
                Node::Storage { children, .. } => children,
                Node::Stream(_) => return Err(CfbError::InvalidDirectory("path crosses a stream")),
            };
        }
        Ok(cur)
    }

    /// Creates a storage (and its parents) at `path` with the given CLSID.
    ///
    /// # Errors
    /// Fails on invalid names or when the path crosses a stream.
    pub fn add_storage(&mut self, path: &str, clsid: [u8; 16]) -> Result<(), CfbError> {
        let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
        let (last, parents) = comps.split_last().ok_or(CfbError::InvalidDirectory("empty path"))?;
        validate_name(last)?;
        let parent = self.storage_mut(parents)?;
        match parent
            .entry((*last).to_owned())
            .or_insert_with(|| Node::Storage { clsid, children: BTreeMap::new() })
        {
            Node::Storage { clsid: c, .. } => {
                *c = clsid;
                Ok(())
            }
            Node::Stream(_) => {
                Err(CfbError::InvalidDirectory("a stream already exists at this path"))
            }
        }
    }

    /// Adds a stream at `path`, creating parent storages as needed.
    ///
    /// # Errors
    /// Fails on invalid names, duplicates or when the path crosses a stream.
    pub fn add_stream(&mut self, path: &str, data: Vec<u8>) -> Result<(), CfbError> {
        let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
        let (last, parents) = comps.split_last().ok_or(CfbError::InvalidDirectory("empty path"))?;
        validate_name(last)?;
        let parent = self.storage_mut(parents)?;
        if parent.contains_key(*last) {
            return Err(CfbError::InvalidDirectory("duplicate entry"));
        }
        parent.insert((*last).to_owned(), Node::Stream(data));
        Ok(())
    }

    /// Serialises the compound file.
    ///
    /// # Errors
    /// Fails if the file would exceed format limits.
    pub fn finish(self) -> Result<Vec<u8>, CfbError> {
        // 1. Flatten the tree into directory entries.
        let mut dir: Vec<FlatEntry> = vec![FlatEntry {
            name: "Root Entry".to_owned(),
            kind: 5,
            clsid: self.root_clsid,
            left: NOSTREAM,
            right: NOSTREAM,
            child: NOSTREAM,
            data: None,
            start: ENDOFCHAIN,
            size: 0,
        }];
        let child = flatten(&self.root, &mut dir)?;
        if let Some(root) = dir.first_mut() {
            root.child = child;
        }

        // 2. Lay out stream data.
        let mut regular: Vec<u8> = Vec::new();
        let mut mini: Vec<u8> = Vec::new();
        let mut fat: Vec<u32> = Vec::new();
        let mut mini_fat: Vec<u32> = Vec::new();
        for e in dir.iter_mut().skip(1) {
            let Some(data) = e.data.take() else { continue };
            e.size = data.len() as u64;
            if data.is_empty() {
                e.start = ENDOFCHAIN;
            } else if data.len() < CUTOFF {
                e.start = u32::try_from(mini.len() / MINI).map_err(|_| too_big())?;
                append_chain(&mut mini, &mut mini_fat, &data, MINI)?;
            } else {
                e.start = u32::try_from(regular.len() / SECTOR).map_err(|_| too_big())?;
                append_chain(&mut regular, &mut fat, &data, SECTOR)?;
            }
        }
        // The mini stream itself is a regular stream owned by the root.
        if !mini.is_empty() {
            let start = u32::try_from(regular.len() / SECTOR).map_err(|_| too_big())?;
            let mini_bytes = mini.clone();
            append_chain(&mut regular, &mut fat, &mini_bytes, SECTOR)?;
            if let Some(root) = dir.first_mut() {
                root.start = start;
                root.size = mini.len() as u64;
            }
        }
        // Mini FAT sectors.
        let mut mini_fat_bytes = Vec::new();
        for v in &mini_fat {
            mini_fat_bytes.extend_from_slice(&v.to_le_bytes());
        }
        let (first_mini_fat, num_mini_fat) = if mini_fat_bytes.is_empty() {
            (ENDOFCHAIN, 0u32)
        } else {
            let start = u32::try_from(regular.len() / SECTOR).map_err(|_| too_big())?;
            let n = mini_fat_bytes.len().div_ceil(SECTOR);
            pad_with(&mut mini_fat_bytes, SECTOR, FREESECT);
            append_chain(&mut regular, &mut fat, &mini_fat_bytes, SECTOR)?;
            (start, u32::try_from(n).map_err(|_| too_big())?)
        };
        // Directory sectors.
        let mut dir_bytes = Vec::with_capacity(dir.len() * 128);
        for e in &dir {
            e.write(&mut dir_bytes);
        }
        while !dir_bytes.len().is_multiple_of(SECTOR) {
            FlatEntry::unused().write(&mut dir_bytes);
        }
        let first_dir = u32::try_from(regular.len() / SECTOR).map_err(|_| too_big())?;
        append_chain(&mut regular, &mut fat, &dir_bytes, SECTOR)?;

        // 3. FAT + DIFAT sizing (fixed point: FAT must describe itself).
        let data_sectors = regular.len() / SECTOR;
        let mut n_fat = 1usize;
        let mut n_difat;
        loop {
            n_difat = n_fat.saturating_sub(109).div_ceil(PER_SECTOR - 1);
            let total = data_sectors + n_fat + n_difat;
            if n_fat * PER_SECTOR >= total {
                break;
            }
            n_fat += 1;
        }
        let fat_start = data_sectors;
        fat.extend(std::iter::repeat_n(FATSECT, n_fat));
        fat.extend(std::iter::repeat_n(DIFSECT, n_difat));
        fat.resize(n_fat * PER_SECTOR, FREESECT);
        let fat_ids: Vec<u32> = (fat_start..fat_start + n_fat)
            .map(|s| u32::try_from(s).map_err(|_| too_big()))
            .collect::<Result<_, _>>()?;
        let difat_start = fat_start + n_fat;

        // 4. Header.
        let mut out = Vec::with_capacity(SECTOR * (1 + data_sectors + n_fat + n_difat));
        out.extend_from_slice(&SIGNATURE);
        out.extend_from_slice(&[0u8; 16]);
        put16(&mut out, 0x003E);
        put16(&mut out, 3);
        put16(&mut out, 0xFFFE);
        put16(&mut out, 9);
        put16(&mut out, 6);
        out.extend_from_slice(&[0u8; 6]);
        put32(&mut out, 0);
        put32(&mut out, u32::try_from(n_fat).map_err(|_| too_big())?);
        put32(&mut out, first_dir);
        put32(&mut out, 0);
        put32(&mut out, u32::try_from(CUTOFF).map_err(|_| too_big())?);
        put32(&mut out, first_mini_fat);
        put32(&mut out, num_mini_fat);
        put32(
            &mut out,
            if n_difat == 0 {
                ENDOFCHAIN
            } else {
                u32::try_from(difat_start).map_err(|_| too_big())?
            },
        );
        put32(&mut out, u32::try_from(n_difat).map_err(|_| too_big())?);
        for i in 0..109 {
            put32(&mut out, fat_ids.get(i).copied().unwrap_or(FREESECT));
        }

        // 5. Body.
        out.extend_from_slice(&regular);
        for v in &fat {
            put32(&mut out, *v);
        }
        let rest: Vec<u32> = fat_ids.iter().skip(109).copied().collect();
        for (i, chunk) in rest.chunks(PER_SECTOR - 1).enumerate() {
            for v in chunk {
                put32(&mut out, *v);
            }
            for _ in chunk.len()..PER_SECTOR - 1 {
                put32(&mut out, FREESECT);
            }
            let next = if i + 1 < n_difat {
                u32::try_from(difat_start + i + 1).map_err(|_| too_big())?
            } else {
                ENDOFCHAIN
            };
            put32(&mut out, next);
        }
        Ok(out)
    }
}

fn too_big() -> CfbError {
    CfbError::InvalidHeader("compound file too large")
}

struct FlatEntry {
    name: String,
    kind: u8,
    clsid: [u8; 16],
    left: u32,
    right: u32,
    child: u32,
    data: Option<Vec<u8>>,
    start: u32,
    size: u64,
}

impl FlatEntry {
    fn unused() -> Self {
        Self {
            name: String::new(),
            kind: 0,
            clsid: [0; 16],
            left: NOSTREAM,
            right: NOSTREAM,
            child: NOSTREAM,
            data: None,
            start: 0,
            size: 0,
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut name = [0u8; 64];
        let units: Vec<u16> = self.name.encode_utf16().collect();
        for (dst, u) in name.chunks_exact_mut(2).zip(units.iter()) {
            dst.copy_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&name);
        let name_len =
            if self.kind == 0 { 0 } else { u16::try_from((units.len() + 1) * 2).unwrap_or(64) };
        put16(out, name_len);
        out.push(self.kind);
        out.push(1); // black
        put32(out, self.left);
        put32(out, self.right);
        put32(out, self.child);
        out.extend_from_slice(&self.clsid);
        put32(out, 0);
        out.extend_from_slice(&[0u8; 16]);
        put32(out, self.start);
        out.extend_from_slice(&self.size.to_le_bytes());
    }
}

/// CFB ordering: shorter names first, then case-insensitive code-unit order.
fn cfb_key(name: &str) -> (usize, Vec<u16>) {
    let upper: Vec<u16> = name.to_uppercase().encode_utf16().collect();
    (name.encode_utf16().count(), upper)
}

fn flatten(children: &BTreeMap<String, Node>, dir: &mut Vec<FlatEntry>) -> Result<u32, CfbError> {
    let mut sorted: Vec<(&String, &Node)> = children.iter().collect();
    sorted.sort_by_key(|(n, _)| cfb_key(n));
    let mut ids = Vec::with_capacity(sorted.len());
    for (name, node) in &sorted {
        let id = u32::try_from(dir.len()).map_err(|_| too_big())?;
        let (kind, clsid, data) = match node {
            Node::Storage { clsid, .. } => (1u8, *clsid, None),
            Node::Stream(d) => (2u8, [0u8; 16], Some(d.clone())),
        };
        dir.push(FlatEntry {
            name: (*name).clone(),
            kind,
            clsid,
            left: NOSTREAM,
            right: NOSTREAM,
            child: NOSTREAM,
            data,
            start: ENDOFCHAIN,
            size: 0,
        });
        ids.push(id);
    }
    for ((_, node), id) in sorted.iter().zip(&ids) {
        if let Node::Storage { children, .. } = node {
            let c = flatten(children, dir)?;
            if let Some(e) = dir.get_mut(*id as usize) {
                e.child = c;
            }
        }
    }
    Ok(balance(&ids, dir))
}

fn balance(ids: &[u32], dir: &mut [FlatEntry]) -> u32 {
    if ids.is_empty() {
        return NOSTREAM;
    }
    let mid = ids.len() / 2;
    let (left, rest) = ids.split_at(mid);
    let Some((&root, right)) = rest.split_first() else { return NOSTREAM };
    let l = balance(left, dir);
    let r = balance(right, dir);
    if let Some(e) = dir.get_mut(root as usize) {
        e.left = l;
        e.right = r;
    }
    root
}

fn append_chain(
    buf: &mut Vec<u8>,
    table: &mut Vec<u32>,
    data: &[u8],
    unit: usize,
) -> Result<(), CfbError> {
    let first = buf.len() / unit;
    let count = data.len().div_ceil(unit).max(1);
    buf.extend_from_slice(data);
    let padded = (first + count) * unit;
    buf.resize(padded, 0);
    if table.len() < first {
        table.resize(first, FREESECT);
    }
    for i in 0..count {
        let next = if i + 1 == count {
            ENDOFCHAIN
        } else {
            u32::try_from(first + i + 1).map_err(|_| too_big())?
        };
        table.push(next);
    }
    Ok(())
}

fn pad_with(buf: &mut Vec<u8>, unit: usize, word: u32) {
    while !buf.len().is_multiple_of(unit) {
        buf.extend_from_slice(&word.to_le_bytes());
    }
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
