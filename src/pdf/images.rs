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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Choice {
    Fixed(u8),
    /// Per row, the type with the smallest sum of absolute (signed) residuals.
    MinSum,
}

pub fn png_filter(px: &[u8], row: usize, bpp: usize, how: Choice) -> Vec<u8> {
    let rows = px.len() / row;
    let mut out = Vec::with_capacity(rows * (row + 1));
    let mut cand = vec![0u8; row];
    for r in 0..rows {
        let cur = &px[r * row..(r + 1) * row];
        let prev = if r > 0 { Some(&px[(r - 1) * row..r * row]) } else { None };
        let residuals = |t: u8, out: &mut [u8]| {
            for i in 0..row {
                let a = if i >= bpp { cur[i - bpp] } else { 0 };
                let b = prev.map_or(0, |p| p[i]);
                let c = if i >= bpp { prev.map_or(0, |p| p[i - bpp]) } else { 0 };
                out[i] = cur[i].wrapping_sub(predict(t, a, b, c));
            }
        };
        let t = match how {
            Choice::Fixed(t) => t,
            Choice::MinSum => {
                let mut best = (u64::MAX, 0u8);
                for t in 0..5u8 {
                    residuals(t, &mut cand);
                    let s: u64 = cand.iter().map(|&x| (x as i8).unsigned_abs() as u64).sum();
                    if s < best.0 {
                        best = (s, t);
                    }
                }
                best.1
            }
        };
        out.push(t);
        residuals(t, &mut cand);
        out.extend_from_slice(&cand);
    }
    out
}

/// What `png_filter` gives for the rows of `bands` only (ranges of whole rows, in order): row `a` of a band
/// reads row `a - 1` as the row above, as it does in the whole image. Exact, since a filtered row depends on
/// that row and the one above only (`MinSum` too chooses for each row alone).
pub fn filter_rows(px: &[u8], row: usize, bpp: usize, how: Choice, bands: &[(usize, usize)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bands.iter().map(|&(a, b)| (b - a) * (row + 1)).sum());
    for &(a, b) in bands {
        // From the row above the band, whose own filtered row is left out.
        let from = a.saturating_sub(1);
        let f = png_filter(&px[from * row..b * row], row, bpp, how);
        out.extend_from_slice(&f[(a - from) * (row + 1)..]);
    }
    out
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
    fn filter_rows_is_png_filter_then_slicing() {
        let mut s = 7u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let every =
            [Choice::Fixed(0), Choice::Fixed(1), Choice::Fixed(2), Choice::Fixed(3), Choice::Fixed(4), Choice::MinSum];
        for bpp in [1usize, 3, 4, 6] {
            let row = bpp * 9;
            for nrows in [1usize, 2, 12] {
                // Smooth with some noise, so every filter type gets chosen somewhere.
                let px: Vec<u8> = (0..row * nrows)
                    .map(|i| {
                        ((i % row) * 5 + (i / row) * 3) as u8 ^ if next() % 3 == 0 { (next() % 64) as u8 } else { 0 }
                    })
                    .collect();
                let mut cases: Vec<Vec<(usize, usize)>> =
                    vec![vec![(0, nrows)], vec![(0, 1)], vec![(nrows - 1, nrows)]];
                if nrows == 12 {
                    // At row 0, in the middle and at the end; one-row bands; touching bands.
                    cases.extend([
                        vec![(0, 2), (5, 7), (10, 12)],
                        vec![(3, 4), (4, 5), (8, 9)],
                        vec![(1, 6), (6, 12)],
                        vec![(0, 1), (11, 12)],
                    ]);
                }
                for how in every {
                    let whole = png_filter(&px, row, bpp, how);
                    for bands in &cases {
                        let want: Vec<u8> =
                            bands.iter().flat_map(|&(a, b)| &whole[a * (row + 1)..b * (row + 1)]).copied().collect();
                        assert_eq!(filter_rows(&px, row, bpp, how, bands), want, "{how:?}, bpp {bpp}, {bands:?}");
                    }
                }
            }
        }
    }
}
