//! Deterministic ZIP writer used to emit sterilised packages.
//!
//! Output is byte-for-byte reproducible: every member carries the DOS epoch
//! timestamp (1980-01-01 00:00:00), no extra fields, no comments and no
//! host-specific attributes, so a sanitised copy leaks neither the time it
//! was produced nor anything about the producing device.

use std::collections::BTreeSet;

use miniz_oxide::deflate::compress_to_vec;

use super::name::{fold_name, validate_member_str};
use super::{METHOD_DEFLATE, METHOD_STORED, ZipError};

/// How a member is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// No compression (required e.g. for the ODF `mimetype` member).
    Stored,
    /// DEFLATE; falls back to stored when deflate would not shrink the data.
    Deflated,
}

const DOS_DATE_1980_01_01: u16 = (1 << 5) | 1;
const VERSION_20: u16 = 20;
const FLAG_UTF8: u16 = 0x0800;

/// Builds an archive in memory.
#[derive(Debug, Default)]
pub struct ZipWriter {
    out: Vec<u8>,
    central: Vec<u8>,
    count: u16,
    names: BTreeSet<String>,
}

impl ZipWriter {
    /// Creates an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a member.
    ///
    /// # Errors
    /// Rejects invalid or duplicate names and archives that would need ZIP64.
    pub fn add(
        &mut self,
        name: &str,
        data: &[u8],
        compression: Compression,
    ) -> Result<(), ZipError> {
        validate_member_str(name).map_err(|reason| ZipError::InvalidName {
            preview: crate::text::display_safe(name, 80),
            reason,
        })?;
        if !self.names.insert(fold_name(name)) {
            return Err(ZipError::DuplicateName(name.to_owned()));
        }
        let too_large =
            || ZipError::EntryTooLarge { name: name.to_owned(), size: data.len() as u64 };

        let crc = arca_digest::crc32(data);
        let deflated;
        let (method, payload): (u16, &[u8]) = match compression {
            Compression::Stored => (METHOD_STORED, data),
            Compression::Deflated => {
                deflated = compress_to_vec(data, 6);
                if deflated.len() < data.len() {
                    (METHOD_DEFLATE, &deflated)
                } else {
                    (METHOD_STORED, data)
                }
            }
        };
        let usize32 = u32::try_from(data.len()).map_err(|_| too_large())?;
        let csize32 = u32::try_from(payload.len()).map_err(|_| too_large())?;
        let name_len = u16::try_from(name.len()).map_err(|_| too_large())?;
        let offset = u32::try_from(self.out.len()).map_err(|_| too_large())?;
        let count = self.count.checked_add(1).ok_or_else(too_large)?;
        let flags = if name.is_ascii() { 0 } else { FLAG_UTF8 };

        // Local file header.
        put32(&mut self.out, 0x0403_4b50);
        put16(&mut self.out, VERSION_20);
        put16(&mut self.out, flags);
        put16(&mut self.out, method);
        put16(&mut self.out, 0); // time 00:00:00
        put16(&mut self.out, DOS_DATE_1980_01_01);
        put32(&mut self.out, crc);
        put32(&mut self.out, csize32);
        put32(&mut self.out, usize32);
        put16(&mut self.out, name_len);
        put16(&mut self.out, 0); // extra length
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(payload);

        // Central directory header.
        put32(&mut self.central, 0x0201_4b50);
        put16(&mut self.central, VERSION_20); // made by: MS-DOS host, 2.0
        put16(&mut self.central, VERSION_20);
        put16(&mut self.central, flags);
        put16(&mut self.central, method);
        put16(&mut self.central, 0);
        put16(&mut self.central, DOS_DATE_1980_01_01);
        put32(&mut self.central, crc);
        put32(&mut self.central, csize32);
        put32(&mut self.central, usize32);
        put16(&mut self.central, name_len);
        put16(&mut self.central, 0); // extra
        put16(&mut self.central, 0); // comment
        put16(&mut self.central, 0); // disk start
        put16(&mut self.central, 0); // internal attributes
        put32(&mut self.central, 0); // external attributes
        put32(&mut self.central, offset);
        self.central.extend_from_slice(name.as_bytes());

        self.count = count;
        Ok(())
    }

    /// Finishes the archive and returns its bytes.
    ///
    /// # Errors
    /// Fails if the archive would exceed the classic (non-ZIP64) limits.
    pub fn finish(mut self) -> Result<Vec<u8>, ZipError> {
        let too_large =
            || ZipError::EntryTooLarge { name: String::from("<archive>"), size: u64::MAX };
        let cd_offset = u32::try_from(self.out.len()).map_err(|_| too_large())?;
        let cd_size = u32::try_from(self.central.len()).map_err(|_| too_large())?;
        self.out.append(&mut self.central);
        put32(&mut self.out, 0x0605_4b50);
        put16(&mut self.out, 0);
        put16(&mut self.out, 0);
        put16(&mut self.out, self.count);
        put16(&mut self.out, self.count);
        put32(&mut self.out, cd_size);
        put32(&mut self.out, cd_offset);
        put16(&mut self.out, 0);
        Ok(self.out)
    }
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
