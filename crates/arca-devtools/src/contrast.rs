// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! Contrast of the colour palettes of the Android app (WCAG 2.x contrast ratio).
//!
//! The palettes live in `ui/Theme.java` (six named ones, each overriding sixteen colours of the
//! resource set it is based on) and `res/values{,-night}/colors.xml` (the colours a palette does not
//! override: verdicts, severities, diff marks). The tests resolve every palette the way
//! `Theme.color` does and check that no text, badge, secondary caption or divider sinks into the
//! surface it is drawn on. Reading the Java source as text keeps the check free of a JVM.

use std::collections::BTreeMap;

const THEME: &str =
    include_str!("../../../android/app/src/main/java/org/arca/viewer/ui/Theme.java");
const LIGHT: &str = include_str!("../../../android/app/src/main/res/values/colors.xml");
const NIGHT: &str = include_str!("../../../android/app/src/main/res/values-night/colors.xml");

/// An opaque colour as `[r, g, b]`.
type Rgb = [u8; 3];

fn parse_hex(token: &str) -> Option<(u8, Rgb)> {
    let digits = token.trim_start_matches("0x").trim_start_matches('#');
    let value = u32::from_str_radix(digits, 16).ok()?;
    let [a, r, g, b] = match digits.len() {
        6 => [0xff, (value >> 16) as u8, (value >> 8) as u8, value as u8],
        8 => [(value >> 24) as u8, (value >> 16) as u8, (value >> 8) as u8, value as u8],
        _ => return None,
    };
    Some((a, [r, g, b]))
}

fn resource_colors(xml: &str) -> BTreeMap<String, (u8, Rgb)> {
    let mut out = BTreeMap::new();
    for line in xml.lines() {
        let Some(rest) = line.trim().strip_prefix("<color name=\"") else { continue };
        let Some((name, tail)) = rest.split_once("\">") else { continue };
        let Some((value, _)) = tail.split_once("</color>") else { continue };
        if let Some(c) = parse_hex(value) {
            out.insert(name.to_owned(), c);
        }
    }
    out
}

/// The names a palette overrides, in the order of the columns.
fn palette_keys() -> Vec<String> {
    let start = THEME.find("KEYS = {").expect("KEYS table");
    let end = THEME[start..].find("};").expect("end of KEYS") + start;
    THEME[start..end].split('"').skip(1).step_by(2).map(str::to_owned).collect()
}

/// The rows of `PALETTES` after the `null` of the classic palette, with the name from the comment.
fn palette_rows() -> Vec<(String, Vec<(u8, Rgb)>)> {
    let start = THEME.find("PALETTES = {").expect("PALETTES table");
    let end = THEME[start..].find("\n    };").expect("end of PALETTES") + start;
    let body = &THEME[start..end];
    let mut rows = Vec::new();
    let mut name = String::new();
    let mut current: Option<Vec<(u8, Rgb)>> = None;
    for line in body.lines() {
        let t = line.trim();
        if let Some(c) = t.strip_prefix("//") {
            name = c.trim().to_owned();
            continue;
        }
        if t.starts_with('{') {
            current = Some(Vec::new());
        }
        if let Some(row) = current.as_mut() {
            for token in
                t.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| w.starts_with("0x"))
            {
                row.push(parse_hex(token).expect("palette colour"));
            }
            if t.ends_with("},") {
                rows.push((name.clone(), current.take().expect("row")));
            }
        }
    }
    rows
}

fn dark_flags() -> Vec<bool> {
    let start = THEME.find("DARK = {").expect("DARK table");
    let end = THEME[start..].find('}').expect("end of DARK") + start;
    THEME[start + "DARK = {".len()..end].split(',').map(|w| w.trim() == "true").collect()
}

