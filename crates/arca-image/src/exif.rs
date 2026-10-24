//! A small, strict reader for the EXIF block of a photograph.
//!
//! Only what matters to a person who is about to share the picture is read: the orientation (to
//! turn the picture upright), the facts that identify the camera and the owner, whether there is
//! a GPS position (and, if so, where), and whether an embedded thumbnail is present (editing
//! programs often leave the *uncropped* original in it). Nothing from this block is ever shown as
//! a picture or interpreted as anything but short plain text and numbers.

use arca_abi::wire_struct;

use crate::bytes::to_usize;

/// Longest text kept from an EXIF string.
const MAX_TEXT: usize = 64;

/// What the EXIF block says, reduced to facts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExifFacts {
    /// Camera maker.
    pub make: String,
    /// Camera model.
    pub model: String,
    /// Software that wrote the file.
    pub software: String,
    /// When the picture was taken or changed (`YYYY:MM:DD HH:MM:SS` as written).
    pub datetime: String,
    /// The photographer's name.
    pub artist: String,
    /// The copyright line.
    pub copyright: String,
    /// The owner of the camera.
    pub owner: String,
    /// Orientation 1..=8 (1 is upright).
    pub orientation: u8,
    /// A body, lens or image serial number is present.
    pub serial: bool,
    /// A maker note (an opaque vendor block) is present.
    pub maker_note: bool,
    /// An embedded thumbnail is present.
    pub thumbnail: bool,
    /// A GPS block is present (or the tag that points to one, even if the pointer is unusable).
    pub gps: bool,
    /// A free-text field is present: an image description, a user comment, or one of the
    /// comment, title, author, keyword and subject fields Windows writes.
    pub free_text: bool,
    /// The GPS position could be read.
    pub has_position: bool,
    /// Latitude in millionths of a degree (north positive).
    pub lat_micro: i64,
    /// Longitude in millionths of a degree (east positive).
    pub lon_micro: i64,
}
wire_struct!(ExifFacts {
    make,
    model,
    software,
    datetime,
    artist,
    copyright,
    owner,
    orientation,
    serial,
    maker_note,
    thumbnail,
    gps,
    free_text,
    has_position,
    lat_micro,
    lon_micro
});

impl ExifFacts {
    /// `true` if the block says anything a person may not want to share.
    #[must_use]
    pub fn is_private(&self) -> bool {
        self.gps
            || self.serial
            || self.thumbnail
            || self.maker_note
            || self.free_text
            || !(self.artist.is_empty() && self.owner.is_empty() && self.copyright.is_empty())
    }

    /// Folds in the facts of another EXIF block of the same file. Flags accumulate and a text
    /// that is still empty is filled; the orientation and the position of the first block win.
    fn absorb(&mut self, o: Self) {
        for (mine, theirs) in [
            (&mut self.make, o.make),
            (&mut self.model, o.model),
            (&mut self.software, o.software),
            (&mut self.datetime, o.datetime),
            (&mut self.artist, o.artist),
            (&mut self.copyright, o.copyright),
            (&mut self.owner, o.owner),
        ] {
            if mine.is_empty() {
                *mine = theirs;
            }
        }
        self.serial |= o.serial;
        self.maker_note |= o.maker_note;
        self.thumbnail |= o.thumbnail;
        self.gps |= o.gps;
        self.free_text |= o.free_text;
        if !self.has_position && o.has_position {
            (self.has_position, self.lat_micro, self.lon_micro) = (true, o.lat_micro, o.lon_micro);
        }
    }
}

/// Records the EXIF block `found` in `slot`: the first one is kept as it is, later ones are
/// folded in. A file can carry several (two `Exif` APP1 segments, a PNG `eXIf` chunk and more);
/// a reader that looked only at the first would miss what a later one holds.
pub(crate) fn merge_into(slot: &mut Option<ExifFacts>, found: ExifFacts) {
    match slot {
        Some(first) => first.absorb(found),
        None => *slot = Some(found),
    }
}

