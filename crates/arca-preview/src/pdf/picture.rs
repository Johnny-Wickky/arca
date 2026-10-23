//! The pictures of a PDF: image XObjects, found by their object number.
//!
//! A picture compressed with `DCTDecode` is a JPEG file inside the stream and goes to the JPEG
//! decoder of `arca-image`. A picture stored as plain samples (usually behind `FlateDecode`) is
//! converted here to RGBA row by row (colour spaces: grey, RGB, CMYK, ICC-based, indexed; soft
//! masks and stencil masks) and reduced while it is produced. JPEG 2000, JBIG2 and CCITT are not
//! read: they are named, not guessed.

use arca_image::{DecodeError, Image, Limits, RowReducer};

use super::file::PdfFile;
use super::filter::FilterError;
use super::lex::{Dict, Obj};
use crate::PreviewPolicy;
use crate::picture::{PictureError, from_preview};

/// Most bytes of decoded samples of one picture (and of its mask).
const MAX_SAMPLES: usize = 96 << 20;
/// Most entries of an indexed palette.
const MAX_PALETTE: usize = 256;

/// What a PDF picture turned out to be.
#[allow(clippy::large_enum_variant, reason = "built once per request and consumed at once")]
pub(crate) enum Found {
    /// A JPEG file (the stream, behind any filters that come before `DCTDecode`).
    Jpeg(Vec<u8>),
    /// Samples that were converted and reduced here.
    Samples(Image),
}

#[derive(Clone)]
enum Space {
    Gray,
    Rgb,
    Cmyk,
    /// Palette as RGB triples.
    Indexed(Vec<[u8; 3]>),
}

impl Space {
    fn components(&self) -> usize {
        match self {
            Self::Gray | Self::Indexed(_) => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
        }
    }
}

fn from_filter(e: FilterError) -> PictureError {
    match e {
        FilterError::Unsupported(n) => {
            PictureError::Unsupported(format!("{n} pictures are not shown"))
        }
        FilterError::Budget => {
            PictureError::Malformed("the picture expands beyond the limit".into())
        }
    }
}

fn from_decode(e: DecodeError) -> PictureError {
    match e {
        DecodeError::Unsupported(s) => PictureError::Unsupported(s),
        DecodeError::Malformed(s) | DecodeError::TooLarge(s) => PictureError::Malformed(s),
    }
}

fn codec_name(codec: &str) -> &str {
    match codec {
        "JPXDecode" => "JPEG 2000",
        "JBIG2Decode" => "JBIG2",
        "CCITTFaxDecode" => "CCITT fax",
        other => other,
    }
}

/// A colour space object, as far as it can be shown.
fn space(pdf: &mut PdfFile<'_>, o: &Obj, depth: u8) -> Option<Space> {
    let o = pdf.resolve(o);
    if let Some(name) = o.as_name() {
        return match name {
            "DeviceGray" | "G" | "CalGray" => Some(Space::Gray),
            "DeviceRGB" | "RGB" | "CalRGB" => Some(Space::Rgb),
            "DeviceCMYK" | "CMYK" => Some(Space::Cmyk),
            _ => None,
        };
    }
    let items: Vec<Obj> = o.as_arr()?.to_vec();
    let kind = pdf.resolve(items.first()?).as_name()?.to_owned();
    match kind.as_str() {
        "CalGray" => Some(Space::Gray),
        "CalRGB" => Some(Space::Rgb),
        "ICCBased" => {
            let stream = pdf.resolve(items.get(1)?);
            let dict = stream.as_dict()?.clone();
            match pdf.get(&dict, "N").as_int()? {
                1 => Some(Space::Gray),
                3 => Some(Space::Rgb),
                4 => Some(Space::Cmyk),
                _ => None,
            }
        }
        "Indexed" | "I" if depth == 0 => {
            let base = space(pdf, items.get(1)?, 1)?;
            let hival =
                usize::try_from(pdf.resolve(items.get(2)?).as_int()?).ok()?.min(MAX_PALETTE - 1);
            let table = pdf.resolve(items.get(3)?);
            let bytes: Vec<u8> = match &table {
                Obj::Str(s) => s.to_vec(),
                Obj::Stream(st) => pdf.stream_data(st, MAX_PALETTE * 4).ok()?.data,
                _ => return None,
            };
            let n = base.components();
            let mut palette = Vec::new();
            for i in 0..=hival {
                let comps = bytes.get(i * n..(i + 1) * n).unwrap_or(&[]);
                let get = |k: usize| comps.get(k).copied().unwrap_or(0);
                palette.push(match &base {
                    Space::Gray | Space::Indexed(_) => [get(0), get(0), get(0)],
                    Space::Rgb => [get(0), get(1), get(2)],
                    Space::Cmyk => cmyk(get(0), get(1), get(2), get(3)),
                });
            }
            Some(Space::Indexed(palette))
        }
        _ => None,
    }
}

