//! Estimating the width of text and breaking it into lines.
//!
//! The sandbox has no font program (and must not load one), so the width of a run is an
//! estimate from per-character classes. The drawing app fits the run to the width given with it
//! (the scale is bounded there), so the estimate only has to be close: it decides where lines of
//! wrapped text break, whether a cell's text overflows into its neighbours, and how tall an
//! auto-sized row is.

use super::model::Font;

/// The advance of `c` in ems of a proportional sans-serif face.
fn glyph_em(c: char) -> f32 {
    match c {
        ' ' => 0.28,
        'i' | 'j' | 'l' | '.' | ',' | ':' | ';' | '\'' | '!' | '|' | '`' => 0.26,
        'f' | 't' | 'r' | 'I' | '(' | ')' | '[' | ']' | '-' | '/' | '\\' | '"' => 0.34,
        'm' | 'w' | 'M' | 'W' | '@' | '%' => 0.85,
        '0'..='9' => 0.556,
        'A'..='Z' => 0.67,
        'a'..='z' => 0.52,
        // Combining marks and joiners take no room of their own.
        '\u{0300}'..='\u{036F}' | '\u{200B}'..='\u{200F}' | '\u{FE00}'..='\u{FE0F}' => 0.0,
        // Cyrillic and Greek.
        '\u{0370}'..='\u{052F}' => 0.58,
        // Hangul, CJK, kana and full-width forms.
        '\u{1100}'..='\u{11FF}'
        | '\u{2E80}'..='\u{A4CF}'
        | '\u{AC00}'..='\u{D7A3}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FF00}'..='\u{FF60}'
        | '\u{20000}'..='\u{3FFFF}' => 1.0,
        // Emoji and symbols.
        '\u{1F000}'..='\u{1FAFF}' => 1.0,
        _ => 0.56,
    }
}

/// The estimated width of `text` in points at the font's size.
pub(super) fn width(text: &str, font: &Font) -> f32 {
    let mut ems = 0.0f32;
    for c in text.chars() {
        let w = glyph_em(c);
        ems += if font.mono && w > 0.0 { 0.6 } else { w };
    }
    if font.bold && !font.mono {
        ems *= 1.06;
    }
    ems * font.size
}

/// One line of wrapped text and its estimated width in points.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Line {
    pub(super) text: String,
    pub(super) width: f32,
}

/// Most lines kept for one cell.
pub(super) const MAX_LINES: usize = 200;

/// Breaks `text` into lines no wider than `max` points. Paragraphs end at a newline; words
/// that are wider than a line are broken between characters. At most [`MAX_LINES`] lines are
/// returned, the second value says whether there were more.
pub(super) fn wrap(text: &str, max: f32, font: &Font) -> (Vec<Line>, bool) {
    // A line holds at least a few characters, whatever the column is.
    let max = if max.is_finite() { max.max(font.size * 0.5) } else { font.size };
    let mut lines: Vec<Line> = Vec::new();
    let mut cut = false;
    for para in text.split('\n') {
        let para = para.trim_end_matches('\r');
        let mut cur = String::new();
        let mut cur_w = 0.0f32;
        let space = width(" ", font);
        for word in para.split(' ') {
            if lines.len() >= MAX_LINES {
                break;
            }
            let w = width(word, font);
            let need = if cur.is_empty() { w } else { cur_w + space + w };
            if need <= max || (cur.is_empty() && w <= max) {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(word);
                cur_w = need;
                continue;
            }
            if !cur.is_empty() {
                lines.push(Line { text: std::mem::take(&mut cur), width: cur_w });
                cur_w = 0.0;
            }
            if w <= max {
                cur.push_str(word);
                cur_w = w;
                continue;
            }
            // A word wider than the line: break it between characters.
            for c in word.chars() {
                let cw = width(c.encode_utf8(&mut [0u8; 4]), font);
                if !cur.is_empty() && cur_w + cw > max {
                    lines.push(Line { text: std::mem::take(&mut cur), width: cur_w });
                    cur_w = 0.0;
                }
                cur.push(c);
                cur_w += cw;
                if lines.len() >= MAX_LINES {
                    break;
                }
            }
            if lines.len() >= MAX_LINES {
                break;
            }
        }
        lines.push(Line { text: cur, width: cur_w });
        if lines.len() >= MAX_LINES {
            cut = true;
            lines.truncate(MAX_LINES);
            break;
        }
    }
    (lines, cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_scale_with_size_and_bold() {
        let plain = Font::default();
        let big = Font { size: plain.size * 2.0, ..plain };
        let bold = Font { bold: true, ..plain };
        let w = width("Hello", &plain);
        assert!(w > 0.0);
        assert!((width("Hello", &big) - 2.0 * w).abs() < 0.01);
        assert!(width("Hello", &bold) > w);
        assert!(width("", &plain).abs() < f32::EPSILON);
    }

    #[test]
    fn text_wraps_at_words_and_breaks_long_words() {
        let f = Font::default();
        let one = wrap("alpha beta gamma", 1000.0, &f);
        assert_eq!(one.0.len(), 1);
        let narrow = wrap("alpha beta gamma", width("alpha", &f) + 1.0, &f);
        assert_eq!(narrow.0.len(), 3);
        let long = wrap("supercalifragilistic", 30.0, &f);
        assert!(long.0.len() > 1);
        assert_eq!(long.0.iter().map(|l| l.text.as_str()).collect::<String>(), "supercalifragilistic");
        let paras = wrap("a\nb\n\nc", 1000.0, &f);
        assert_eq!(paras.0.len(), 4);
    }

    #[test]
    fn the_number_of_lines_is_bounded() {
        let f = Font::default();
        let text = "word ".repeat(100_000);
        let (lines, cut) = wrap(&text, 20.0, &f);
        assert_eq!(lines.len(), MAX_LINES);
        assert!(cut);
        let (lines, cut) = wrap(&"x".repeat(100_000), 1.0, &f);
        assert!(lines.len() <= MAX_LINES);
        assert!(cut || lines.len() < MAX_LINES);
        let (lines, _) = wrap("abc", f32::NAN, &f);
        assert_eq!(lines.len(), 1);
    }
}