#[derive(Clone, Copy)]
struct Order {
    little: bool,
}

impl Order {
    fn u16(self, d: &[u8], at: usize) -> Option<u16> {
        let b = d.get(at..at.checked_add(2)?)?;
        let a = [*b.first()?, *b.get(1)?];
        Some(if self.little { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) })
    }

    fn u32(self, d: &[u8], at: usize) -> Option<u32> {
        let b = d.get(at..at.checked_add(4)?)?;
        let a = [*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?];
        Some(if self.little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    }
}

struct Entry {
    tag: u16,
    typ: u16,
    count: usize,
    /// Where the value bytes start in the TIFF block.
    at: usize,
}

fn type_size(typ: u16) -> usize {
    match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        // 13 is IFD (a sub-IFD offset), 9 SLONG, 11 FLOAT.
        4 | 9 | 11 | 13 => 4,
        // 16 to 18 are the 64-bit types of BigTIFF.
        5 | 10 | 12 | 16..=18 => 8,
        _ => 0,
    }
}

/// The entries of the IFD at `off`, and the offset of the next IFD (0 for none). Every entry the
/// count field declares is read (it is a `u16`, so at most 65 535 of them): a limit lower than
/// that would let a tag hide behind a run of filler entries.
fn ifd(d: &[u8], o: Order, off: usize) -> (Vec<Entry>, usize) {
    let mut out = Vec::new();
    let Some(n) = o.u16(d, off) else { return (out, 0) };
    let n = usize::from(n);
    for i in 0..n {
        let Some(e) = off.checked_add(2 + i * 12) else { return (out, 0) };
        let (Some(tag), Some(typ), Some(count)) =
            (o.u16(d, e), o.u16(d, e.saturating_add(2)), o.u32(d, e.saturating_add(4)))
        else {
            return (out, 0);
        };
        let count = to_usize(count);
        let size = type_size(typ).saturating_mul(count);
        let at = if size <= 4 {
            e + 8
        } else {
            match o.u32(d, e.saturating_add(8)) {
                Some(p) => to_usize(p),
                None => continue,
            }
        };
        if at.checked_add(size).is_some_and(|end| end <= d.len()) {
            out.push(Entry { tag, typ, count, at });
        }
    }
    let next = off.checked_add(2 + n * 12).and_then(|at| o.u32(d, at)).map_or(0, to_usize);
    (out, next)
}

