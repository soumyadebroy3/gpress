//! Image rows and PNG predictors: undoing them and choosing better ones. A predictor only changes how the
//! samples are stored; the decoded image is the same.

use super::object::{Dict, Obj};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub colors: usize,
    pub bpc: usize,
    pub columns: usize,
}

impl Geometry {
    /// Bytes per row (never 0: `geometry` refuses what doesn't fit, so the product can't wrap around).
    pub fn row(&self) -> usize {
        self.colors.saturating_mul(self.bpc).saturating_mul(self.columns).div_ceil(8).max(1)
    }
    pub fn bpp(&self) -> usize {
        self.colors.saturating_mul(self.bpc).div_ceil(8).max(1)
    }
}

/// Widest image whose rows are worked out (16 million samples; the row length must fit a usize on every
/// target, and nothing real is wider).
const MAX_COLUMNS: i64 = 1 << 24;

/// Colour components of a colour space (None if unknown).
pub fn components(cs: &Obj, resolve: &dyn Fn(&Obj) -> Option<Obj>) -> Option<usize> {
    let cs = if let Obj::Ref(..) = cs { resolve(cs)? } else { cs.clone() };
    match &cs {
        Obj::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"CalGray" | b"G" => Some(1),
            b"DeviceRGB" | b"CalRGB" | b"RGB" => Some(3),
            b"DeviceCMYK" | b"CMYK" => Some(4),
            _ => None,
        },
        Obj::Array(a) => {
            let fam = a.first()?.as_name()?;
            match fam {
                b"Indexed" | b"I" => Some(1),
                b"CalGray" => Some(1),
                b"CalRGB" | b"Lab" => Some(3),
                b"ICCBased" => {
                    let s = resolve(a.get(1)?)?;
                    match s {
                        Obj::Stream(st) => st.dict.get(b"N")?.as_int().map(|n| n as usize),
                        _ => None,
                    }
                }
                b"Separation" => Some(1),
                b"DeviceN" => match a.get(1)? {
                    Obj::Array(names) => Some(names.len()),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

/// The geometry of an image XObject's samples.
pub fn geometry(d: &Dict, resolve: &dyn Fn(&Obj) -> Option<Obj>) -> Option<Geometry> {
    if d.name(b"Subtype") != Some(b"Image") {
        return None;
    }
    let w = d.get(b"Width")?.as_int()?;
    let mask = d.get(b"ImageMask") == Some(&Obj::Bool(true));
    let bpc = if mask { 1 } else { d.get(b"BitsPerComponent")?.as_int()? };
    let colors = if mask { 1 } else { components(d.get(b"ColorSpace")?, resolve)? };
    if !(1..=MAX_COLUMNS).contains(&w) || ![1, 2, 4, 8, 16].contains(&bpc) || colors == 0 || colors > 32 {
        return None;
    }
    // A row of at most u32::MAX bits (512 MiB) on every target, so 32-bit and 64-bit builds agree on which
    // images they work on (the limits above allow 2^24 x 32 x 16 bits, which only a 64-bit usize holds).
    if colors as u64 * bpc as u64 * w as u64 > u32::MAX as u64 {
        return None;
    }
    Some(Geometry { colors, bpc: bpc as usize, columns: w as usize })
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[inline]
fn predict(t: u8, a: u8, b: u8, c: u8) -> u8 {
    match t {
        0 => 0,
        1 => a,
        2 => b,
        3 => ((a as u16 + b as u16) / 2) as u8,
        _ => paeth(a, b, c),
    }
}

/// PNG-predicted rows (type byte + row) back to samples; None unless the data is whole valid rows.
pub fn png_unfilter(data: &[u8], row: usize, bpp: usize) -> Option<Vec<u8>> {
    if row == 0 || !data.len().is_multiple_of(row + 1) {
        return None;
    }
    let rows = data.len() / (row + 1);
    let mut px = vec![0u8; rows * row];
    for r in 0..rows {
        let t = data[r * (row + 1)];
        if t > 4 {
            return None;
        }
        for i in 0..row {
            let a = if i >= bpp { px[r * row + i - bpp] } else { 0 };
            let b = if r > 0 { px[(r - 1) * row + i] } else { 0 };
            let c = if r > 0 && i >= bpp { px[(r - 1) * row + i - bpp] } else { 0 };
            px[r * row + i] = data[r * (row + 1) + 1 + i].wrapping_add(predict(t, a, b, c));
        }
    }
    Some(px)
}

/// TIFF predictor 2 for 8-bit samples back to samples.
pub fn tiff_unfilter(data: &[u8], row: usize, bpp: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    for r in out.chunks_mut(row.max(1)) {
        for i in bpp..r.len() {
            r[i] = r[i].wrapping_add(r[i - bpp]);
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Fixed(u8),
    /// Per row, the type with the smallest sum of absolute (signed) residuals.
    MinSum,
}

/// Row `r` of `px` stored with PNG predictor type `t`, into `out` (one byte per sample byte).
fn residuals(px: &[u8], row: usize, bpp: usize, r: usize, t: u8, out: &mut [u8]) {
    let cur = &px[r * row..(r + 1) * row];
    let prev = if r > 0 { Some(&px[(r - 1) * row..r * row]) } else { None };
    for i in 0..row {
        let a = if i >= bpp { cur[i - bpp] } else { 0 };
        let b = prev.map_or(0, |p| p[i]);
        let c = if i >= bpp { prev.map_or(0, |p| p[i - bpp]) } else { 0 };
        out[i] = cur[i].wrapping_sub(predict(t, a, b, c));
    }
}

/// The type `Choice::MinSum` picks for row `r` (the first of equal sums). `cand` is scratch: it is left with
/// the last type's residuals, not the chosen one's.
fn min_sum_type(px: &[u8], row: usize, bpp: usize, r: usize, cand: &mut [u8]) -> u8 {
    let mut best = (u64::MAX, 0u8);
    for t in 0..5u8 {
        residuals(px, row, bpp, r, t, cand);
        let s: u64 = cand.iter().map(|&x| (x as i8).unsigned_abs() as u64).sum();
        if s < best.0 {
            best = (s, t);
        }
    }
    best.1
}

pub fn png_filter(px: &[u8], row: usize, bpp: usize, how: Choice) -> Vec<u8> {
    let rows = px.len() / row;
    let mut out = Vec::with_capacity(rows * (row + 1));
    let mut cand = vec![0u8; row];
    for r in 0..rows {
        let t = match how {
            Choice::Fixed(t) => t,
            Choice::MinSum => min_sum_type(px, row, bpp, r, &mut cand),
        };
        out.push(t);
        residuals(px, row, bpp, r, t, &mut cand);
        out.extend_from_slice(&cand);
    }
    out
}

/// The predictor to store the samples with on the fast path, chosen without encoding: None (as they are),
/// one of the PNG types for every row, or the per-row minimum sum (libpng's heuristic), whichever costs the
/// fewest bits under an order-0 entropy estimate of the rows in `bands` (`(first, end)` pairs; the first of
/// equal costs wins). Below 8 bits per component always None: a predictor works on whole bytes, so it mixes
/// neighbouring samples (on the sample's 44 such images it never paid). The caller keeps the samples as they
/// are when the chosen predictor's encode is not smaller, so a wrong guess costs time, never size.
pub fn estimate_choice(px: &[u8], row: usize, bpp: usize, bpc: usize, bands: &[(usize, usize)]) -> Option<Choice> {
    if bpc < 8 || row == 0 {
        return None;
    }
    let rows = px.len() / row;
    // Counts for: as they are, PNG types 1 to 4, minimum sum.
    let mut counts = [[0u32; 256]; 6];
    let mut cand = vec![0u8; row];
    for &(a, b) in bands {
        for r in a..b.min(rows) {
            for &x in &px[r * row..(r + 1) * row] {
                counts[0][x as usize] += 1;
            }
            for t in 1..5u8 {
                residuals(px, row, bpp, r, t, &mut cand);
                // The type byte is stored in front of every row.
                counts[t as usize][t as usize] += 1;
                for &x in &cand {
                    counts[t as usize][x as usize] += 1;
                }
            }
            let t = min_sum_type(px, row, bpp, r, &mut cand);
            residuals(px, row, bpp, r, t, &mut cand);
            counts[5][t as usize] += 1;
            for &x in &cand {
                counts[5][x as usize] += 1;
            }
        }
    }
    let mut best = (entropy_bits(&counts[0]), None);
    for (k, how) in [
        (1, Choice::Fixed(1)),
        (2, Choice::Fixed(2)),
        (3, Choice::Fixed(3)),
        (4, Choice::Fixed(4)),
        (5, Choice::MinSum),
    ] {
        let bits = entropy_bits(&counts[k]);
        if bits < best.0 {
            best = (bits, Some(how));
        }
    }
    best.1
}

/// Order-0 entropy of byte counts, in 1/65536 bits, with integer arithmetic only (so the choice is the same on
/// every machine): the sum of c * (log2 n - log2 c) with a piecewise-linear log2.
fn entropy_bits(counts: &[u32; 256]) -> u64 {
    fn log2(x: u64) -> u64 {
        let k = 63 - x.leading_zeros() as u64;
        (k << 16) + (((x - (1 << k)) << 16) >> k)
    }
    let n: u64 = counts.iter().map(|&c| c as u64).sum();
    if n == 0 {
        return 0;
    }
    let ln = log2(n);
    counts.iter().filter(|&&c| c > 0).map(|&c| c as u64 * (ln - log2(c as u64))).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_round_trip() {
        let mut s = 3u64;
        let (row, bpp) = (30usize, 3usize);
        let px: Vec<u8> = (0..row * 20)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((i % row) as u8).wrapping_mul(3).wrapping_add((s % 5) as u8)
            })
            .collect();
        for how in
            [Choice::Fixed(0), Choice::Fixed(1), Choice::Fixed(2), Choice::Fixed(3), Choice::Fixed(4), Choice::MinSum]
        {
            let f = png_filter(&px, row, bpp, how);
            assert_eq!(png_unfilter(&f, row, bpp).unwrap(), px);
        }
    }

    #[test]
    fn the_estimate_scores_the_minimum_sum_with_its_own_residuals() {
        // Every 4 rows: random, the same again, a ramp, the same again. Per row the best type changes (Up on the
        // copies, Sub on the ramp), so the minimum sum beats every fixed type. Scored with the last type's
        // residuals (Paeth's) instead of the chosen ones it only ties with Paeth, and Paeth would win.
        let (row, bpp) = (64usize, 1usize);
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut px = Vec::new();
        for k in 0..16u8 {
            let random: Vec<u8> = (0..row)
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    s as u8
                })
                .collect();
            let ramp: Vec<u8> = (0..row).map(|i| k.wrapping_mul(17).wrapping_add(3 * i as u8)).collect();
            for r in [&random, &random, &ramp, &ramp] {
                px.extend_from_slice(r);
            }
        }
        let nrows = px.len() / row;
        assert_eq!(estimate_choice(&px, row, bpp, 8, &[(0, nrows)]), Some(Choice::MinSum));
        // In a row of 100s under a row of 1s, the minimum sum is Sub (1, 0, 0, ...), not Paeth (157, 0, ...).
        let two: Vec<u8> = [vec![100u8; 16], vec![1u8; 16]].concat();
        let mut cand = vec![0u8; 16];
        assert_eq!(min_sum_type(&two, 16, 1, 1, &mut cand), 1);
    }

    #[test]
    fn geometry_is_the_same_on_every_target() {
        let resolve = |o: &Obj| Some(o.clone());
        let image = |w: i64, bpc: i64, cs: Obj| {
            let mut d = Dict::default();
            d.set(b"Subtype", Obj::Name(b"Image".to_vec()));
            d.set(b"Width", Obj::Int(w));
            d.set(b"BitsPerComponent", Obj::Int(bpc));
            d.set(b"ColorSpace", cs);
            d
        };
        let names = |n: usize| Obj::Array((0..n).map(|k| Obj::Name(format!("C{k}").into_bytes())).collect());
        let devicen =
            |n: usize| Obj::Array(vec![Obj::Name(b"DeviceN".to_vec()), names(n), Obj::Name(b"DeviceRGB".to_vec())]);
        // 2^24 x 32 x 16 bits per row: refused everywhere (a 32-bit usize can't hold the bit count).
        assert_eq!(geometry(&image(1 << 24, 16, devicen(32)), &resolve), None);
        // Just over and just under u32::MAX bits.
        assert_eq!(geometry(&image(1 << 24, 16, devicen(16)), &resolve), None);
        let g = geometry(&image((1 << 24) - 1, 16, devicen(16)), &resolve).unwrap();
        assert_eq!(g.row(), (((1usize << 24) - 1) * 16 * 16).div_ceil(8));
        assert!(geometry(&image(4000, 8, Obj::Name(b"DeviceRGB".to_vec())), &resolve).is_some());
    }

    #[test]
    fn the_estimate_wants_a_predictor_only_where_it_helps() {
        let (row, rows) = (300usize, 200usize);
        let all = [(0usize, rows)];
        // A smooth gradient: a predictor leaves almost nothing.
        let smooth: Vec<u8> = (0..row * rows).map(|i| ((i % row) + (i / row) * 2) as u8).collect();
        assert!(estimate_choice(&smooth, row, 3, 8, &all).is_some());
        // Identical rows: Up leaves only zeros below the first row, and so do Paeth (on identical rows it predicts
        // the row above) and the minimum sum; Paeth also takes the first row as Sub does, which suits this one.
        let stripes: Vec<u8> = (0..row * rows).map(|i| ((i % row) * 37 % 251) as u8).collect();
        assert!(matches!(
            estimate_choice(&stripes, row, 3, 8, &all),
            Some(Choice::Fixed(2) | Choice::Fixed(4) | Choice::MinSum)
        ));
        // Noise: the residuals are noise as well, and the type bytes cost extra.
        let mut s = 9u64;
        let noise: Vec<u8> = (0..row * rows)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u8
            })
            .collect();
        assert_eq!(estimate_choice(&noise, row, 3, 8, &all), None);
        // Below 8 bits per component never; nor for a blank image (nothing to gain).
        assert_eq!(estimate_choice(&smooth, row, 1, 4, &all), None);
        assert_eq!(estimate_choice(&vec![255u8; row * rows], row, 1, 8, &all), None);
        // Bands past the end are cut, and an empty band list wants nothing.
        assert!(estimate_choice(&smooth, row, 3, 8, &[(0, 10), (150, 1000)]).is_some());
        assert_eq!(estimate_choice(&smooth, row, 3, 8, &[]), None);
        // The integer entropy: two equally likely bytes cost one bit each.
        let mut c = [0u32; 256];
        c[1] = 1000;
        c[2] = 1000;
        assert_eq!(entropy_bits(&c), 2000 << 16);
    }
}
