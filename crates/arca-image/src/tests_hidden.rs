#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "test-only arithmetic on small values"
)]

//! Hostile pictures: every byte of a file that is not picture data, structure or recognised
//! metadata has to show up as `unaccounted`, and none may show up in an honest file.

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::*;
use crate::exif::tests::{V, build};
use crate::testkit::{PngSpec, chunk, png};
use crate::testkit_img::{GifSpec, bmp, gif};

fn lim() -> Limits {
    Limits::default()
}

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// What `inspect` and `decode` say about the file; they must agree on `unaccounted`.
fn unaccounted(data: &[u8]) -> u64 {
    let f = inspect(data, &lim()).unwrap();
    let d = decode(data, &lim()).unwrap().facts;
    assert_eq!(f.unaccounted, d.unaccounted, "inspect and decode count the same bytes");
    f.unaccounted
}

// ---- PNG -----------------------------------------------------------------------------------

type Chunks = Vec<([u8; 4], Vec<u8>)>;

/// The chunks of a PNG file.
fn chunks_of(png: &[u8]) -> Chunks {
    let mut out = Vec::new();
    let mut at = 8;
    while at + 12 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = png[at + 4..at + 8].try_into().unwrap();
        out.push((kind, png[at + 8..at + 8 + len].to_vec()));
        at += 12 + len;
    }
    out
}

/// A PNG file from chunks (with correct checksums) and bytes after the last one.
fn assemble(chunks: &Chunks, after: &[u8]) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    for (kind, body) in chunks {
        chunk(&mut out, kind, body);
    }
    out.extend_from_slice(after);
    out
}

/// The chunks of an honest 2 × 2 RGB picture.
fn honest() -> Chunks {
    let raw = [0, 1, 2, 3, 4, 5, 6, 0, 7, 8, 9, 10, 11, 12];
    let mut ihdr = 2u32.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    vec![(*b"IHDR", ihdr), (*b"IDAT", compress_to_vec_zlib(&raw, 6)), (*b"IEND", Vec::new())]
}

fn at_kind(chunks: &Chunks, kind: &[u8; 4]) -> usize {
    chunks.iter().position(|(k, _)| k == kind).unwrap()
}

#[test]
fn an_honest_png_has_nothing_unaccounted() {
    assert_eq!(unaccounted(&assemble(&honest(), &[])), 0);
    for (w, h, depth, color, interlace) in
        [(37, 23, 8, 2, false), (37, 23, 8, 6, true), (16, 9, 16, 0, true), (5, 5, 1, 0, false)]
    {
        let spec = PngSpec { w, h, depth, color, interlace, ..PngSpec::default() };
        let ch = match color {
            0 => 1,
            4 => 2,
            2 => 3,
            _ => 4,
        };
        let data = png(&spec, &|x, y| vec![((x + y) % 2) as u16; ch]);
        assert_eq!(unaccounted(&data), 0, "{w}x{h} depth {depth} colour {color} {interlace}");
    }
}

#[test]
fn png_private_chunks_are_unaccounted_whatever_they_are_called() {
    let mut zip = b"PK\x03\x04".to_vec();
    zip.resize(40, 0);
    for kind in [*b"prVt", *b"zIPp", *b"IDAt", *b"iDOT"] {
        let mut c = honest();
        c.insert(1, (kind, zip.clone()));
        // The chunk's own length, type and checksum count too: 12 bytes around 40.
        assert_eq!(unaccounted(&assemble(&c, &[])), 52, "{kind:?}");
    }
    // Before IHDR it is no PNG at all; after IEND it is trailing data, not a chunk.
    let mut c = honest();
    let n = c.len();
    c.insert(n - 1, (*b"ZIPz", zip.clone()));
    assert_eq!(unaccounted(&assemble(&c, &[])), 52);
}

