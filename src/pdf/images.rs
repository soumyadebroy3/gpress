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
    // At most 2^24 x 32 x 16 bits: fits a usize on 64-bit targets; checked for the others.
    colors.checked_mul(bpc as usize)?.checked_mul(w as usize)?;
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

/// The type `Choice::MinSum` picks for row `r` (the first of equal sums), its residuals left in `cand`.
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

/// Whether the samples are worth encoding a second time with `Choice::MinSum` (libpng's heuristic), judged
/// without encoding: never below 8 bits per component (a predictor works on whole bytes, so it mixes
/// neighbouring samples; on the sample's 44 such images it never paid), else when the rows in `bands`
/// (`(first, end)` pairs) cost fewer bits under an order-0 entropy estimate with the predictor than without.
/// The caller encodes both and keeps the smaller, so a wrong guess costs time, never size. On the 314 images
/// of 8 or 16 bits in the sample this missed no image the predictor made smaller.
pub fn min_sum_pays(px: &[u8], row: usize, bpp: usize, bpc: usize, bands: &[(usize, usize)]) -> bool {
    if bpc < 8 || row == 0 {
        return false;
    }
    let rows = px.len() / row;
    let (mut plain, mut predicted) = ([0u32; 256], [0u32; 256]);
    let mut cand = vec![0u8; row];
    for &(a, b) in bands {
        for r in a..b.min(rows) {
            for &x in &px[r * row..(r + 1) * row] {
                plain[x as usize] += 1;
            }
            // The type byte counts too: it is stored in front of every row.
            predicted[min_sum_type(px, row, bpp, r, &mut cand) as usize] += 1;
            for &x in &cand {
                predicted[x as usize] += 1;
            }
        }
    }
    entropy_bits(&predicted) < entropy_bits(&plain)
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
    fn the_estimate_wants_a_predictor_only_where_it_helps() {
        let (row, rows) = (300usize, 200usize);
        let all = [(0usize, rows)];
        // A smooth gradient: the predictor leaves almost nothing.
        let smooth: Vec<u8> = (0..row * rows).map(|i| ((i % row) + (i / row) * 2) as u8).collect();
        assert!(min_sum_pays(&smooth, row, 3, 8, &all));
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
        assert!(!min_sum_pays(&noise, row, 3, 8, &all));
        // Below 8 bits per component never; nor for a blank image (nothing to gain).
        assert!(!min_sum_pays(&smooth, row, 1, 4, &all));
        assert!(!min_sum_pays(&vec![255u8; row * rows], row, 1, 8, &all));
        // Bands past the end are cut, and an empty band list wants nothing.
        assert!(min_sum_pays(&smooth, row, 3, 8, &[(0, 10), (150, 1000)]));
        assert!(!min_sum_pays(&smooth, row, 3, 8, &[]));
        // The integer entropy: two equally likely bytes cost one bit each.
        let mut c = [0u32; 256];
        c[1] = 1000;
        c[2] = 1000;
        assert_eq!(entropy_bits(&c), 2000 << 16);
    }
}
