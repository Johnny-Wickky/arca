//! The drawing of one page: what a page looks like, as a list of things to draw.
//!
//! The reading view ([`crate::model::Preview`]) is the text of a document; this is its layout.
//! It is a *model*, not pixels: filled and stroked paths, text runs with a position, a size, a
//! direction and the width they occupy, pictures as the parallelogram they cover, and clip
//! rectangles. The app draws them with its own canvas and its own fonts. Nothing here decodes a
//! picture or loads a font program, and the number of things is bounded.
//!
//! Coordinates are in points (1/72 inch) with the origin at the top left of the visible page and
//! y pointing down, already turned by the page's rotation.

use arca_abi::wire::{Reader, Wire, WireError, Writer};

/// Most primitives on one page.
pub const MAX_PRIMS: usize = 150_000;
/// Most numbers in the path of one primitive.
pub const MAX_PATH_NUMBERS: usize = 200_000;

/// Path operations in [`Prim::Path::ops`]: a code, then the numbers it takes.
pub mod op {
    /// `MOVE x y`.
    pub const MOVE: f32 = 0.0;
    /// `LINE x y`.
    pub const LINE: f32 = 1.0;
    /// `CURVE x1 y1 x2 y2 x y`.
    pub const CURVE: f32 = 2.0;
    /// `CLOSE`.
    pub const CLOSE: f32 = 3.0;
}

/// Text style bits.
pub mod style {
    /// Bold.
    pub const BOLD: u8 = 1;
    /// Italic.
    pub const ITALIC: u8 = 2;
    /// Monospaced.
    pub const MONO: u8 = 4;
    /// Serif.
    pub const SERIF: u8 = 8;
}

/// One thing to draw.
#[derive(Debug, Clone, PartialEq)]
pub enum Prim {
    /// A filled and/or stroked path.
    Path {
        /// Operations: see [`op`].
        ops: Vec<f32>,
        /// Fill colour as `0xAARRGGBB`; alpha 0 means no fill.
        fill: u32,
        /// Stroke colour as `0xAARRGGBB`; alpha 0 means no stroke.
        stroke: u32,
        /// Stroke width in points.
        width: f32,
        /// Fill rule: even-odd instead of non-zero winding.
        even_odd: bool,
    },
    /// A run of text.
    Text {
        /// Start of the baseline.
        x: f32,
        /// Start of the baseline.
        y: f32,
        /// Font size in points.
        size: f32,
        /// Direction of the baseline in radians (0 is left to right, positive turns clockwise on
        /// the screen).
        angle: f32,
        /// The width the run occupies along the baseline, in points.
        width: f32,
        /// [`style`] bits.
        style: u8,
        /// Colour as `0xAARRGGBB`.
        color: u32,
        /// The text.
        text: String,
    },
    /// A picture: the unit square under the matrix `[a b c d e f]` (the picture itself is not
    /// decoded).
    Image {
        /// Matrix.
        m: [f32; 6],
        /// A short description (its size in pixels).
        label: String,
    },
    /// Remember the clip.
    Save,
    /// Return to the last remembered clip.
    Restore,
    /// Intersect the clip with a rectangle.
    Clip {
        /// Left.
        x: f32,
        /// Top.
        y: f32,
        /// Width.
        w: f32,
        /// Height.
        h: f32,
    },
}

/// The drawing of one page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageModel {
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
    /// The page's number, from 0.
    pub index: u32,
    /// How many pages the document has.
    pub pages: u32,
    /// Things to draw, in order.
    pub prims: Vec<Prim>,
    /// The page was cut short by a limit.
    pub truncated: bool,
    /// Some pictures, gradients or fonts are only approximated.
    pub approximate: bool,
}

const T_PATH: u8 = 0;
const T_TEXT: u8 = 1;
const T_IMAGE: u8 = 2;
const T_SAVE: u8 = 3;
const T_RESTORE: u8 = 4;
const T_CLIP: u8 = 5;

