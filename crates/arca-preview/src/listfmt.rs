//! Counters for numbered lists (shared by the presentation and OpenDocument extractors).

/// `1 → a`, `27 → aa`; at most eight letters.
pub(crate) fn letters(mut n: u32, upper: bool) -> String {
    let mut s = Vec::new();
    while n > 0 && s.len() < 8 {
        n -= 1;
        let base = if upper { b'A' } else { b'a' };
        s.push(char::from(base.saturating_add(u8::try_from(n % 26).unwrap_or(0))));
        n /= 26;
    }
    s.iter().rev().collect()
}

/// Roman numerals for `1..4000`; other values are written in decimal.
pub(crate) fn roman(n: u32) -> String {
    if n == 0 || n >= 4000 {
        return n.to_string();
    }
    const T: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    let mut rest = n;
    for (v, s) in T {
        while rest >= v {
            out.push_str(s);
            rest -= v;
        }
    }
    out
}

/// Formats counter `n` as `format` (`1`, `a`, `A`, `i`, `I`; anything else is decimal).
pub(crate) fn counter(format: &str, n: u32) -> String {
    match format {
        "a" => letters(n, false),
        "A" => letters(n, true),
        "i" => roman(n).to_lowercase(),
        "I" => roman(n),
        _ => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters() {
        assert_eq!(letters(1, false), "a");
        assert_eq!(letters(26, true), "Z");
        assert_eq!(letters(27, false), "aa");
        assert_eq!(letters(0, false), "");
        assert_eq!(roman(4), "IV");
        assert_eq!(roman(1994), "MCMXCIV");
        assert_eq!(roman(0), "0");
        assert_eq!(roman(4000), "4000");
        assert_eq!(counter("i", 9), "ix");
        assert_eq!(counter("1", 7), "7");
        assert_eq!(counter("weird", 7), "7");
    }
}