#[test]
fn png_known_chunks_may_not_carry_more_than_the_format_gives_them() {
    // gAMA is four bytes; a longer one hides the rest. tRNS on an RGB picture is six.
    for (kind, body, hidden) in [
        (*b"gAMA", vec![0u8; 4 + 30], 30u64),
        (*b"pHYs", vec![0u8; 9 + 5], 5),
        (*b"tIME", vec![0u8; 7 + 100], 100),
        (*b"tRNS", vec![0u8; 6 + 11], 11),
        (*b"sRGB", vec![0u8; 1], 0),
    ] {
        let mut c = honest();
        c.insert(1, (kind, body));
        assert_eq!(unaccounted(&assemble(&c, &[])), hidden, "{kind:?}");
    }
    // An IEND with a body.
    let mut c = honest();
    let n = c.len();
    c[n - 1].1 = vec![0x41; 7];
    assert_eq!(unaccounted(&assemble(&c, &[])), 7);
    // A second header, and a PLTE of a length no palette has.
    let mut c = honest();
    let ihdr = c[0].clone();
    c.insert(1, ihdr);
    assert_eq!(unaccounted(&assemble(&c, &[])), 13 + 12);
    let mut c = honest();
    c.insert(1, (*b"PLTE", vec![1, 2, 3, 4]));
    assert_eq!(unaccounted(&assemble(&c, &[])), 4);
}

#[test]
fn png_data_after_the_end_of_the_zlib_stream_is_unaccounted() {
    // Bytes in the IDAT chunk after the zlib stream (and its checksum).
    let mut c = honest();
    let i = at_kind(&c, b"IDAT");
    c[i].1.extend_from_slice(b"PK\x03\x04 nine");
    assert_eq!(unaccounted(&assemble(&c, &[])), 9);
    // Inflated data after the last row: the picture is 2 × 2, the stream holds more.
    let mut raw = vec![0, 1, 2, 3, 4, 5, 6, 0, 7, 8, 9, 10, 11, 12];
    raw.extend(std::iter::repeat_n(0x5A, 5000));
    let mut c = honest();
    let i = at_kind(&c, b"IDAT");
    c[i].1 = compress_to_vec_zlib(&raw, 6);
    assert_eq!(unaccounted(&assemble(&c, &[])), 5000);
    // A second IDAT after another chunk is ignored by every decoder, so it carries nothing.
    let mut c = honest();
    c.insert(2, (*b"tEXt", b"k\0v".to_vec()));
    c.insert(3, (*b"IDAT", vec![0x41; 20]));
    let data = assemble(&c, &[]);
    let d = decode(&data, &lim()).unwrap();
    assert_eq!(d.facts.unaccounted, 20);
    assert!(d.facts.has(flag::DAMAGED));
}

#[test]
fn png_idat_split_into_one_byte_chunks_is_the_same_picture() {
    let spec = PngSpec { w: 37, h: 23, depth: 8, color: 2, ..PngSpec::default() };
    let whole = png(&spec, &|x, y| vec![(x * 7 % 256) as u16, (y * 11 % 256) as u16, 3]);
    let mut c = chunks_of(&whole);
    let i = at_kind(&c, b"IDAT");
    let body = c.remove(i).1;
    for (k, byte) in body.iter().enumerate() {
        c.insert(i + k, (*b"IDAT", vec![*byte]));
    }
    let split = assemble(&c, &[]);
    assert_eq!(unaccounted(&split), 0);
    assert!(decode(&split, &lim()).unwrap().rgba == decode(&whole, &lim()).unwrap().rgba);
}

#[test]
fn no_fixture_has_bytes_nobody_can_explain() {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if name.ends_with(".jpg") || name.ends_with(".png") {
            assert_eq!(unaccounted(&fixture(&name)), 0, "{name}");
            seen += 1;
        }
    }
    assert!(seen > 40, "{seen} fixtures");
}

// ---- JPEG ----------------------------------------------------------------------------------

fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, marker];
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// `base` with `extra` inserted after its first segment.
fn after_first_segment(base: &[u8], extra: &[u8]) -> Vec<u8> {
    let at = 4 + usize::from(u16::from_be_bytes([base[4], base[5]]));
    [&base[..at], extra, &base[at..]].concat()
}

#[test]
fn jpeg_private_markers_carrying_a_zip_are_unaccounted() {
    let base = fixture("img-444.jpg");
    let mut zip = b"PK\x03\x04".to_vec();
    zip.resize(300, 7);
    // JPG13 (FFFD) and a reserved marker; their length field is not part of the payload.
    for marker in [0xFD, 0xF0, 0xC8, 0x42] {
        let data = after_first_segment(&base, &segment(marker, &zip));
        assert_eq!(unaccounted(&data), 300, "marker {marker:#x}");
    }
    // An application segment of a kind nobody knows.
    let data = after_first_segment(&base, &segment(0xE5, &zip));
    assert_eq!(unaccounted(&data), 300);
}

#[test]
fn jpeg_headers_longer_than_their_format_are_unaccounted() {
    let base = fixture("img-444.jpg");
    // A JFIF header is 14 bytes (16 with the length field); the rest is not a thumbnail.
    let mut jfif = b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0".to_vec();
    assert_eq!(jfif.len(), 14);
    jfif.extend(std::iter::repeat_n(0x41, 500));
    let data = after_first_segment(&base, &segment(0xE0, &jfif));
    assert_eq!(unaccounted(&data), 500);
    // With a declared thumbnail (2 × 1 pixels = 6 bytes) exactly that much is the thumbnail's.
    let mut jfif = b"JFIF\0\x01\x01\0\0\x01\0\x01\x02\x01".to_vec();
    jfif.extend_from_slice(&[9; 6]);
    let data = after_first_segment(&base, &segment(0xE0, &jfif));
    assert_eq!(unaccounted(&data), 0);
    assert_eq!(inspect(&data, &lim()).unwrap().metadata_bytes, 6);
    jfif.extend_from_slice(&[1, 2, 3]);
    assert_eq!(unaccounted(&after_first_segment(&base, &segment(0xE0, &jfif))), 3);
    // An Adobe header is 12 bytes (14 with the length field).
    let mut adobe = b"Adobe\0\x64\0\0\0\0\x01".to_vec();
    assert_eq!(adobe.len(), 12);
    assert_eq!(unaccounted(&after_first_segment(&base, &segment(0xEE, &adobe))), 0);
    adobe.extend(std::iter::repeat_n(0x42, 40));
    assert_eq!(unaccounted(&after_first_segment(&base, &segment(0xEE, &adobe))), 40);
}

#[test]
fn jpeg_bytes_between_segments_and_after_the_last_one_are_unaccounted() {
    let base = fixture("img-444.jpg");
    let garbage = [0x41u8; 13];
    // Between two segments.
    assert_eq!(unaccounted(&after_first_segment(&base, &garbage)), 13);
    // Fill bytes (FF FF) are padding the format allows.
    assert_eq!(unaccounted(&after_first_segment(&base, &[0xFF, 0xFF, 0xFF])), 0);
    // No end-of-image marker: a comment, then bytes that no marker follows.
    let mut cut = base[..base.len() - 2].to_vec();
    cut.extend_from_slice(&segment(0xFE, b"hi"));
    cut.extend_from_slice(&[0x50u8; 20]);
    let f = inspect(&cut, &lim()).unwrap();
    assert_eq!(f.unaccounted, 20);
    assert!(f.has(flag::DAMAGED));
    // A stray start-of-image marker is two bytes the format has no use for.
    assert_eq!(unaccounted(&after_first_segment(&base, &[0xFF, 0xD8])), 2);
    // Segments longer than the format defines: SOF, SOS and DRI.
    let sof = base.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    let mut fat = base.clone();
    let len = u16::from_be_bytes([fat[sof + 2], fat[sof + 3]]) as usize;
    fat.splice(sof + 2 + len..sof + 2 + len, [0u8; 5]);
    fat[sof + 2..sof + 4].copy_from_slice(&((len + 5) as u16).to_be_bytes());
    assert_eq!(unaccounted(&fat), 5);
    let dri = segment(0xDD, &[0, 0, 1, 2, 3]);
    assert_eq!(unaccounted(&after_first_segment(&base, &dri)), 3);
}

