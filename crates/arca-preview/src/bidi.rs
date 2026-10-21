//! Bidirectional text: from the order glyphs are drawn in to the order they are read in.
//!
//! A PDF page lists glyphs left to right as they appear. For Hebrew and Arabic that is the
//! *reverse* of the reading order, so text taken from a content stream comes out backwards.
//! This module undoes it with a deliberately small version of the Unicode Bidirectional
//! Algorithm (UAX #9): strong letters, digits and neutrals are resolved the way the standard
//! resolves them in ordinary text, and the reordering rule (L2) is applied. Embeddings,
//! isolates and overrides are not interpreted (the sink removes them anyway).
//!
//! The reordering is its own inverse, so applying the display rule to a visual string with the
//! levels of its characters gives the logical string.
//!
//! Which way a paragraph runs cannot be told from the glyphs alone: `x ABC y` drawn left to right
//! is the same picture in a left-to-right paragraph and in a right-to-left one, and reads
//! differently. The caller says ([`Base`]) when it knows, for instance from which margin the line
//! is flush with; otherwise the script with more letters decides.

/// What a character is, as far as direction goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A letter of a left-to-right script.
    Ltr,
    /// A letter of a right-to-left script.
    Rtl,
    /// A European or Arabic-Indic digit.
    Digit,
    /// A separator that stays with the digits around it (`1,234.5`, `12:30`, `10/2`).
    Separator,
    /// A combining mark: takes the level of the character before it.
    Mark,
    /// Anything else: spaces and punctuation.
    Neutral,
}

/// `true` for characters of right-to-left scripts (letters, not digits or marks).
fn is_rtl_letter(c: char) -> bool {
    matches!(u32::from(c),
        0x05D0..=0x05EA | 0x05EF..=0x05F4 | 0x0608 | 0x060B | 0x060D | 0x061B..=0x064A
        | 0x066D..=0x066F | 0x0671..=0x06D5 | 0x06E5..=0x06E6 | 0x06EE..=0x06EF
        | 0x06FA..=0x070D | 0x070F..=0x0710 | 0x0712..=0x072F | 0x074D..=0x07A5 | 0x07B1
        | 0x07C0..=0x07EA | 0x07F4..=0x07F5 | 0x07FA | 0x0800..=0x0815 | 0x081A | 0x0824
        | 0x0828 | 0x0830..=0x083E | 0x0840..=0x0858 | 0x085E | 0x0860..=0x086A
        | 0x08A0..=0x08C9 | 0x200F | 0xFB1D | 0xFB1F..=0xFB28 | 0xFB2A..=0xFB4F
        | 0xFB50..=0xFBC2 | 0xFBD3..=0xFD3D | 0xFD50..=0xFD8F | 0xFD92..=0xFDC7
        | 0xFDF0..=0xFDFC | 0xFE70..=0xFE74 | 0xFE76..=0xFEFC
        | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
}

/// Combining marks of the right-to-left scripts.
fn is_rtl_mark(c: char) -> bool {
    matches!(u32::from(c),
        0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2 | 0x05C4..=0x05C5 | 0x05C7
        | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x06DF..=0x06E4
        | 0x06E7..=0x06E8 | 0x06EA..=0x06ED | 0x0711 | 0x0730..=0x074A | 0x07A6..=0x07B0
        | 0x07EB..=0x07F3 | 0x08D3..=0x08E1 | 0x08E3..=0x08FF)
}

fn kind_of(c: char) -> Kind {
    if is_rtl_letter(c) {
        Kind::Rtl
    } else if is_rtl_mark(c) {
        Kind::Mark
    } else if c.is_ascii_digit() || matches!(u32::from(c), 0x0660..=0x0669 | 0x06F0..=0x06F9) {
        Kind::Digit
    } else if matches!(
        c,
        ',' | '.' | ':' | '/' | '٫' | '٬' | '%' | '+' | '-' | '$' | '€' | '£' | '#'
    ) {
        Kind::Separator
    } else if c.is_alphabetic() {
        Kind::Ltr
    } else {
        Kind::Neutral
    }
}

/// `true` if `text` has any right-to-left letter, so that reordering can matter.
#[must_use]
pub(crate) fn has_rtl(text: &str) -> bool {
    text.chars().any(is_rtl_letter)
}

/// The mirror image of a bracket, for characters displayed in a right-to-left run.
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        c => c,
    }
}

/// The direction of the paragraph a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Base {
    Ltr,
    Rtl,
    /// Not known: the script with more letters in the line decides.
    Auto,
}

