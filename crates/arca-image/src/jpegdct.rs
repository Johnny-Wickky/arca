//! Inverse DCT of JPEG blocks, at full size and reduced to 1/2, 1/4 and 1/8.
//!
//! The full-size transform is the accurate integer algorithm of the IJG library (`jidctint`).
//! The reduced ones use only the low-frequency corner of the block (4 × 4, 2 × 2 or just the DC
//! term) and give a block of 4 × 4, 2 × 2 or 1 sample directly: a picture that is going to be
//! reduced anyway is then decoded without ever computing most of its pixels.
//!
//! Everything is computed in 64 bits: a hostile file can put any 16-bit value in a coefficient,
//! and an overflow in release builds with `overflow-checks` would end the guest. The input is
//! clamped first, so no product can leave the range of an `i64`.

const CONST_BITS: i64 = 13;
const PASS1_BITS: i64 = 2;
const F_0_298: i64 = 2446;
const F_0_390: i64 = 3196;
const F_0_541: i64 = 4433;
const F_0_765: i64 = 6270;
const F_0_899: i64 = 7373;
const F_1_175: i64 = 9633;
const F_1_501: i64 = 12299;
const F_1_847: i64 = 15137;
const F_1_961: i64 = 16069;
const F_2_053: i64 = 16819;
const F_2_562: i64 = 20995;
const F_3_072: i64 = 25172;

/// `½ · C(u) · cos((2x + 1)·u·π / 2N)` in 13-bit fixed point, indexed `[x][u]`, for N = 4.
const T4: [[i64; 4]; 4] = [
    [2896, 3784, 2896, 1567],
    [2896, 1567, -2896, -3784],
    [2896, -1567, -2896, 3784],
    [2896, -3784, 2896, -1567],
];
/// The same for N = 2.
const T2: [[i64; 2]; 2] = [[2896, 2896], [2896, -2896]];

fn descale(x: i64, n: i64) -> i64 {
    (x + (1 << (n - 1))) >> n
}

fn clamp8(v: i64) -> u8 {
    u8::try_from(v.clamp(0, 255)).unwrap_or(0)
}

/// One-dimensional inverse DCT of 8 values, scaled up by `2^CONST_BITS` (not yet descaled).
fn idct1d(i: [i64; 8]) -> [i64; 8] {
    let [i0, i1, i2, i3, i4, i5, i6, i7] = i;
    let z1 = (i2 + i6) * F_0_541;
    let t2 = z1 - i6 * F_1_847;
    let t3 = z1 + i2 * F_0_765;
    let t0 = (i0 + i4) << CONST_BITS;
    let t1 = (i0 - i4) << CONST_BITS;
    let (t10, t13, t11, t12) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
    let (mut o0, mut o1, mut o2, mut o3) = (i7, i5, i3, i1);
    let (z1, z2, z3, z4) = (o0 + o3, o1 + o2, o0 + o2, o1 + o3);
    let z5 = (z3 + z4) * F_1_175;
    o0 *= F_0_298;
    o1 *= F_2_053;
    o2 *= F_3_072;
    o3 *= F_1_501;
    let (z1, z2) = (-z1 * F_0_899, -z2 * F_2_562);
    let (z3, z4) = (-z3 * F_1_961 + z5, -z4 * F_0_390 + z5);
    o0 += z1 + z3;
    o1 += z2 + z4;
    o2 += z2 + z3;
    o3 += z1 + z4;
    [t10 + o3, t11 + o2, t12 + o1, t13 + o0, t13 - o0, t12 - o1, t11 - o2, t10 - o3]
}

/// The dequantised coefficient `i` (natural order), clamped to a range the arithmetic can take.
fn coef(coefs: &[i16], q: &[u16; 64], i: usize) -> i64 {
    let c = i64::from(coefs.get(i).copied().unwrap_or(0));
    let q = i64::from(q.get(i).copied().unwrap_or(1));
    (c * q).clamp(-32_768, 32_767)
}

/// Full-size transform: 64 samples.
#[allow(
    clippy::indexing_slicing,
    reason = "every index below is `row * 8 + col` with row and col bounded by 8, into arrays of 64"
)]
fn idct8(coefs: &[i16], q: &[u16; 64], out: &mut [u8]) {
    let mut ws = [0i64; 64];
    for col in 0..8 {
        let g = [
            coef(coefs, q, col),
            coef(coefs, q, col + 8),
            coef(coefs, q, col + 16),
            coef(coefs, q, col + 24),
            coef(coefs, q, col + 32),
            coef(coefs, q, col + 40),
            coef(coefs, q, col + 48),
            coef(coefs, q, col + 56),
        ];
        if g[1..].iter().all(|&v| v == 0) {
            let dc = g[0] << PASS1_BITS;
            for row in 0..8 {
                ws[row * 8 + col] = dc;
            }
            continue;
        }
        let o = idct1d(g);
        for row in 0..8 {
            ws[row * 8 + col] = descale(o[row], CONST_BITS - PASS1_BITS);
        }
    }
    for (row, line) in out.chunks_exact_mut(8).take(8).enumerate() {
        let r: [i64; 8] = [
            ws[row * 8],
            ws[row * 8 + 1],
            ws[row * 8 + 2],
            ws[row * 8 + 3],
            ws[row * 8 + 4],
            ws[row * 8 + 5],
            ws[row * 8 + 6],
            ws[row * 8 + 7],
        ];
        if r[1..].iter().all(|&v| v == 0) {
            // A row that has only its first term is flat.
            let v = clamp8(descale(r[0] << CONST_BITS, CONST_BITS + PASS1_BITS + 3) + 128);
            line.fill(v);
            continue;
        }
        let o = idct1d(r);
        for (col, px) in line.iter_mut().enumerate() {
            *px = clamp8(descale(o[col], CONST_BITS + PASS1_BITS + 3) + 128);
        }
    }
}