#[test]
fn jpeg_metadata_kinds_that_are_recognised_are_not_unaccounted() {
    let base = fixture("img-444.jpg");
    let mut photoshop = b"Photoshop 3.0\0".to_vec();
    photoshop.extend_from_slice(&[0; 200]);
    let data = after_first_segment(&base, &segment(0xED, &photoshop));
    let f = inspect(&data, &lim()).unwrap();
    assert_eq!((f.unaccounted, f.metadata_bytes), (0, 214));
}

#[test]
fn a_second_exif_block_is_read_too() {
    let base = fixture("img-444.jpg");
    let clean = build(&[(0x010F, V::Ascii("Acme"))], &[], &[], false);
    let gps = build(
        &[(0x0112, V::Short(6))],
        &[],
        &[
            (1, V::Ascii("N")),
            (2, V::Rationals(&[(10, 1), (0, 1), (0, 1)])),
            (3, V::Ascii("E")),
            (4, V::Rationals(&[(20, 1), (0, 1), (0, 1)])),
        ],
        true,
    );
    let seg = |t: &[u8]| segment(0xE1, &[b"Exif\0\0".as_slice(), t].concat());
    let data = after_first_segment(&base, &[seg(&clean), seg(&gps)].concat());
    let e = inspect(&data, &lim()).unwrap().exif.unwrap();
    assert_eq!(e.make, "Acme");
    assert!(e.gps && e.has_position && e.thumbnail, "{e:?}");
    assert_eq!(e.orientation, 1, "the first block decides how the picture is turned");
    assert_eq!((e.lat_micro, e.lon_micro), (10_000_000, 20_000_000));
}

/// The positions of the markers of a JPEG file, entropy-coded data skipped.
fn markers(d: &[u8]) -> Vec<(usize, u8)> {
    let mut out = Vec::new();
    let mut p = 2;
    while p + 4 <= d.len() {
        let m = d[p + 1];
        out.push((p, m));
        if m == 0xD9 {
            break;
        }
        p += 2 + usize::from(u16::from_be_bytes([d[p + 2], d[p + 3]]));
        if m == 0xDA {
            while p + 1 < d.len() && !(d[p] == 0xFF && !matches!(d[p + 1], 0 | 0xD0..=0xD7)) {
                p += 1;
            }
        }
    }
    out
}

#[test]
fn a_progressive_jpeg_of_thousands_of_scans_is_refused() {
    let base = fixture("img-444-prog.jpg");
    let sos: Vec<usize> = markers(&base).iter().filter(|m| m.1 == 0xDA).map(|m| m.0).collect();
    // The first scan (DC of all components) and what follows it up to the next scan, repeated.
    let unit = &base[sos[0]..sos[1]];
    for (repeats, cheap_inspect) in [(5000usize, false), (150, true)] {
        let mut data = base[..sos[0]].to_vec();
        for _ in 0..repeats {
            data.extend_from_slice(unit);
        }
        data.extend_from_slice(&[0xFF, 0xD9]);
        // Every scan is walked again over the whole picture; the header-only reading does not
        // decode them, but counts them.
        assert_eq!(inspect(&data, &lim()).is_ok(), cheap_inspect, "{repeats} scans");
        assert!(
            matches!(decode(&data, &lim()), Err(DecodeError::TooLarge(_))),
            "{repeats} scans: {:?}",
            decode(&data, &lim()).map(|i| i.facts)
        );
    }
    // The honest file has a handful.
    assert!(decode(&base, &lim()).is_ok());
}

// ---- GIF -----------------------------------------------------------------------------------