fn text(d: &[u8], e: &Entry) -> String {
    let Some(raw) = d.get(e.at..e.at + e.count.min(256)) else { return String::new() };
    raw.iter()
        .take_while(|&&b| b != 0)
        .map(|&b| if (0x20..0x7F).contains(&b) { char::from(b) } else { '?' })
        .take(MAX_TEXT)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn short(d: &[u8], o: Order, e: &Entry) -> Option<u16> {
    if e.typ == 3 && e.count >= 1 { o.u16(d, e.at) } else { None }
}

fn long(d: &[u8], o: Order, e: &Entry) -> Option<usize> {
    match e.typ {
        // LONG, SLONG and IFD: all of them are used for offsets to a sub-IFD.
        4 | 9 | 13 if e.count >= 1 => o.u32(d, e.at).map(to_usize),
        3 | 8 if e.count >= 1 => o.u16(d, e.at).map(usize::from),
        _ => None,
    }
}

/// Three rationals (degrees, minutes, seconds) as millionths of a degree.
fn dms_micro(d: &[u8], o: Order, e: &Entry) -> Option<i64> {
    if e.typ != 5 || e.count < 3 {
        return None;
    }
    let mut parts = [0i64; 3];
    for (i, part) in parts.iter_mut().enumerate() {
        let num = i64::from(o.u32(d, e.at + i * 8)?);
        let den = i64::from(o.u32(d, e.at + i * 8 + 4)?);
        if den == 0 {
            return None;
        }
        *part = num.checked_mul(1_000_000)? / den;
    }
    Some(parts[0] + parts[1] / 60 + parts[2] / 3600)
}

/// Reads the EXIF block that starts with the TIFF header (`II*\0` or `MM\0*`).
#[must_use]
pub fn parse(tiff: &[u8]) -> Option<ExifFacts> {
    let little = match tiff.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let o = Order { little };
    let ifd0_off = to_usize(o.u32(tiff, 4)?);
    let mut f = ExifFacts { orientation: 1, ..ExifFacts::default() };
    let (ifd0, next) = ifd(tiff, o, ifd0_off);
    let (mut exif_ptr, mut gps_ptr) = (None, None);
    for e in &ifd0 {
        match e.tag {
            0x010E | 0x9C9B..=0x9C9F => f.free_text = true,
            0x010F => f.make = text(tiff, e),
            0x0110 => f.model = text(tiff, e),
            0x0112 => {
                if let Some(v) = short(tiff, o, e).filter(|v| (1..=8).contains(v)) {
                    f.orientation = u8::try_from(v).unwrap_or(1);
                }
            }
            0x0131 => f.software = text(tiff, e),
            0x0132 => f.datetime = text(tiff, e),
            0x013B => f.artist = text(tiff, e),
            0x8298 => f.copyright = text(tiff, e),
            0x8769 => exif_ptr = long(tiff, o, e),
            0x8825 => {
                // The tag alone says that a position was recorded, whatever its pointer holds.
                f.gps = true;
                gps_ptr = long(tiff, o, e);
            }
            _ => {}
        }
    }
    if next != 0 {
        let (ifd1, _) = ifd(tiff, o, next);
        // JPEG thumbnail (0x0201, 0x0202) or a thumbnail stored as TIFF strips (0x0111).
        f.thumbnail = ifd1.iter().any(|e| matches!(e.tag, 0x0111 | 0x0201 | 0x0202));
    }
    if let Some(p) = exif_ptr {
        for e in ifd(tiff, o, p).0 {
            match e.tag {
                0x9286 => f.free_text = true,
                0x9003 if f.datetime.is_empty() => f.datetime = text(tiff, &e),
                0xA430 => f.owner = text(tiff, &e),
                0xA431 | 0xA435 | 0xA420 => f.serial = true,
                0x927C => f.maker_note = true,
                _ => {}
            }
        }
    }
    if let Some(p) = gps_ptr {
        let (gps, _) = ifd(tiff, o, p);
        f.gps |= !gps.is_empty();
        let find = |tag: u16| gps.iter().find(|e| e.tag == tag);
        let sign = |tag: u16, negative: u8| -> i64 {
            find(tag)
                .and_then(|e| tiff.get(e.at).copied())
                .map_or(1, |c| if c.to_ascii_uppercase() == negative { -1 } else { 1 })
        };
        if let (Some(lat), Some(lon)) = (
            find(2).and_then(|e| dms_micro(tiff, o, e)),
            find(4).and_then(|e| dms_micro(tiff, o, e)),
        ) {
            let (lat, lon) = (lat * sign(1, b'S'), lon * sign(3, b'W'));
            if lat.abs() <= 90_000_000 && lon.abs() <= 180_000_000 {
                f.has_position = true;
                f.lat_micro = lat;
                f.lon_micro = lon;
            }
        }
    }
    Some(f)
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "test-only arithmetic on small values"
)]
pub(crate) mod tests {
    use super::*;