/// CMYK (255 = full ink) to RGB.
fn cmyk(c: u8, m: u8, y: u8, k: u8) -> [u8; 3] {
    let f = |v: u8| u8::try_from((u32::from(255 - v) * u32::from(255 - k)) / 255).unwrap_or(0);
    [f(c), f(m), f(y)]
}

/// Sample `i` of a row of `bpc`-bit samples.
fn sample(row: &[u8], bpc: usize, i: usize) -> u32 {
    match bpc {
        8 => u32::from(row.get(i).copied().unwrap_or(0)),
        16 => {
            u32::from(row.get(i * 2).copied().unwrap_or(0)) << 8
                | u32::from(row.get(i * 2 + 1).copied().unwrap_or(0))
        }
        b => {
            let bit = i * b;
            let byte = row.get(bit / 8).copied().unwrap_or(0);
            let shift = 8 - b - (bit % 8);
            (u32::from(byte) >> shift) & ((1u32 << b) - 1)
        }
    }
}

/// A sample as 8 bits, through the `Decode` pair `(d0, d1)` (the identity is `(0, 1)`).
fn level(v: u32, bpc: usize, d: (f32, f32)) -> u8 {
    let max = (1u32 << bpc) - 1;
    if d == (0.0, 1.0) {
        return u8::try_from((v * 255 + max / 2) / max).unwrap_or(255);
    }
    #[allow(clippy::cast_precision_loss, reason = "samples are at most 16 bits")]
    let x = d.0 + (v as f32 / max as f32) * (d.1 - d.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=255 first"
    )]
    {
        (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }
}

/// Decode pairs from the `Decode` array (empty for the default).
fn decode_pairs(pdf: &mut PdfFile<'_>, dict: &Dict, comps: usize) -> Vec<(f32, f32)> {
    let nums = pdf.get_numbers(dict, "Decode");
    if nums.len() == comps * 2 {
        #[allow(clippy::cast_possible_truncation, reason = "Decode values are small")]
        return nums
            .chunks_exact(2)
            .map(|p| (p.first().map_or(0.0, |v| *v as f32), p.get(1).map_or(1.0, |v| *v as f32)))
            .collect();
    }
    vec![(0.0, 1.0); comps]
}

fn dims(pdf: &mut PdfFile<'_>, dict: &Dict, limits: &Limits) -> Result<(u32, u32), PictureError> {
    let w = pdf.get(dict, "Width").as_int().unwrap_or(0);
    let h = pdf.get(dict, "Height").as_int().unwrap_or(0);
    let side = i64::from(limits.max_side);
    if w < 1 || h < 1 || w > side || h > side {
        return Err(PictureError::Malformed(format!("the picture's size ({w}×{h}) is not usable")));
    }
    Ok((u32::try_from(w).unwrap_or(1), u32::try_from(h).unwrap_or(1)))
}

/// Decodes a soft mask (a grey picture) into one alpha byte per pixel of a `w × h` picture.
fn soft_mask(
    pdf: &mut PdfFile<'_>,
    mask: &Obj,
    w: u32,
    h: u32,
    limits: &Limits,
) -> Option<Vec<u8>> {
    let mask = pdf.resolve(mask);
    let stream = mask.as_stream()?.clone();
    let (mw, mh) = dims(pdf, &stream.dict, limits).ok()?;
    if u64::from(mw) * u64::from(mh) > limits.max_stream_pixels {
        return None;
    }
    let bpc =
        usize::try_from(pdf.get(&stream.dict, "BitsPerComponent").as_int().unwrap_or(8)).ok()?;
    if ![1, 2, 4, 8, 16].contains(&bpc) {
        return None;
    }
    let (decoded, codec) = pdf.image_stream(&stream, MAX_SAMPLES).ok()?;
    if codec.is_some() {
        return None;
    }
    let pairs = decode_pairs(pdf, &stream.dict, 1);
    let d = pairs.first().copied().unwrap_or((0.0, 1.0));
    let stride = (usize::try_from(mw).ok()? * bpc).div_ceil(8);
    let mut alpha = vec![255u8; usize::try_from(w).ok()? * usize::try_from(h).ok()?];
    for y in 0..usize::try_from(h).ok()? {
        let sy = y * usize::try_from(mh).ok()? / usize::try_from(h).ok()?;
        let row = decoded.data.get(sy * stride..(sy + 1) * stride).unwrap_or(&[]);
        for x in 0..usize::try_from(w).ok()? {
            let sx = x * usize::try_from(mw).ok()? / usize::try_from(w).ok()?;
            if let Some(a) = alpha.get_mut(y * usize::try_from(w).ok()? + x) {
                *a = level(sample(row, bpc, sx), bpc, d);
            }
        }
    }
    Some(alpha)
}