/// Reduced transform from the `N × N` low-frequency corner: `N × N` samples.
#[allow(
    clippy::indexing_slicing,
    reason = "every index is below N (at most 4) into arrays of N, or `v * 8 + u` below 64"
)]
fn small<const N: usize>(coefs: &[i16], q: &[u16; 64], t: &[[i64; N]; N], out: &mut [u8]) {
    let mut f = [[0i64; N]; N];
    for (v, row) in f.iter_mut().enumerate() {
        for (u, value) in row.iter_mut().enumerate() {
            *value = coef(coefs, q, v * 8 + u);
        }
    }
    // Rows: g[v][x] = sum over u of t[x][u] * f[v][u].
    let mut g = [[0i64; N]; N];
    for v in 0..N {
        for x in 0..N {
            let mut sum = 0;
            for u in 0..N {
                sum += t[x][u] * f[v][u];
            }
            g[v][x] = sum;
        }
    }
    // Columns, and the two scalings of 2^13 at once.
    for (y, line) in out.chunks_exact_mut(N).take(N).enumerate() {
        for (x, px) in line.iter_mut().enumerate() {
            let mut sum = 0;
            for v in 0..N {
                sum += t[y][v] * g[v][x];
            }
            *px = clamp8(descale(sum, 2 * CONST_BITS) + 128);
        }
    }
}

/// Dequantises one block (`coefs` in natural order) and writes its samples: `(8 >> shift)²` of
/// them, row by row. `dc_only` may be set when the caller knows that every AC term is zero.
pub(crate) fn idct_block(shift: u32, coefs: &[i16], q: &[u16; 64], dc_only: bool, out: &mut [u8]) {
    let side = 8usize >> shift;
    let Some(out) = out.get_mut(..side * side) else { return };
    let flat = dc_only || coefs.get(1..).is_none_or(|ac| ac.iter().all(|&c| c == 0));
    if flat || shift >= 3 {
        // The average of the block: DC / 8.
        let v = clamp8(descale(coef(coefs, q, 0), 3) + 128);
        out.fill(v);
        return;
    }
    match shift {
        0 => idct8(coefs, q, out),
        1 => small::<4>(coefs, q, &T4, out),
        _ => small::<2>(coefs, q, &T2, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_block_is_flat_at_every_size() {
        let mut q = [1u16; 64];
        q[0] = 8;
        let mut coefs = [0i16; 64];
        coefs[0] = 10; // 10 * 8 / 8 = 10 above the mid-grey
        for shift in 0..4 {
            let side = 8usize >> shift;
            let mut out = vec![0u8; side * side];
            idct_block(shift, &coefs, &q, false, &mut out);
            assert!(out.iter().all(|&v| v == 138), "shift {shift}: {out:?}");
        }
    }

    #[test]
    fn reduced_transforms_follow_the_full_one() {
        // A smooth gradient: the 4x4 and 2x2 results are close to box averages of the 8x8 one.
        let q = [1u16; 64];
        let mut coefs = [0i16; 64];
        coefs[0] = 400;
        coefs[1] = -120; // horizontal ramp
        coefs[8] = 90; // vertical ramp
        coefs[9] = 30;
        let mut full = [0u8; 64];
        idct_block(0, &coefs, &q, false, &mut full);
        for (shift, side) in [(1u32, 4usize), (2, 2), (3, 1)] {
            let mut small = vec![0u8; side * side];
            idct_block(shift, &coefs, &q, false, &mut small);
            let k = 8 / side;
            for y in 0..side {
                for x in 0..side {
                    let mut sum = 0i32;
                    for dy in 0..k {
                        for dx in 0..k {
                            sum += i32::from(full[(y * k + dy) * 8 + x * k + dx]);
                        }
                    }
                    let avg = sum / i32::try_from(k * k).unwrap();
                    let got = i32::from(small[y * side + x]);
                    assert!(
                        (avg - got).abs() <= 6,
                        "shift {shift} ({x},{y}): box {avg}, reduced {got}"
                    );
                }
            }
        }
    }

    #[test]
    fn extreme_coefficients_cannot_overflow() {
        let q = [255u16; 64];
        let coefs = [i16::MAX; 64];
        let mut out = [0u8; 64];
        for shift in 0..4 {
            idct_block(shift, &coefs, &q, false, &mut out);
        }
        let coefs = [i16::MIN; 64];
        for shift in 0..4 {
            idct_block(shift, &coefs, &q, false, &mut out);
        }
    }
}