/// Resolves the embedding level of every character of a *visual* line.
fn levels(chars: &[char], direction: Base) -> Vec<u8> {
    let kinds: Vec<Kind> = chars.iter().copied().map(kind_of).collect();
    let rtl = kinds.iter().filter(|k| **k == Kind::Rtl).count();
    let ltr = kinds.iter().filter(|k| **k == Kind::Ltr).count();
    let base: u8 = match direction {
        Base::Ltr => 0,
        Base::Rtl => 1,
        Base::Auto => u8::from(rtl > ltr),
    };

    // Direction of the strong character before and after each position: `Some(true)` is
    // right-to-left. Digits count as right-to-left next to neutrals, as in rule N1.
    let strong = |k: Kind| match k {
        Kind::Rtl | Kind::Digit => Some(true),
        Kind::Ltr => Some(false),
        _ => None,
    };
    let mut before: Vec<Option<bool>> = Vec::with_capacity(kinds.len());
    let mut last: Option<bool> = None;
    for k in &kinds {
        before.push(last);
        last = strong(*k).or(last);
    }
    let mut after: Vec<Option<bool>> = Vec::with_capacity(kinds.len());
    let mut next: Option<bool> = None;
    for k in kinds.iter().rev() {
        after.push(next);
        next = strong(*k).or(next);
    }
    after.reverse();

    let rtl_text = |letter_dir: Option<bool>| base == 1 || letter_dir == Some(true);
    let mut out: Vec<u8> = Vec::with_capacity(kinds.len());
    // The direction of the text a digit sits in: that of the last letter before it.
    let mut letter_dir: Option<bool> = None;
    for (i, ((k, b), a)) in kinds.iter().zip(&before).zip(&after).enumerate() {
        let level = match k {
            Kind::Rtl => {
                letter_dir = Some(true);
                1
            }
            Kind::Ltr => {
                letter_dir = Some(false);
                if base == 1 { 2 } else { 0 }
            }
            // Digits are written left to right, also inside right-to-left text.
            Kind::Digit => {
                if rtl_text(letter_dir) {
                    2
                } else {
                    0
                }
            }
            Kind::Separator => {
                // Between two digits a separator belongs to them; otherwise it is neutral.
                let between = i > 0
                    && matches!(kinds.get(i - 1), Some(Kind::Digit))
                    && matches!(kinds.get(i + 1), Some(Kind::Digit));
                if between {
                    if rtl_text(letter_dir) { 2 } else { 0 }
                } else {
                    neutral_level(*b, *a, base)
                }
            }
            Kind::Mark => out.last().copied().unwrap_or(base),
            Kind::Neutral => neutral_level(*b, *a, base),
        };
        out.push(level);
    }
    out
}

/// A neutral between two characters of the same direction takes that direction; between
/// different ones it takes the paragraph's.
fn neutral_level(before: Option<bool>, after: Option<bool>, base: u8) -> u8 {
    match (before, after) {
        (Some(true), Some(true)) => 1,
        (Some(false), Some(false)) => {
            if base == 1 {
                2
            } else {
                0
            }
        }
        _ => base,
    }
}

