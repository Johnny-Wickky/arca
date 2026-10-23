//! Synthetic picture files for the corpus: headers are real, pixel data is not (the X-ray only
//! reads headers; the pixels are decoded elsewhere).

/// An IFD entry for [`tiff`]: tag, type, count, value bytes.
pub(super) struct E(pub u16, pub u16, pub u32, pub Vec<u8>);

pub(super) fn ascii(tag: u16, s: &str) -> E {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    E(tag, 2, u32::try_from(v.len()).unwrap(), v)
}

pub(super) fn rationals(tag: u16, r: &[(u32, u32)]) -> E {
    let v: Vec<u8> =
        r.iter().flat_map(|(a, b)| [a.to_le_bytes(), b.to_le_bytes()].concat()).collect();
    E(tag, 5, u32::try_from(r.len()).unwrap(), v)
}

/// A little-endian TIFF block (the body of an EXIF segment): IFD0 with a pointer to an Exif IFD
/// (if `exif` is not empty) and to a GPS IFD (if `gps` is not empty), and optionally an IFD1
/// that says a thumbnail is present.
pub(super) fn tiff(ifd0: Vec<E>, exif: Vec<E>, gps: Vec<E>, thumbnail: bool) -> Vec<u8> {
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    let mut fixups: Vec<(usize, usize)> = Vec::new();
    let mut starts = [0usize; 4];
    let mut write = |out: &mut Vec<u8>, entries: Vec<E>, pointers: &[(u16, usize)], slot: usize| {
        starts[slot] = out.len();
        let n = entries.len() + pointers.len();
        out.extend_from_slice(&u16::try_from(n).unwrap().to_le_bytes());
        let table = out.len();
        out.resize(table + n * 12 + 4, 0);
        let mut data: Vec<u8> = Vec::new();
        let data_base = table + n * 12 + 4;
        let mut at = table;
        for E(tag, typ, count, bytes) in entries {
            out[at..at + 2].copy_from_slice(&tag.to_le_bytes());
            out[at + 2..at + 4].copy_from_slice(&typ.to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&count.to_le_bytes());
            if bytes.len() <= 4 {
                out[at + 8..at + 8 + bytes.len()].copy_from_slice(&bytes);
            } else {
                let off = u32::try_from(data_base + data.len()).unwrap();
                out[at + 8..at + 12].copy_from_slice(&off.to_le_bytes());
                data.extend_from_slice(&bytes);
                if data.len() % 2 == 1 {
                    data.push(0);
                }
            }
            at += 12;
        }
        for (tag, which) in pointers {
            out[at..at + 2].copy_from_slice(&tag.to_le_bytes());
            out[at + 2..at + 4].copy_from_slice(&4u16.to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&1u32.to_le_bytes());
            fixups.push((at + 8, *which));
            at += 12;
        }
        out.extend_from_slice(&data);
    };
    let mut pointers = Vec::new();
    if !exif.is_empty() {
        pointers.push((0x8769u16, 1usize));
    }
    if !gps.is_empty() {
        pointers.push((0x8825u16, 2usize));
    }
    let n0 = ifd0.len() + pointers.len();
    write(&mut out, ifd0, &pointers, 0);
    if !exif.is_empty() {
        write(&mut out, exif, &[], 1);
    }
    if !gps.is_empty() {
        write(&mut out, gps, &[], 2);
    }
    if thumbnail {
        let ifd1 = vec![E(0x0201, 4, 1, vec![0; 4]), E(0x0202, 4, 1, 4u32.to_le_bytes().to_vec())];
        write(&mut out, ifd1, &[], 3);
        let at = starts[0] + 2 + n0 * 12;
        out[at..at + 4].copy_from_slice(&u32::try_from(starts[3]).unwrap().to_le_bytes());
    }
    for (pos, which) in fixups {
        out[pos..pos + 4].copy_from_slice(&u32::try_from(starts[which]).unwrap().to_le_bytes());
    }
    out
}

/// A JPEG with the given application/comment segments `(marker, body)` and a one-block frame.
pub(super) fn jpeg(segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    for (marker, body) in segments {
        out.extend_from_slice(&[0xFF, *marker]);
        out.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_be_bytes());
        out.extend_from_slice(body);
    }
    // SOF0: 8 bit, 8 x 8, one component.
    out.extend_from_slice(&[
        0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11, 0x00,
    ]);
    // SOS and a little entropy-coded data.
    out.extend_from_slice(&[
        0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x12, 0x34,
    ]);
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// An EXIF segment body (`Exif\0\0` and the TIFF block).
pub(super) fn exif_segment(tiff: Vec<u8>) -> (u8, Vec<u8>) {
    let mut body = b"Exif\0\0".to_vec();
    body.extend_from_slice(&tiff);
    (0xE1, body)
}

/// A GIF with `frames` identical 1 x 1 frames.
pub(super) fn gif(frames: usize) -> Vec<u8> {
    let mut out = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xFF\xFF\xFF".to_vec();
    for _ in 0..frames {
        out.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x02, 0x02, 0x44, 0x01, 0x00]);
    }
    out.push(0x3B);
    out
}

/// A PNG whose header claims `w` x `h` pixels and that has almost no image data.
pub(super) fn png_header(w: u32, h: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc = arca_digest::Crc32::new();
        crc.update(kind);
        crc.update(body);
        out.extend_from_slice(&crc.finalize().to_be_bytes());
    }
    let mut out = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
    chunk(&mut out, b"IEND", &[]);
    out
}