impl Wire for Prim {
    fn encode(&self, w: &mut Writer) {
        match self {
            Self::Path { ops, fill, stroke, width, even_odd } => {
                w.u8(T_PATH);
                w.put(ops);
                w.u32(*fill);
                w.u32(*stroke);
                w.put(width);
                w.bool(*even_odd);
            }
            Self::Text { x, y, size, angle, width, style, color, text } => {
                w.u8(T_TEXT);
                for v in [x, y, size, angle, width] {
                    w.put(v);
                }
                w.u8(*style);
                w.u32(*color);
                w.str(text);
            }
            Self::Image { m, label } => {
                w.u8(T_IMAGE);
                for v in m {
                    w.put(v);
                }
                w.str(label);
            }
            Self::Save => w.u8(T_SAVE),
            Self::Restore => w.u8(T_RESTORE),
            Self::Clip { x, y, w: cw, h } => {
                w.u8(T_CLIP);
                for v in [x, y, cw, h] {
                    w.put(v);
                }
            }
        }
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(match r.u8()? {
            T_PATH => Self::Path {
                ops: r.get()?,
                fill: r.u32()?,
                stroke: r.u32()?,
                width: r.get()?,
                even_odd: r.bool()?,
            },
            T_TEXT => Self::Text {
                x: r.get()?,
                y: r.get()?,
                size: r.get()?,
                angle: r.get()?,
                width: r.get()?,
                style: r.u8()?,
                color: r.u32()?,
                text: r.str()?,
            },
            T_IMAGE => {
                let mut m = [0f32; 6];
                for v in &mut m {
                    *v = r.get()?;
                }
                Self::Image { m, label: r.str()? }
            }
            T_SAVE => Self::Save,
            T_RESTORE => Self::Restore,
            T_CLIP => Self::Clip { x: r.get()?, y: r.get()?, w: r.get()?, h: r.get()? },
            other => return Err(WireError::InvalidTag(u16::from(other))),
        })
    }
}

impl Wire for PageModel {
    fn encode(&self, w: &mut Writer) {
        w.put(&self.width);
        w.put(&self.height);
        w.u32(self.index);
        w.u32(self.pages);
        w.put(&self.prims);
        w.bool(self.truncated);
        w.bool(self.approximate);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        r.enter()?;
        let v = Self {
            width: r.get()?,
            height: r.get()?,
            index: r.u32()?,
            pages: r.u32()?,
            prims: r.get()?,
            truncated: r.bool()?,
            approximate: r.bool()?,
        };
        r.leave();
        Ok(v)
    }
}

/// The page as an SVG image: a reference drawing for tests and for looking at what the model
/// holds (the apps draw the model with their own canvas).
#[must_use]
pub fn to_svg(page: &PageModel) -> String {
    use core::fmt::Write as _;

    fn rgba(c: u32) -> (String, f32) {
        let a = f32::from(u8::try_from(c >> 24).unwrap_or(0)) / 255.0;
        (format!("#{:06x}", c & 0x00FF_FFFF), a)
    }
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }
    let mut out = String::new();
    let _ = write!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\"><rect width=\"{w}\" height=\"{h}\" fill=\"#fff\"/>",
        w = page.width,
        h = page.height
    );
    // `open[i]` is how many clip groups were opened since the i-th Save.
    let mut open: Vec<usize> = vec![0];
    let mut clips = 0usize;
    for p in &page.prims {
        match p {
            Prim::Save => open.push(0),
            Prim::Restore => {
                if open.len() > 1
                    && let Some(n) = open.pop()
                {
                    for _ in 0..n {
                        out.push_str("</g>");
                    }
                }
            }
            Prim::Clip { x, y, w, h } => {
                clips += 1;
                let _ = write!(
                    out,
                    "<clipPath id=\"c{clips}\"><rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\"/></clipPath><g clip-path=\"url(#c{clips})\">"
                );
                if let Some(n) = open.last_mut() {
                    *n += 1;
                }
            }
            Prim::Path { ops, fill, stroke, width, even_odd } => {
                let mut d = String::new();
                let mut i = 0;
                while i < ops.len() {
                    let code = ops.get(i).copied().unwrap_or(-1.0);
                    let n = if code == op::MOVE || code == op::LINE {
                        2
                    } else if code == op::CURVE {
                        6
                    } else {
                        0
                    };
                    let letter = if code == op::MOVE {
                        'M'
                    } else if code == op::LINE {
                        'L'
                    } else if code == op::CURVE {
                        'C'
                    } else {
                        'Z'
                    };
                    d.push(letter);
                    for k in 0..n {
                        let _ = write!(d, "{} ", ops.get(i + 1 + k).copied().unwrap_or(0.0));
                    }
                    i += 1 + n;
                }
                let (fc, fa) = rgba(*fill);
                let (sc, sa) = rgba(*stroke);
                let _ = write!(
                    out,
                    "<path d=\"{d}\" fill=\"{}\" fill-opacity=\"{fa}\" fill-rule=\"{}\" stroke=\"{}\" stroke-opacity=\"{sa}\" stroke-width=\"{width}\"/>",
                    if fill >> 24 == 0 { "none" } else { &fc },
                    if *even_odd { "evenodd" } else { "nonzero" },
                    if stroke >> 24 == 0 { "none" } else { &sc },
                );
            }
            Prim::Text { x, y, size, angle, width, style, color, text } => {
                let (c, a) = rgba(*color);
                let family = if style & style::MONO != 0 {
                    "monospace"
                } else if style & style::SERIF != 0 {
                    "serif"
                } else {
                    "sans-serif"
                };
                let _ = write!(
                    out,
                    "<text x=\"{x}\" y=\"{y}\" font-size=\"{size}\" font-family=\"{family}\" font-weight=\"{}\" font-style=\"{}\" fill=\"{c}\" fill-opacity=\"{a}\" textLength=\"{width}\" lengthAdjust=\"spacingAndGlyphs\" xml:space=\"preserve\" transform=\"rotate({} {x} {y})\">{}</text>",
                    if style & style::BOLD != 0 { "bold" } else { "normal" },
                    if style & style::ITALIC != 0 { "italic" } else { "normal" },
                    angle.to_degrees(),
                    esc(text)
                );
            }
            Prim::Image { m, label } => {
                let pt = |u: f32, v: f32| (m[0] * u + m[2] * v + m[4], m[1] * u + m[3] * v + m[5]);
                let pts: Vec<String> = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
                    .iter()
                    .map(|(u, v)| {
                        let (x, y) = pt(*u, *v);
                        format!("{x},{y}")
                    })
                    .collect();
                let (cx, cy) = pt(0.5, 0.5);
                let _ = write!(
                    out,
                    "<polygon points=\"{}\" fill=\"#dddddd\" stroke=\"#999999\"/><text x=\"{cx}\" y=\"{cy}\" font-size=\"10\" text-anchor=\"middle\" fill=\"#666\">{}</text>",
                    pts.join(" "),
                    esc(label)
                );
            }
        }
    }
    for n in open {
        for _ in 0..n {
            out.push_str("</g>");
        }
    }
    out.push_str("</svg>");
    out
}

