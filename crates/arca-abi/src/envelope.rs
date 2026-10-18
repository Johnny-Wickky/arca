//! Result envelope written by a guest into its linear memory.
//!
//! ```text
//! offset  size  field
//! 0       4     magic  "ARCA"
//! 4       2     ABI version (u16, = 1)
//! 6       2     payload kind (u16, see PayloadKind)
//! 8       4     payload length (u32)
//! 12      n     payload (wire-encoded)
//! ```

use alloc::vec::Vec;

use crate::wire::WireError;

/// Envelope magic.
pub const MAGIC: [u8; 4] = *b"ARCA";
/// Header size in bytes.
pub const HEADER_LEN: usize = 12;

/// What the payload contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    /// A wire-encoded threat X-ray report.
    XRayReport,
    /// A wire-encoded sterilised document.
    SanitizedDocument,
    /// A wire-encoded error description (`String`).
    Error,
}

impl PayloadKind {
    const fn code(self) -> u16 {
        match self {
            Self::XRayReport => 1,
            Self::SanitizedDocument => 2,
            Self::Error => 0xFFFF,
        }
    }

    const fn from_code(c: u16) -> Option<Self> {
        match c {
            1 => Some(Self::XRayReport),
            2 => Some(Self::SanitizedDocument),
            0xFFFF => Some(Self::Error),
            _ => None,
        }
    }
}

/// Wraps a payload.
///
/// # Errors
/// [`WireError::EncodeOverflow`] if the payload exceeds 4 GiB.
pub fn seal(kind: PayloadKind, payload: &[u8]) -> Result<Vec<u8>, WireError> {
    let len = u32::try_from(payload.len()).map_err(|_| WireError::EncodeOverflow)?;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&kind.code().to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Validates an envelope and returns its kind and payload.
///
/// # Errors
/// Bad magic, unknown version or kind, or a length that does not match.
pub fn open(data: &[u8]) -> Result<(PayloadKind, &[u8]), WireError> {
    let header = data.get(..HEADER_LEN).ok_or(WireError::UnexpectedEnd)?;
    let (magic, rest) = header.split_at(4);
    if magic != MAGIC {
        return Err(WireError::InvalidTag(0));
    }
    let version =
        u16::from_le_bytes([rest.first().copied().unwrap_or(0), rest.get(1).copied().unwrap_or(0)]);
    if version != 1 {
        return Err(WireError::InvalidTag(version));
    }
    let code =
        u16::from_le_bytes([rest.get(2).copied().unwrap_or(0), rest.get(3).copied().unwrap_or(0)]);
    let kind = PayloadKind::from_code(code).ok_or(WireError::InvalidTag(code))?;
    let len_bytes: [u8; 4] =
        rest.get(4..8).and_then(|s| s.try_into().ok()).ok_or(WireError::UnexpectedEnd)?;
    let len = usize::try_from(u32::from_le_bytes(len_bytes)).map_err(|_| WireError::LengthLimit)?;
    let payload = data.get(HEADER_LEN..).ok_or(WireError::UnexpectedEnd)?;
    if payload.len() != len {
        return Err(if payload.len() < len {
            WireError::UnexpectedEnd
        } else {
            WireError::TrailingBytes
        });
    }
    Ok((kind, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let e = seal(PayloadKind::XRayReport, b"payload").unwrap();
        assert_eq!(open(&e).unwrap(), (PayloadKind::XRayReport, &b"payload"[..]));
    }

    #[test]
    fn tampering_is_detected() {
        let e = seal(PayloadKind::Error, b"x").unwrap();
        let mut bad = e.clone();
        bad[0] = b'X';
        assert!(open(&bad).is_err());
        let mut bad = e.clone();
        bad[6] = 9;
        assert!(open(&bad).is_err());
        assert!(open(&e[..e.len() - 1]).is_err());
        let mut longer = e.clone();
        longer.push(0);
        assert_eq!(open(&longer), Err(WireError::TrailingBytes));
        assert!(open(&e[..5]).is_err());
    }
}