fn channel(c: u8) -> f64 {
    let v = f64::from(c) / 255.0;
    if v <= 0.039_28 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn luminance(c: Rgb) -> f64 {
    0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
}

/// WCAG contrast ratio of two opaque colours.
fn ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// `top` laid over `under` with its own alpha.
fn over(top: (u8, Rgb), under: Rgb) -> Rgb {
    let a = f64::from(top.0) / 255.0;
    let mix = |t: u8, u: u8| (f64::from(t) * a + f64::from(u) * (1.0 - a)).round() as u8;
    [mix(top.1[0], under[0]), mix(top.1[1], under[1]), mix(top.1[2], under[2])]
}

/// Every palette resolved to a name → colour map.
fn palettes() -> Vec<(String, BTreeMap<String, Rgb>)> {
    let keys = palette_keys();
    let dark = dark_flags();
    let light = resource_colors(LIGHT);
    let night = resource_colors(NIGHT);
    let mut out = Vec::new();
    for (i, (name, row)) in palette_rows().into_iter().enumerate() {
        assert_eq!(row.len(), keys.len(), "{name}: one colour per key");
        // The classic palette is index 0; the named ones follow.
        let is_dark = dark[i + 1];
        let base = if is_dark { &night } else { &light };
        let mut map: BTreeMap<String, (u8, Rgb)> = base.clone();
        for (key, colour) in keys.iter().zip(&row) {
            map.insert(key.clone(), *colour);
        }
        let surface = map["arca_surface"].1;
        let resolved = map.iter().map(|(k, v)| (k.clone(), over(*v, surface))).collect();
        out.push((name, resolved));
    }
    out
}

/// `(foreground, background, minimum ratio)`: what is drawn on what.
const PAIRS: &[(&str, &str, f64)] = &[
    // Body text on every surface.
    ("arca_on_surface", "arca_bg", 7.0),
    ("arca_on_surface", "arca_surface", 7.0),
    ("arca_on_surface", "arca_surface_variant", 4.5),
    ("arca_on_surface", "arca_code_bg", 4.5),
    // Secondary captions, hints, notes.
    ("arca_on_surface_variant", "arca_bg", 4.5),
    ("arca_on_surface_variant", "arca_surface", 4.5),
    ("arca_on_surface_variant", "arca_surface_variant", 4.5),
    ("arca_hidden_text", "arca_bg", 3.0),
    ("arca_hidden_text", "arca_surface", 3.0),
    ("arca_hidden_text", "arca_code_bg", 3.0),
    // Buttons, chips, links.
    ("arca_primary", "arca_bg", 4.5),
    ("arca_primary", "arca_surface", 4.5),
    ("arca_on_primary", "arca_primary", 4.5),
    ("arca_on_primary_container", "arca_primary_container", 4.5),
    ("arca_link", "arca_bg", 4.5),
    ("arca_link", "arca_surface", 4.5),
    ("arca_link", "arca_code_bg", 4.5),
    // Verdict badges and banners.
    ("arca_safe", "arca_safe_container", 4.5),
    ("arca_warn", "arca_warn_container", 4.5),
    ("arca_danger", "arca_danger_container", 4.5),
    ("arca_unknown", "arca_unknown_container", 4.5),
    ("arca_safe", "arca_surface", 4.5),
    ("arca_warn", "arca_surface", 4.5),
    ("arca_danger", "arca_surface", 4.5),
    ("arca_unknown", "arca_surface", 4.5),
    // Severities, differences.
    ("arca_sev_critical", "arca_surface", 4.5),
    ("arca_sev_high", "arca_surface", 4.5),
    ("arca_sev_medium", "arca_surface", 4.5),
    ("arca_sev_low", "arca_surface", 4.5),
    ("arca_sev_info", "arca_surface", 4.5),
    ("arca_deleted", "arca_surface", 4.5),
    ("arca_inserted", "arca_surface", 4.5),
    // Search highlight under body text.
    ("arca_on_surface", "arca_search_hit", 4.5),
    ("arca_on_surface", "arca_search_current", 4.5),
    // Dividers and outlines are decoration, but must be seen.
    ("arca_outline", "arca_surface", 1.15),
    ("arca_outline", "arca_bg", 1.15),
    // Containers must stand apart from the window they sit on.
    ("arca_surface", "arca_bg", 1.02),
    ("arca_surface_variant", "arca_bg", 1.03),
];

#[test]
fn six_named_palettes_are_read() {
    let names: Vec<String> = palettes().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names.len(), 6, "{names:?}");
    for want in ["Alabaster", "Obsidian", "Cashmere", "Sierra", "Emerald", "Desert"] {
        assert!(names.iter().any(|n| n.contains(want)), "{want} in {names:?}");
    }
}

#[test]
fn no_text_badge_caption_or_divider_sinks_into_its_background() {
    let mut failures = Vec::new();
    for (name, colours) in palettes() {
        for (fg, bg, min) in PAIRS {
            let (Some(f), Some(b)) = (colours.get(*fg), colours.get(*bg)) else {
                failures.push(format!("{name}: {fg} or {bg} is not defined"));
                continue;
            };
            let r = ratio(*f, *b);
            if r < *min {
                failures.push(format!("{name}: {fg} on {bg} is {r:.2}:1, need {min}:1"));
            }
        }
    }
    assert!(failures.is_empty(), "contrast failures:\n{}", failures.join("\n"));
}

/// What a Liquid Glass panel is made of, read from `Theme.java` so the test cannot drift from it.
fn glass_constant(name: &str) -> u32 {
    let key = format!("{name} = 0x");
    let at = THEME.find(&key).unwrap_or_else(|| panic!("{name} in Theme.java")) + key.len();
    let hex: String = THEME[at..].chars().take_while(char::is_ascii_hexdigit).collect();
    u32::from_str_radix(&hex, 16).expect("hex constant")
}

#[test]
fn text_on_a_glass_panel_keeps_its_contrast_over_the_worst_backdrop() {
    let body_light = glass_constant("GLASS_BODY_LIGHT");
    let body_dark = glass_constant("GLASS_BODY_DARK");
    let sheen_light = glass_constant("GLASS_SHEEN_LIGHT");
    let sheen_dark = glass_constant("GLASS_SHEEN_DARK");
    let dark = dark_flags();
    let mut failures = Vec::new();
    for (i, (name, c)) in palettes().into_iter().enumerate() {
        let is_dark = dark[i + 1];
        let (alpha, sheen) =
            if is_dark { (body_dark, sheen_dark) } else { (body_light, sheen_light) };
        // The window shows a wash of the brand colour (up to 0x66) behind the panels.
        let backdrop = over((0x66, c["arca_primary"]), c["arca_bg"]);
        for under in [c["arca_bg"], backdrop] {
            let body = over((alpha as u8, c["arca_surface"]), under);
            let sheen_rgb = [(sheen >> 16) as u8, (sheen >> 8) as u8, sheen as u8];
            let lit = over(((sheen >> 24) as u8, sheen_rgb), body);
            for panel in [body, lit] {
                for (fg, min) in [
                    ("arca_on_surface", 4.5),
                    ("arca_on_surface_variant", 4.5),
                    ("arca_primary", 4.5),
                    ("arca_link", 4.5),
                    ("arca_hidden_text", 3.0),
                ] {
                    let r = ratio(c[fg], panel);
                    if r < min {
                        failures.push(format!("{name}: {fg} on glass is {r:.2}:1, need {min}:1"));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "glass contrast failures:\n{}", failures.join("\n"));
}