/// A GIF header: `w × h` canvas, a global table of two colours.
fn gif_header(w: u16, h: u16) -> Vec<u8> {
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&[0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
    out
}

/// A 1 × 1 frame whose single pixel is index 0.
const FRAME_1X1: [u8; 15] = [0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x02, 0x02, 0x44, 0x01, 0x00];

#[test]
fn a_gif_frame_far_larger_than_its_canvas_is_refused_before_anything_is_reserved() {
    let mut data = gif_header(1, 1);
    data.extend_from_slice(&[0x2C, 0, 0, 0, 0]);
    data.extend_from_slice(&15_000u16.to_le_bytes());
    data.extend_from_slice(&15_000u16.to_le_bytes());
    data.extend_from_slice(&[0, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3B]);
    assert!(matches!(inspect(&data, &lim()), Err(DecodeError::TooLarge(_))));
    assert!(matches!(decode(&data, &lim()), Err(DecodeError::TooLarge(_))));
}

#[test]
fn gif_extensions_nobody_reads_are_unaccounted() {
    let with = |ext: &[u8]| {
        let mut d = gif_header(1, 1);
        d.extend_from_slice(ext);
        d.extend_from_slice(&FRAME_1X1);
        d.push(0x3B);
        d
    };
    assert_eq!(unaccounted(&with(&[])), 0);
    // An application extension that is no animation, comment, profile or XMP packet.
    let mut app = vec![0x21, 0xFF, 11];
    app.extend_from_slice(b"ARCAHIDE1.0");
    app.extend_from_slice(&[3, b'P', b'K', 3, 0]);
    assert_eq!(unaccounted(&with(&app)), 14);
    // The same framing with the name of the animation extension is the loop count, nothing more.
    let mut netscape = vec![0x21, 0xFF, 11];
    netscape.extend_from_slice(b"NETSCAPE2.0");
    netscape.extend_from_slice(&[3, 1, 0, 0, 0]);
    assert_eq!(unaccounted(&with(&netscape)), 0);
    let mut fat = vec![0x21, 0xFF, 11];
    fat.extend_from_slice(b"NETSCAPE2.0");
    fat.extend_from_slice(&[3, 1, 0, 0, 10]);
    fat.extend_from_slice(&[0x41; 10]);
    fat.push(0);
    assert_eq!(unaccounted(&with(&fat)), 10);
    // Plain text (12 bytes of grid, then the text), and an extension with an unknown label.
    let mut text = vec![0x21, 0x01, 12];
    text.extend_from_slice(&[0; 12]);
    text.extend_from_slice(&[4, b'h', b'i', b'!', b'!', 0]);
    assert_eq!(unaccounted(&with(&text)), 16);
    assert_eq!(unaccounted(&with(&[0x21, 0x77, 2, 9, 9, 0])), 2);
    // A graphic control block is four bytes.
    assert_eq!(unaccounted(&with(&[0x21, 0xF9, 4, 0, 0, 0, 0, 0])), 0);
    assert_eq!(unaccounted(&with(&[0x21, 0xF9, 6, 0, 0, 0, 0, 7, 7, 0])), 2);
}

#[test]
fn gif_with_a_thousand_frames_still_shows_what_follows_it() {
    let spec = GifSpec {
        w: 2,
        h: 2,
        palette: vec![[0, 0, 0], [255, 255, 255]],
        extra_frames: 1100,
        trailing: b"PK\x03\x04appended".to_vec(),
        ..GifSpec::default()
    };
    let data = gif(&spec, &|_, _| 0);
    let f = inspect(&data, &lim()).unwrap();
    assert_eq!((f.frames, f.trailing), (1000, 12));
    assert!(!f.has(flag::DAMAGED));
    assert_eq!(f.unaccounted, 0);
}

#[test]
fn gif_bytes_after_the_end_of_the_lzw_data_are_unaccounted() {
    let palette = vec![[0, 0, 0], [255, 255, 255], [9, 9, 9], [7, 7, 7]];
    let honest = GifSpec { w: 9, h: 7, palette: palette.clone(), ..GifSpec::default() };
    let f = |x: usize, y: usize| ((x + y) % 4) as u8;
    assert_eq!(unaccounted(&gif(&honest, &f)), 0);
    let extra = b"PK\x03\x04 hidden in the image data".to_vec();
    let hiding = GifSpec { lzw_extra: extra.clone(), ..honest };
    assert_eq!(unaccounted(&gif(&hiding, &f)), extra.len() as u64);
}

// ---- BMP -----------------------------------------------------------------------------------

fn palette2() -> Vec<[u8; 3]> {
    vec![[0, 0, 0], [255, 255, 255]]
}

/// An RLE8 bitmap of `w × h` with the given codes as its pixel data, and bytes after them.
fn rle8(w: i32, h: i32, codes: &[u8], after: &[u8]) -> Vec<u8> {
    let mut d = bmp(w, h, 8, 1, &palette2(), None, &[]);
    d.extend_from_slice(codes);
    d.extend_from_slice(after);
    d
}

#[test]
fn bmp_rle_reports_what_trails_the_codes_and_what_draws_nothing() {
    let codes = [4, 1, 0, 0, 4, 0, 0, 1];
    let data = rle8(4, 2, &codes, &[]);
    let f = inspect(&data, &lim()).unwrap();
    assert_eq!((f.trailing, f.unaccounted), (0, 0));
    assert!(!f.has(flag::DAMAGED));
    // A ZIP behind the end-of-bitmap code.
    let data = rle8(4, 2, &codes, b"PK\x03\x04zip");
    assert_eq!(unaccounted(&data), 0);
    assert_eq!(inspect(&data, &lim()).unwrap().trailing, 7);
    assert_eq!(decode(&data, &lim()).unwrap().facts.trailing, 7);
    // Runs that start outside the bitmap draw nothing: here 100 pairs of `FF 41` after the
    // two rows, and one before the end of its row.
    let mut hidden = vec![4, 1, 0, 0, 4, 0, 0, 0];
    for _ in 0..100 {
        hidden.extend_from_slice(&[0xFF, 0x41]);
    }
    hidden.extend_from_slice(&[0, 1]);
    assert_eq!(unaccounted(&rle8(4, 2, &hidden, &[])), 200);
    let beyond_the_edge = [0xFF, 0x41, 0xFF, 0x41, 0, 0, 4, 0, 0, 1];
    assert_eq!(
        unaccounted(&rle8(4, 2, &beyond_the_edge, &[])),
        2,
        "the second run starts at x=255"
    );
}

#[test]
fn bmp_rle_runs_past_the_edge_cost_no_more_than_the_codes() {
    // A million `FF 41` pairs on the first row: the first is drawn (cut at the edge), the others
    // start far outside. The work is that of the codes, not 255 pixels per pair.
    let mut codes = Vec::new();
    for _ in 0..1_000_000 {
        codes.extend_from_slice(&[0xFF, 0x41]);
    }
    codes.extend_from_slice(&[0, 1]);
    let data = rle8(8, 8, &codes, &[]);
    let img = decode(&data, &lim()).unwrap();
    assert_eq!(img.facts.unaccounted, 1_999_998);
}

#[test]
fn bmp_gaps_reserved_words_and_row_padding_are_unaccounted() {
    let row = vec![1, 2, 3, 4, 5, 6];
    let base = bmp(2, 1, 24, 0, &[], None, &[row]);
    assert_eq!(unaccounted(&base), 0);
    // Ten bytes between the headers and the pixels.
    let mut gap = base.clone();
    gap.splice(54..54, [0x55u8; 10]);
    gap[10..14].copy_from_slice(&64u32.to_le_bytes());
    assert_eq!(unaccounted(&gap), 10);
    // The reserved words of the file header.
    let mut reserved = base.clone();
    reserved[6..10].copy_from_slice(&[0, 9, 0, 9]);
    assert_eq!(unaccounted(&reserved), 2);
    // Row padding (two bytes after 6 bytes of pixels) that is not zero.
    let mut padded = base.clone();
    padded[54 + 6] = 0x41;
    padded[54 + 7] = 0x42;
    assert_eq!(unaccounted(&padded), 2);
    // Bytes after the pixels are trailing data, as before.
    let mut tail = base.clone();
    tail.extend_from_slice(b"PK\x03\x04");
    let f = inspect(&tail, &lim()).unwrap();
    assert_eq!((f.trailing, f.unaccounted), (4, 0));
    // A palette bitmap: the colour table is structure.
    let pal = bmp(3, 1, 8, 0, &[[1, 2, 3], [4, 5, 6]], None, &[vec![0, 1, 0]]);
    assert_eq!(unaccounted(&pal), 0);
}

// ---- the wire ------------------------------------------------------------------------------

#[test]
fn unaccounted_bytes_survive_the_wire() {
    let mut c = honest();
    c.insert(1, (*b"prVt", vec![1; 30]));
    let img = decode(&assemble(&c, &[]), &lim()).unwrap();
    assert_eq!(img.facts.unaccounted, 42);
    use arca_abi::wire::{DecodeLimits, from_bytes, to_bytes};
    let bytes = to_bytes(&img).unwrap();
    let limits = DecodeLimits { max_string_bytes: 1 << 20, max_items: 1 << 16, max_depth: 8 };
    let back = from_bytes::<Image>(&bytes, limits).unwrap();
    assert_eq!(back.facts.unaccounted, 42);
    assert_eq!(back, img);
}

// ---- the sterile copy ----------------------------------------------------------------------

fn sterile_copy() -> Vec<u8> {
    let rgba: Vec<u8> = (0..16 * 9 * 4).map(|i| (i * 7 % 251) as u8).collect();
    encode_png(16, 9, &rgba).unwrap().0
}

#[test]
fn the_sterile_check_accepts_what_the_writer_wrote_and_nothing_else() {
    let good = sterile_copy();
    let img = decode_sterile(&good, &lim()).unwrap();
    assert_eq!((img.width, img.height), (16, 9));
    let bad = |data: &[u8]| decode_sterile(data, &lim()).is_err();
    // A byte after IEND.
    assert!(bad(&[good.as_slice(), &[0]].concat()));
    // A text chunk, a private chunk, an extra frame header.
    for kind in [*b"tEXt", *b"prVt", *b"acTL"] {
        let mut c = chunks_of(&good);
        c.insert(1, (kind, vec![0; 8]));
        assert!(bad(&assemble(&c, &[])), "{kind:?}");
    }
    // Bytes behind the zlib stream, and a second IDAT.
    let mut c = chunks_of(&good);
    let i = at_kind(&c, b"IDAT");
    c[i].1.push(0x41);
    assert!(bad(&assemble(&c, &[])));
    // A damaged checksum of a chunk (the last, that of IEND).
    let mut broken = good.clone();
    let last = broken.len() - 1;
    broken[last] ^= 1;
    assert!(bad(&broken));
    // Not a PNG at all.
    assert!(bad(&fixture("img-444.jpg")));
}

#[test]
fn the_sterile_check_verifies_the_zlib_checksum_the_viewer_ignores() {
    let good = sterile_copy();
    let mut c = chunks_of(&good);
    let i = at_kind(&c, b"IDAT");
    let n = c[i].1.len();
    c[i].1[n - 1] ^= 0x55;
    // The chunk's own CRC is recomputed by `assemble`, so only the Adler-32 is wrong.
    let forged = assemble(&c, &[]);
    assert!(decode(&forged, &lim()).is_ok(), "the viewer does not check the Adler-32");
    assert!(matches!(decode_sterile(&forged, &lim()), Err(DecodeError::Malformed(_))));
}