/// Packs a colour from 0..=1 components as `0xAARRGGBB`.
#[must_use]
pub fn argb(r: f64, g: f64, b: f64, a: f64) -> u32 {
    let c = |v: f64| -> u32 {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        // `v` is in 0..=1, so the product is in 0..=255.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            (v * 255.0).round() as u32
        }
    };
    (c(a) << 24) | (c(r) << 16) | (c(g) << 8) | c(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arca_abi::wire::{DecodeLimits, from_bytes, to_bytes};

    fn sample() -> PageModel {
        PageModel {
            width: 612.0,
            height: 792.0,
            index: 2,
            pages: 5,
            prims: vec![
                Prim::Save,
                Prim::Clip { x: 10.0, y: 10.0, w: 500.0, h: 700.0 },
                Prim::Path {
                    ops: vec![op::MOVE, 1.0, 2.0, op::LINE, 3.0, 4.0, op::CLOSE],
                    fill: argb(1.0, 0.0, 0.0, 1.0),
                    stroke: 0,
                    width: 1.5,
                    even_odd: true,
                },
                Prim::Text {
                    x: 72.0,
                    y: 100.0,
                    size: 12.0,
                    angle: 0.5,
                    width: 80.0,
                    style: style::BOLD | style::SERIF,
                    color: 0xFF00_0000,
                    text: "Привет, мир".into(),
                },
                Prim::Image { m: [100.0, 0.0, 0.0, -50.0, 10.0, 200.0], label: "640×480".into() },
                Prim::Restore,
            ],
            truncated: false,
            approximate: true,
        }
    }

    #[test]
    fn a_page_survives_the_wire() {
        let page = sample();
        let bytes = to_bytes(&page).unwrap();
        let back: PageModel = from_bytes(&bytes, DecodeLimits::default()).unwrap();
        assert_eq!(back, page);
        // Every cut of the encoding is an error, never a panic.
        for len in 0..bytes.len() {
            assert!(from_bytes::<PageModel>(&bytes[..len], DecodeLimits::default()).is_err());
        }
        // An unknown primitive tag is refused.
        let mut bad = bytes.clone();
        let at = bad.iter().position(|b| *b == T_SAVE).unwrap();
        bad[at] = 99;
        let _ = from_bytes::<PageModel>(&bad, DecodeLimits::default());
    }

    #[test]
    fn colours_are_packed_and_clamped() {
        assert_eq!(argb(1.0, 0.0, 0.0, 1.0), 0xFFFF_0000);
        assert_eq!(argb(0.0, 0.0, 0.0, 0.0), 0);
        assert_eq!(argb(2.0, -1.0, f64::NAN, 1.0), 0xFFFF_0000);
    }
}