/// Picture `num` of the PDF `data`.
pub(crate) fn picture(
    data: &[u8],
    num: u32,
    policy: &PreviewPolicy,
) -> Result<Found, PictureError> {
    let limits = Limits::default();
    let mut pdf = PdfFile::open(data, policy).map_err(from_preview)?;
    let obj = pdf.object(num);
    let Some(stream) = obj.as_stream().cloned() else { return Err(PictureError::Missing) };
    if pdf.get(&stream.dict, "Subtype").as_name() != Some("Image") {
        return Err(PictureError::Missing);
    }
    let (w, h) = dims(&mut pdf, &stream.dict, &limits)?;
    let (decoded, codec) = pdf.image_stream(&stream, MAX_SAMPLES).map_err(from_filter)?;
    match codec.as_deref() {
        Some("DCTDecode") => return Ok(Found::Jpeg(decoded.data)),
        Some(other) => {
            return Err(PictureError::Unsupported(format!(
                "{} pictures are not shown",
                codec_name(other)
            )));
        }
        None => {}
    }
    let stencil = matches!(pdf.get(&stream.dict, "ImageMask"), Obj::Bool(true));
    let bpc = if stencil {
        1
    } else {
        usize::try_from(pdf.get(&stream.dict, "BitsPerComponent").as_int().unwrap_or(8))
            .unwrap_or(0)
    };
    if ![1, 2, 4, 8, 16].contains(&bpc) {
        return Err(PictureError::Unsupported(format!(
            "pictures with {bpc} bits per sample are not shown"
        )));
    }
    let cs_obj = pdf.get(&stream.dict, "ColorSpace");
    let cs = if stencil {
        Space::Gray
    } else {
        space(&mut pdf, &cs_obj, 0)
            .ok_or_else(|| PictureError::Unsupported("this colour space is not shown".into()))?
    };
    let comps = cs.components();
    let pairs = decode_pairs(&mut pdf, &stream.dict, comps);
    let indexed_default = matches!(cs, Space::Indexed(_));
    let alpha = if stencil {
        None
    } else {
        let sm = pdf.get(&stream.dict, "SMask");
        if sm.is_null() { None } else { soft_mask(&mut pdf, &sm, w, h, &limits) }
    };
    let (wu, hu) = (usize::try_from(w).unwrap_or(0), usize::try_from(h).unwrap_or(0));
    let stride = (wu * comps * bpc).div_ceil(8);
    let mut reducer = RowReducer::new(w, h, &limits).map_err(from_decode)?;
    let mut out = vec![0u8; wu * 4];
    for y in 0..hu {
        let Some(row) = decoded.data.get(y * stride..(y + 1) * stride) else { break };
        for (x, px) in out.chunks_exact_mut(4).enumerate() {
            let rgb: [u8; 3];
            let mut a = 255u8;
            if stencil {
                // Sample 0 paints (black), 1 leaves the page alone; a Decode of [1 0] swaps them.
                let painted = (sample(row, 1, x) == 0)
                    == (pairs.first().copied().unwrap_or((0.0, 1.0)).0 == 0.0);
                rgb = [0, 0, 0];
                a = if painted { 255 } else { 0 };
            } else {
                match &cs {
                    Space::Gray => {
                        let g = level(
                            sample(row, bpc, x),
                            bpc,
                            pairs.first().copied().unwrap_or((0.0, 1.0)),
                        );
                        rgb = [g, g, g];
                    }
                    Space::Rgb => {
                        let l = |k: usize| {
                            level(
                                sample(row, bpc, x * 3 + k),
                                bpc,
                                pairs.get(k).copied().unwrap_or((0.0, 1.0)),
                            )
                        };
                        rgb = [l(0), l(1), l(2)];
                    }
                    Space::Cmyk => {
                        let l = |k: usize| {
                            level(
                                sample(row, bpc, x * 4 + k),
                                bpc,
                                pairs.get(k).copied().unwrap_or((0.0, 1.0)),
                            )
                        };
                        rgb = cmyk(l(0), l(1), l(2), l(3));
                    }
                    Space::Indexed(palette) => {
                        let i = usize::try_from(sample(row, bpc, x)).unwrap_or(0);
                        let _ = indexed_default;
                        rgb = palette.get(i).copied().unwrap_or([0, 0, 0]);
                    }
                }
                if let Some(al) = &alpha {
                    a = al.get(y * wu + x).copied().unwrap_or(255);
                }
            }
            px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
        reducer.push_row(&out);
    }
    Ok(Found::Samples(reducer.finish(alpha.is_some() || stencil)))
}