/// The reading order of a visual line: the characters in logical order, brackets mirrored back
/// in right-to-left runs. A line with nothing right-to-left in it is returned as it is.
#[must_use]
pub(crate) fn to_logical(visual: &[char], direction: Base) -> Vec<char> {
    if !visual.iter().copied().any(is_rtl_letter) {
        return visual.to_vec();
    }
    let lv = levels(visual, direction);
    let mut order: Vec<usize> = (0..visual.len()).collect();
    // L2: from the highest level down to the lowest odd one, reverse every run at that level
    // or above.
    let max = lv.iter().copied().max().unwrap_or(0);
    let min_odd = lv.iter().copied().filter(|l| l % 2 == 1).min().unwrap_or(1);
    let mut level = max;
    while level >= min_odd && level > 0 {
        let mut i = 0;
        while i < lv.len() {
            if lv.get(i).is_some_and(|l| *l >= level) {
                let start = i;
                while lv.get(i).is_some_and(|l| *l >= level) {
                    i += 1;
                }
                if let Some(run) = order.get_mut(start..i) {
                    run.reverse();
                }
            } else {
                i += 1;
            }
        }
        level -= 1;
    }
    order
        .iter()
        .filter_map(|&p| {
            let c = *visual.get(p)?;
            // Characters at odd levels were drawn mirrored; the text holds the original.
            Some(if lv.get(p).is_some_and(|l| l % 2 == 1) { mirror(c) } else { c })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logical(visual: &str) -> String {
        logical_in(visual, Base::Auto)
    }

    fn logical_in(visual: &str, base: Base) -> String {
        to_logical(&visual.chars().collect::<Vec<_>>(), base).into_iter().collect()
    }

    /// The display rule applied to a logical string gives the visual one; used to build inputs.
    fn visual(logical_text: &str) -> String {
        // For a single run of right-to-left words in a left-to-right line the reversal is plain.
        logical_text.chars().rev().collect()
    }

    #[test]
    fn text_without_right_to_left_letters_is_untouched() {
        for s in ["", "Hello, world 123", "Привет, мир!", "日本語のテキスト", "1,234.50 (ok)"]
        {
            assert_eq!(logical(s), s);
        }
        assert!(!has_rtl("Hello"));
        assert!(has_rtl("שלום"));
        assert!(has_rtl("a ب b"));
    }

    #[test]
    fn a_hebrew_or_arabic_word_in_a_left_to_right_line_is_turned_round() {
        assert_eq!(logical("TOK_RTL "), "TOK_RTL ");
        assert_eq!(
            logical_in(&format!("see {} here", visual("עברית")), Base::Ltr),
            "see עברית here"
        );
        assert_eq!(logical_in(&format!("x {} y", visual("مرحبا")), Base::Ltr), "x مرحبا y");
        // Several words: the order of the words flips too.
        assert_eq!(logical(&visual("עברית مرحبا بالعالم")), "עברית مرحبا بالعالم");
        assert_eq!(
            logical_in(&format!("TOK {} end", visual("עברית مرحبا بالعالم")), Base::Ltr),
            "TOK עברית مرحبا بالعالم end"
        );
    }

    #[test]
    fn the_report_fixture_line() {
        // As the glyphs of tests/fixtures/report.pdf are drawn: Arabic, Hebrew in one run.
        let drawn = "日本語のテキスト, תירבע ,ةيبرعلا, emoji";
        assert_eq!(logical_in(drawn, Base::Ltr), "日本語のテキスト, العربية, עברית, emoji");
        // The letters alone point the same way here (Japanese counts as left to right).
        assert_eq!(logical(drawn), "日本語のテキスト, العربية, עברית, emoji");
    }

    #[test]
    fn digits_inside_right_to_left_text_keep_their_order() {
        // Logical: "דוח 2024 סופי" — the number stays readable left to right.
        let logical_text = "דוח 2024 סופי";
        // Drawn: the whole line is mirrored except the digits.
        let drawn: String = "יפוס 2024 חוד".to_owned();
        assert_eq!(logical(&drawn), logical_text);
        // Numbers with separators.
        assert_eq!(logical("יפוס 1,234.50 חוד"), "דוח 1,234.50 סופי");
    }

    #[test]
    fn a_right_to_left_paragraph_with_a_latin_word() {
        // Logical: "שלום world שלום" is a right-to-left line with an English word inside.
        let drawn = "םולש world םולש";
        assert_eq!(logical_in(drawn, Base::Rtl), "שלום world שלום");
        assert_eq!(logical(drawn), "שלום world שלום", "two Hebrew words outweigh one Latin one");
        // Mostly Arabic, a Latin phrase in the middle: the phrase is not reversed.
        assert_eq!(logical("ملاعلا Hello World ابحرم"), "مرحبا Hello World العالم");
    }

    #[test]
    fn the_same_picture_reads_differently_in_the_two_directions() {
        // `x ABC y` drawn left to right.
        let drawn = format!("x {} y", visual("مرحبا"));
        assert_eq!(logical_in(&drawn, Base::Ltr), "x مرحبا y");
        assert_eq!(logical_in(&drawn, Base::Rtl), "y مرحبا x");
    }

    #[test]
    fn brackets_are_mirrored_back_in_right_to_left_runs() {
        // Logical "(שלום)": drawn mirrored.
        assert_eq!(logical("(םולש)"), "(שלום)");
        // In a left-to-right run they are left alone.
        assert_eq!(logical("(a) [b]"), "(a) [b]");
    }

    #[test]
    fn combining_marks_stay_with_their_letter() {
        // Hebrew points (niqqud). Shaping lists glyphs in drawing order, so in a mirrored run a
        // mark is drawn before its letter; reversing the run puts it back after.
        let word = "שָׁלוֹם";
        let drawn: String = word.chars().rev().collect();
        assert_eq!(logical(&drawn), word);
        assert_eq!(logical_in(&format!("a {drawn} b"), Base::Ltr), format!("a {word} b"));
    }

    #[test]
    fn hostile_input_never_panics() {
        // Long runs of every direction, lone marks and separators.
        let s: String = ('\u{0590}'..='\u{06FF}').chain('a'..='z').chain('0'..='9').collect();
        let _ = logical(&s);
        let _ = logical("\u{064B}");
        let _ = logical(",.:/");
        let big: String = "עב1,2 ".repeat(50_000);
        assert_eq!(logical(&big).chars().count(), big.chars().count());
    }
}
