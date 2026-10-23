#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "test-only arithmetic on small values"
)]

//! A JPEG that will be shown reduced is decoded reduced (1/2, 1/4 or 1/8 of each block); the
//! result must match what averaging the full-size decode gives.

use super::*;

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// Box average of `img` by `f` in both directions (edge boxes are partial).
fn boxed(img: &Image, f: usize) -> (usize, usize, Vec<u8>) {
    let (w, h) = (img.width as usize, img.height as usize);
    let (ow, oh) = (w.div_ceil(f), h.div_ceil(f));
    let mut out = vec![0u8; ow * oh * 4];
    for oy in 0..oh {
        for ox in 0..ow {
            let mut sum = [0u32; 3];
            let mut n = 0u32;
            for y in oy * f..((oy + 1) * f).min(h) {
                for x in ox * f..((ox + 1) * f).min(w) {
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += u32::from(img.rgba[(y * w + x) * 4 + c]);
                    }
                    n += 1;
                }
            }
            for c in 0..3 {
                out[(oy * ow + ox) * 4 + c] = (sum[c] / n) as u8;
            }
            out[(oy * ow + ox) * 4 + 3] = 255;
        }
    }
    (ow, oh, out)
}

#[test]
fn reduced_decoding_matches_box_averages_of_the_full_one() {
    // (file, longest side that is allowed, expected reduction)
    let cases: &[(&str, u32, usize)] = &[
        ("img-444.jpg", 31, 2),
        ("img-444.jpg", 8, 8),
        ("img-gray.jpg", 31, 2),
        ("img-java-prog-big.jpg", 150, 2),
        ("img-java-prog-big.jpg", 75, 4),
        ("img-java-prog-big.jpg", 38, 8),
        ("img-java-base.jpg", 25, 4),
    ];
    let mut failures = Vec::new();
    for &(name, longest, f) in cases {
        let data = fixture(name);
        let full = decode(&data, &Limits::default()).unwrap();
        let reduced =
            decode(&data, &Limits { max_out_side: longest, ..Limits::default() }).unwrap();
        let (ow, oh, expect) = boxed(&full, f);
        assert_eq!(
            (reduced.width as usize, reduced.height as usize),
            (ow, oh),
            "{name} at {longest}"
        );
        // The test pictures have a fine checkerboard that a reduced transform cannot follow (it
        // drops the frequencies a box average keeps), so the typical difference is what counts.
        let mut diffs: Vec<u8> = Vec::new();
        for (i, (a, b)) in reduced.rgba.chunks_exact(4).zip(expect.chunks_exact(4)).enumerate() {
            // Only the right half of the pictures, where they are smooth.
            if i % ow < ow / 2 {
                continue;
            }
            for c in 0..3 {
                diffs.push(a[c].abs_diff(b[c]));
            }
        }
        diffs.sort_unstable();
        let median = diffs[diffs.len() / 2];
        let p90 = diffs[diffs.len() * 9 / 10];
        if median > 8 || p90 > 60 {
            failures.push(format!(
                "{name} reduced to {longest}: median difference {median}, 90th percentile {p90}"
            ));
        }
        assert!(reduced.facts.has(flag::DOWNSCALED), "{name}");
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));
}