    /// One IFD entry for the test builder.
    pub(crate) enum V<'a> {
        Ascii(&'a str),
        Short(u16),
        Long(u32),
        Rationals(&'a [(u32, u32)]),
        Undefined(&'a [u8]),
    }

    /// Builds a little-endian TIFF block: IFD0 with `ifd0`, an Exif IFD, a GPS IFD and (if
    /// `thumb`) an IFD1 pointing at a fake thumbnail.
    pub(crate) fn build(
        ifd0: &[(u16, V<'_>)],
        exif: &[(u16, V<'_>)],
        gps: &[(u16, V<'_>)],
        thumb: bool,
    ) -> Vec<u8> {
        struct B {
            out: Vec<u8>,
        }
        impl B {
            fn put16(&mut self, v: u16) {
                self.out.extend_from_slice(&v.to_le_bytes());
            }
            fn put32(&mut self, v: u32) {
                self.out.extend_from_slice(&v.to_le_bytes());
            }
        }
        // Layout: header (8), then IFDs, each followed by its out-of-line data.
        let mut b = B { out: b"II*\0".to_vec() };
        b.put32(8);
        let mut ptr_fixups: Vec<(usize, usize)> = Vec::new(); // (position of pointer, which ifd)
        let mut write_ifd =
            |b: &mut B, entries: &[(u16, V<'_>)], extra: &[(u16, usize)]| -> usize {
                let start = b.out.len();
                let n = entries.len() + extra.len();
                b.put16(n as u16);
                let table = b.out.len();
                b.out.resize(table + n * 12 + 4, 0);
                let mut data: Vec<u8> = Vec::new();
                let data_base = table + n * 12 + 4;
                let mut slot = table;
                let set = |b: &mut B, slot: usize, tag: u16, typ: u16, count: u32, val: &[u8]| {
                    let mut e = Vec::new();
                    e.extend_from_slice(&tag.to_le_bytes());
                    e.extend_from_slice(&typ.to_le_bytes());
                    e.extend_from_slice(&count.to_le_bytes());
                    e.extend_from_slice(val);
                    b.out[slot..slot + 12].copy_from_slice(&e);
                };
                for (tag, v) in entries {
                    let (typ, count, bytes): (u16, u32, Vec<u8>) = match v {
                        V::Ascii(s) => (2, s.len() as u32 + 1, [s.as_bytes(), &[0]].concat()),
                        V::Short(x) => (3, 1, x.to_le_bytes().to_vec()),
                        V::Long(x) => (4, 1, x.to_le_bytes().to_vec()),
                        V::Rationals(r) => (
                            5,
                            r.len() as u32,
                            r.iter()
                                .flat_map(|(a, d)| [a.to_le_bytes(), d.to_le_bytes()].concat())
                                .collect(),
                        ),
                        V::Undefined(u) => (7, u.len() as u32, u.to_vec()),
                    };
                    if bytes.len() <= 4 {
                        let mut val = bytes.clone();
                        val.resize(4, 0);
                        set(b, slot, *tag, typ, count, &val);
                    } else {
                        let off = (data_base + data.len()) as u32;
                        set(b, slot, *tag, typ, count, &off.to_le_bytes());
                        data.extend_from_slice(&bytes);
                        if data.len() % 2 == 1 {
                            data.push(0);
                        }
                    }
                    slot += 12;
                }
                for (tag, which) in extra {
                    set(b, slot, *tag, 4, 1, &[0; 4]);
                    ptr_fixups.push((slot + 8, *which));
                    slot += 12;
                }
                b.out.extend_from_slice(&data);
                start
            };
        let mut starts = [0usize; 4];
        let mut extra0: Vec<(u16, usize)> = vec![(0x8769, 1)];
        if !gps.is_empty() {
            extra0.push((0x8825, 2));
        }
        starts[0] = write_ifd(&mut b, ifd0, &extra0);
        starts[1] = write_ifd(&mut b, exif, &[]);
        if !gps.is_empty() {
            starts[2] = write_ifd(&mut b, gps, &[]);
        }
        if thumb {
            starts[3] = write_ifd(&mut b, &[(0x0201, V::Long(0)), (0x0202, V::Long(4))], &[]);
            // Link IFD0 -> IFD1: the next-IFD field sits after IFD0's entries.
            let n = ifd0.len() + extra0.len();
            let at = starts[0] + 2 + n * 12;
            b.out[at..at + 4].copy_from_slice(&(starts[3] as u32).to_le_bytes());
        }
        for (pos, which) in ptr_fixups {
            b.out[pos..pos + 4].copy_from_slice(&(starts[which] as u32).to_le_bytes());
        }
        b.out
    }

    #[test]
    fn reads_the_facts_that_identify_the_camera_and_the_owner() {
        let t = build(
            &[
                (0x010F, V::Ascii("Acme")),
                (0x0110, V::Ascii("Shot 9000")),
                (0x0112, V::Short(6)),
                (0x0131, V::Ascii("Editor 1.0")),
                (0x013B, V::Ascii("Ann Author")),
            ],
            &[
                (0x9003, V::Ascii("2026:10:23 12:00:00")),
                (0xA431, V::Ascii("SN123")),
                (0x927C, V::Undefined(&[1, 2, 3, 4, 5, 6])),
            ],
            &[],
            true,
        );
        let f = parse(&t).unwrap();
        assert_eq!(
            (f.make.as_str(), f.model.as_str(), f.software.as_str(), f.artist.as_str()),
            ("Acme", "Shot 9000", "Editor 1.0", "Ann Author")
        );
        assert_eq!(f.orientation, 6);
        assert_eq!(f.datetime, "2026:10:23 12:00:00");
        assert!(f.serial && f.maker_note && f.thumbnail && !f.gps);
        assert!(f.is_private());
    }

    #[test]
    fn reads_a_gps_position() {
        let t = build(
            &[(0x010F, V::Ascii("Acme"))],
            &[],
            &[
                (1, V::Ascii("N")),
                (2, V::Rationals(&[(55, 1), (45, 1), (30, 1)])),
                (3, V::Ascii("W")),
                (4, V::Rationals(&[(37, 1), (36, 1), (0, 1)])),
            ],
            false,
        );
        let f = parse(&t).unwrap();
        assert!(f.gps && f.has_position);
        assert_eq!(f.lat_micro, 55_758_333);
        assert_eq!(f.lon_micro, -37_600_000);
    }

    #[test]
    fn nonsense_never_panics_and_is_bounded() {
        assert_eq!(parse(b"II*\0"), None);
        assert_eq!(parse(b"XX*\0\x08\0\0\0"), None);
        let mut x = 0x2545_F491u32;
        let base = build(
            &[(0x010F, V::Ascii("Acme"))],
            &[(0x9003, V::Ascii("x"))],
            &[(2, V::Rationals(&[(1, 0), (0, 0), (0, 0)]))],
            true,
        );
        for _ in 0..3000 {
            let mut t = base.clone();
            for _ in 0..4 {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let i = (x >> 8) as usize % t.len();
                t[i] = (x >> 24) as u8;
            }
            let cut = (x >> 4) as usize % (t.len() + 1);
            let _ = parse(&t[..cut]);
        }
    }

    #[test]
    fn text_is_plain_printable_ascii() {
        let t = build(&[(0x010F, V::Ascii("A\u{202e}B\x01C"))], &[], &[], false);
        let f = parse(&t).unwrap();
        assert!(f.make.chars().all(|c| c.is_ascii_graphic() || c == ' '));
    }

    /// A little-endian TIFF block whose IFD0 holds `entries` (tag, type, count, inline value), then
    /// a GPS IFD with one entry and, if asked, an IFD1 with one entry of the given tag.
    fn raw_tiff(entries: &[(u16, u16, u32, [u8; 4])], ifd1_tag: Option<u16>) -> Vec<u8> {
        let gps_at = 8 + 2 + 12 * entries.len() + 4;
        let ifd1_at = gps_at + 2 + 12 + 4;
        let mut t = b"II*\0".to_vec();
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, typ, count, value) in entries {
            t.extend_from_slice(&tag.to_le_bytes());
            t.extend_from_slice(&typ.to_le_bytes());
            t.extend_from_slice(&count.to_le_bytes());
            t.extend_from_slice(value);
        }
        let next = if ifd1_tag.is_some() { ifd1_at as u32 } else { 0 };
        t.extend_from_slice(&next.to_le_bytes());
        // The GPS IFD: GPSLatitudeRef = "N".
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend_from_slice(&[1, 0, 2, 0, 2, 0, 0, 0, b'N', 0, 0, 0]);
        t.extend_from_slice(&[0; 4]);
        if let Some(tag) = ifd1_tag {
            t.extend_from_slice(&1u16.to_le_bytes());
            t.extend_from_slice(&tag.to_le_bytes());
            t.extend_from_slice(&[4, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
            t.extend_from_slice(&[0; 4]);
        }
        t
    }

    fn gps_pointer(entries_total: usize, typ: u16) -> (u16, u16, u32, [u8; 4]) {
        let at = (8 + 2 + 12 * entries_total + 4) as u32;
        (0x8825, typ, 1, at.to_le_bytes())
    }

    #[test]
    fn a_gps_pointer_behind_a_run_of_filler_entries_is_found() {
        let filler = (0x0100u16, 3u16, 1u32, [1u8, 0, 0, 0]);
        for fillers in [0usize, 100, 511, 512, 1000, 65_000] {
            let mut entries = vec![filler; fillers];
            entries.push(gps_pointer(fillers + 1, 4));
            let f = parse(&raw_tiff(&entries, None)).unwrap();
            assert!(f.gps, "{fillers} entries before the pointer");
        }
    }

    #[test]
    fn a_gps_pointer_of_any_type_is_a_gps_block() {
        // IFD (13) and SLONG (9) are offset types some writers use; with a type nobody can
        // follow, the tag still says that a position was recorded.
        for typ in [3u16, 4, 9, 13, 2, 7] {
            let f = parse(&raw_tiff(&[gps_pointer(1, typ)], None)).unwrap();
            assert!(f.gps, "type {typ}");
        }
        let f = parse(&raw_tiff(&[gps_pointer(1, 13)], None)).unwrap();
        assert!(!f.has_position && f.is_private());
    }

    #[test]
    fn free_text_and_strip_thumbnails_are_private() {
        for tag in [0x010Eu16, 0x9C9B, 0x9C9C, 0x9C9D, 0x9C9E, 0x9C9F] {
            let f = parse(&raw_tiff(&[(tag, 1, 1, [0; 4])], None)).unwrap();
            assert!(f.free_text && f.is_private(), "tag {tag:#x}");
        }
        let f = parse(&raw_tiff(&[(0x0100, 3, 1, [1, 0, 0, 0])], None)).unwrap();
        assert!(!f.free_text && !f.thumbnail && !f.is_private());
        for tag in [0x0111u16, 0x0201, 0x0202] {
            let f = parse(&raw_tiff(&[(0x0100, 3, 1, [1, 0, 0, 0])], Some(tag))).unwrap();
            assert!(f.thumbnail, "IFD1 tag {tag:#x}");
        }
        let t = build(&[], &[(0x9286, V::Undefined(b"ASCII\0\0\0secret"))], &[], false);
        assert!(parse(&t).unwrap().free_text);
    }

    #[test]
    fn blocks_are_merged_and_the_first_decides_the_orientation() {
        let a = parse(&build(&[(0x0112, V::Short(6)), (0x010F, V::Ascii("A"))], &[], &[], false));
        let b = parse(&build(
            &[(0x0112, V::Short(3)), (0x0110, V::Ascii("B"))],
            &[(0xA431, V::Ascii("SN"))],
            &[(1, V::Ascii("N"))],
            true,
        ));
        let mut slot = None;
        merge_into(&mut slot, a.unwrap());
        merge_into(&mut slot, b.unwrap());
        let f = slot.unwrap();
        assert_eq!((f.make.as_str(), f.model.as_str(), f.orientation), ("A", "B", 6));
        assert!(f.serial && f.gps && f.thumbnail);
    }
}
