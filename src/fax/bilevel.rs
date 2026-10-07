//! A context-mixing model for bilevel images (1 bit per pixel), used by the peyaji format for fax images.
//!
//! Each pixel is predicted from pixels already coded around it, in the current row and the three rows
//! above: JBIG2's template 0 (16 pixels), a small template, a large 28-pixel template (hashed), a wide
//! template, and a context of the current run and the nearest edges in the row above. The predictions are
//! mixed by a trained mixer and refined by an adaptive probability map, with 16-bit precision so that white
//! areas cost almost nothing. A row equal to the row above costs one decision (typical prediction), and a
//! pixel whose whole 28-pixel neighbourhood is white is coded with one small counter instead of the full
//! model (most pixels of a page; much faster).
//!
//! Everything is integer arithmetic, so encoder and decoder agree on every platform. The settings are
//! constants of the format.

use super::ccitt::{expand_row, row_changes};
use crate::cm::{Coder, Counter};
use std::sync::OnceLock;

// ---------------------------------------------------------------- logistic functions, 16-bit precision

/// Stretched probabilities are ln(p / (1 - p)) * 256, within +-SMAX (p from 1/65536 to 65535/65536).
const SMAX: i32 = 2840;
/// e^(-1/256) in Q32 fixed point; the table of e^(-k/256) is built from it by integer multiplication.
const EXP_STEP: u64 = 4_278_222_805;

struct Logistic {
    /// squash[x + SMAX]: the probability (in 1/65536) of stretch value x.
    squash: Vec<u16>,
    /// stretch[p]: the stretch value of probability p (in 1/65536).
    stretch: Vec<i16>,
}

fn logistic() -> &'static Logistic {
    static L: OnceLock<Logistic> = OnceLock::new();
    L.get_or_init(|| {
        let n = SMAX as usize;
        let mut e = vec![0u64; n + 1];
        e[0] = 1 << 32;
        for k in 1..=n {
            e[k] = (e[k - 1] * EXP_STEP) >> 32;
        }
        let mut squash = vec![0u16; 2 * n + 1];
        for x in 0..=n {
            let d = (1u64 << 32) + e[x];
            let p = (((65536u64 << 32) + d / 2) / d).clamp(1, 65535) as u16;
            squash[n + x] = p;
            squash[n - x] = (65536 - p as u32).clamp(1, 65535) as u16;
        }
        // stretch: the smallest x whose probability reaches p.
        let mut stretch = vec![0i16; 65536];
        let mut x = 0usize;
        for p in 0..65536usize {
            while x < 2 * n && (squash[x] as usize) < p {
                x += 1;
            }
            stretch[p] = (x as i32 - SMAX) as i16;
        }
        Logistic { squash, stretch }
    })
}

#[inline]
fn squash(l: &Logistic, x: i32) -> u32 {
    l.squash[(x.clamp(-SMAX, SMAX) + SMAX) as usize] as u32
}

#[inline]
fn stretch(l: &Logistic, p: u32) -> i32 {
    l.stretch[p as usize] as i32
}

// ---------------------------------------------------------------- the model

/// Context models; the mixer gets their stretched predictions plus a bias.
const N: usize = 7;
const INPUTS: usize = N + 1;
/// Table sizes (bits) of the context models; 0 marks the hashed ones (sized by `new`).
const TABLE_BITS: [u32; N] = [16, 12, 0, 16, 13, 13, 0];
/// Pixels of white margin around each row buffer.
const M: usize = 16;
/// Learning rate of the mixer (a shift; larger is slower).
const MIX_SHIFT: i32 = 15;
/// Rate of the adaptive probability map (a shift).
const APM_SHIFT: i32 = 6;
const LIMIT: u32 = 1023;
const LIMIT_HASHED: u32 = 1023;
/// Bits of the hashed tables of the large templates, at most (smaller for small files, see `new`).
const BIG_BITS: u32 = 22;

pub struct Bilevel {
    l: &'static Logistic,
    big_bits: u32,
    tables: Vec<Vec<Counter>>,
    fast: [Counter; 8],
    same_row: [Counter; 2],
    weights: Vec<i32>,
    weights2: Vec<i32>,
    apm: Vec<u16>,
    apm2: Vec<u16>,
    width: usize,
    /// rows[0] is the row being coded, rows[1..5] the four rows above (white above the image).
    rows: [Vec<u8>; 5],
    /// For each column, how many rows above have the colour of the row just above (capped).
    vrun: Vec<u8>,
    prev: Vec<u32>,
    prev_same: usize,
}

impl Bilevel {
    /// `big_bits`: size of the hashed tables (both sides must use the same; see `part`).
    pub fn new(big_bits: u32) -> Bilevel {
        let big_bits = big_bits.clamp(12, BIG_BITS);
        let l = logistic();
        let mut apm = vec![0u16; (1 << 16) * 33];
        for c in 0..(1 << 16) {
            for j in 0..33 {
                apm[c * 33 + j] = squash(l, (j as i32 - 16) * SMAX / 16) as u16;
            }
        }
        let apm2 = apm[..(1 << 13) * 33].to_vec();
        Bilevel {
            l,
            big_bits,
            apm2,
            tables: TABLE_BITS.iter().map(|&b| vec![Counter::new(); 1 << if b == 0 { big_bits } else { b }]).collect(),
            fast: [Counter::new(); 8],
            same_row: [Counter::new(); 2],
            weights: vec![(1 << 16) * 3 / 10; INPUTS * 32],
            weights2: vec![(1 << 16) * 3 / 10; INPUTS * 128],
            apm,
            width: 0,
            rows: [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            vrun: Vec::new(),
            prev: Vec::new(),
            prev_same: 0,
        }
    }

    /// Starts a new image (the statistics learned so far are kept).
    pub fn start(&mut self, width: usize) {
        self.width = width;
        for r in self.rows.iter_mut() {
            r.clear();
            r.resize(width + 2 * M, 0);
        }
        self.vrun.clear();
        self.vrun.resize(width + 2 * M, 0);
        self.prev.clear();
        self.prev_same = 0;
    }

    /// Codes one row: encoding when `row` (its changing elements) is given, decoding otherwise. Returns the
    /// row's changing elements in `out`.
    pub fn code_row<C: Coder>(&mut self, c: &mut C, row: Option<&[u32]>, out: &mut Vec<u32>) {
        let w = self.width;
        self.rows.rotate_right(1);
        // Typical prediction: the same row as the one above.
        let same_in = row.map(|r| (r == self.prev.as_slice()) as u32);
        let cx = &mut self.same_row[self.prev_same];
        let same = c.code(same_in, cx.p12());
        cx.update(same, LIMIT);
        self.prev_same = same as usize;
        if same == 1 {
            let (a, b) = self.rows.split_at_mut(1);
            a[0].copy_from_slice(&b[0]);
            for v in self.vrun[M..M + w].iter_mut() {
                *v = v.saturating_add(1);
            }
            out.clear();
            out.extend_from_slice(&self.prev);
            return;
        }
        if let Some(r) = row {
            expand_row(r, &mut self.rows[0][M..M + w]);
        }
        let encoding = row.is_some();
        let l = self.l;
        let (r0, rest) = self.rows.split_at_mut(1);
        let cur = &mut r0[0];
        let (row1, row2, row3, row4) = (&rest[0], &rest[1], &rest[2], &rest[3]);
        let vrun = &mut self.vrun;
        let vq = |v: u8| -> u32 {
            match v {
                0..=1 => 0,
                2 => 1,
                3 => 2,
                4..=5 => 3,
                6..=8 => 4,
                9..=16 => 5,
                17..=32 => 6,
                _ => 7,
            }
        };
        // Windows of the rows above: bit 0 is x+8, bit 16 is x-8.
        let (mut a1, mut a2, mut a3, mut a4) = (0u32, 0u32, 0u32, 0u32);
        for i in 0..16 {
            a1 = a1 << 1 | row1[M - 8 + i] as u32;
            a2 = a2 << 1 | row2[M - 8 + i] as u32;
            a3 = a3 << 1 | row3[M - 8 + i] as u32;
            a4 = a4 << 1 | row4[M - 8 + i] as u32;
        }
        // The current row: bit 0 is x-1.
        let mut c0 = 0u32;
        let mut run = 0u32;
        for x in 0..w {
            a1 = (a1 << 1 | row1[M + x + 8] as u32) & 0x1FFFF;
            a2 = (a2 << 1 | row2[M + x + 8] as u32) & 0x1FFFF;
            a3 = (a3 << 1 | row3[M + x + 8] as u32) & 0x1FFFF;
            a4 = (a4 << 1 | row4[M + x + 8] as u32) & 0x1FFFF;
            let big = (c0 & 0x7F) | ((a1 >> 3) & 0x7FF) << 7 | ((a2 >> 5) & 0x7F) << 18 | ((a3 >> 7) & 7) << 25;
            let px_in = if encoding { Some(cur[M + x] as u32) } else { None };
            let bit;
            if big == 0 {
                // A white neighbourhood: one counter, chosen by what is a little farther away.
                let f = ((a1 & 0x7) != 0) as usize | (((a2 & 0x1F) | (a3 & 0x3F)) != 0) as usize * 2 | (c0 >> 7) as usize * 4;
                let ct = &mut self.fast[f];
                bit = c.code16(px_in, ct.p16());
                ct.update(bit, LIMIT);
            } else {
                let t16 = ((c0 & 0xF) | ((a1 >> 5) & 0x7F) << 4 | ((a2 >> 6) & 0x1F) << 11) as usize;
                let small = ((c0 & 7) | ((a1 >> 5) & 0x3F) << 3 | ((a2 >> 7) & 7) << 9) as usize;
                let hb = (big.wrapping_mul(0x9E37_79B1) >> (32 - self.big_bits)) as usize;
                let wide = ((c0 & 0xFF) | (a1 & 0x7) << 8 | ((a1 >> 14) & 0x7) << 11 | ((a1 >> 8) & 1) << 14 | ((a2 >> 8) & 1) << 15) as usize;
                // Distance to the next change to the right in the row above, the pixel above, the run so far.
                let above = (a1 >> 8) & 1;
                let v = (a1 ^ if above == 1 { 0xFF } else { 0 }) & 0xFF;
                let right = if v == 0 { 9 } else { 8 - (31 - v.leading_zeros()) };
                let rq = match run {
                    0..=3 => run,
                    4..=5 => 4,
                    6..=8 => 5,
                    9..=13 => 6,
                    14..=20 => 7,
                    _ => 8,
                };
                let runs = (rq | (c0 & 1) << 4 | right << 5 | above << 9 | ((a1 >> 7) & 1) << 10 | ((a1 >> 9) & 1) << 11 | ((a2 >> 8) & 1) << 12) as usize;
                // Vertical runs here and next to here.
                let vert = (vq(vrun[M + x]) | above << 3 | vq(vrun[M + x - 1]) << 4 | vq(vrun[M + x + 1]) << 7 | (c0 & 1) << 10 | ((a1 >> 7) & 1) << 11 | ((a1 >> 9) & 1) << 12) as usize;
                // Four rows up and wider: more of a glyph's shape.
                let deep = (c0 as u64 | (((a1 >> 1) & 0x7FFF) as u64) << 8 | (((a2 >> 3) & 0x7FF) as u64) << 23 | (((a3 >> 5) & 0x7F) as u64) << 34 | (((a4 >> 6) & 0x1F) as u64) << 41)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15);
                let hd = (deep >> (64 - self.big_bits)) as usize;
                let cxs = [t16, small, hb, wide, runs, vert, hd];
                let mut st = [0i32; INPUTS];
                for i in 0..N {
                    st[i] = stretch(l, self.tables[i][cxs[i]].p16());
                }
                st[N] = 256;
                let sel = ((c0 & 3) | ((a1 >> 7) & 7) << 2) as usize * INPUTS;
                let wts = &mut self.weights[sel..sel + INPUTS];
                let sel2 = (rq | right.min(7) << 4) as usize * INPUTS;
                let wts2 = &mut self.weights2[sel2..sel2 + INPUTS];
                let (mut dot, mut dot2) = (0i64, 0i64);
                for i in 0..INPUTS {
                    dot += st[i] as i64 * wts[i] as i64;
                    dot2 += st[i] as i64 * wts2[i] as i64;
                }
                let s1 = ((dot >> 16) as i32).clamp(-SMAX, SMAX);
                let s2 = ((dot2 >> 16) as i32).clamp(-SMAX, SMAX);
                let (p1, p2) = (squash(l, s1), squash(l, s2));
                let sm = (s1 + s2) / 2;
                let pm = squash(l, sm);
                // Adaptive probability map over the stretched mixer output, by template 0.
                let pos = (sm + SMAX) * 32 * 64 / (2 * SMAX);
                let (lo, fr) = ((pos >> 6) as usize, pos & 63);
                let base = t16 * 33;
                let pa = ((self.apm[base + lo] as i32 * (64 - fr) + self.apm[base + (lo + 1).min(32)] as i32 * fr) >> 6) as u32;
                let base2 = vert * 33;
                let pb = ((self.apm2[base2 + lo] as i32 * (64 - fr) + self.apm2[base2 + (lo + 1).min(32)] as i32 * fr) >> 6) as u32;
                let p = ((pm + pa + 2 * pb + 2) >> 2).clamp(1, 65535);
                bit = c.code16(px_in, p);
                // Learn.
                for i in 0..N {
                    self.tables[i][cxs[i]].update(bit, if i == 2 || i == 6 { LIMIT_HASHED } else { LIMIT });
                }
                let err = ((bit as i64) << 16) - p1 as i64;
                let err2 = ((bit as i64) << 16) - p2 as i64;
                for i in 0..INPUTS {
                    wts[i] += ((st[i] as i64 * err) >> MIX_SHIFT) as i32;
                    wts2[i] += ((st[i] as i64 * err2) >> MIX_SHIFT) as i32;
                }
                let near = if fr >= 32 { (lo + 1).min(32) } else { lo };
                let target: i32 = if bit == 1 { 65535 } else { 0 };
                let a = self.apm[base + near] as i32;
                self.apm[base + near] = (a + ((target - a) >> APM_SHIFT)) as u16;
                let a = self.apm2[base2 + near] as i32;
                self.apm2[base2 + near] = (a + ((target - a) >> APM_SHIFT)) as u16;
            }
            if bit as u32 == (c0 & 1) {
                run += 1;
            } else {
                run = 0;
            }
            cur[M + x] = bit as u8;
            c0 = (c0 << 1 | bit) & 0xFF;
        }
        for x in M..M + w {
            vrun[x] = if cur[x] == row1[x] { vrun[x].saturating_add(1) } else { 1 };
        }
        if encoding {
            out.clear();
            out.extend_from_slice(row.unwrap());
        } else {
            row_changes(&cur[M..M + w], out);
        }
        self.prev.clear();
        self.prev.extend_from_slice(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cm::{Decoder, Encoder};

    #[test]
    fn logistic_tables_are_monotone_and_accurate() {
        let l = logistic();
        for i in 1..l.squash.len() {
            assert!(l.squash[i] >= l.squash[i - 1]);
        }
        for x in (-SMAX..=SMAX).step_by(7) {
            let f = 65536.0 / (1.0 + (-(x as f64) / 256.0).exp());
            assert!((squash(l, x) as f64 - f.clamp(1.0, 65535.0)).abs() <= 1.0, "{x}");
        }
        for p in [1u32, 2, 10, 100, 1000, 32768, 60000, 65535] {
            let back = squash(l, stretch(l, p));
            assert!(back >= p && back <= p + p / 50 + 2, "{p} -> {back}");
        }
    }

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    #[test]
    fn rows_round_trip_and_text_like_images_compress() {
        // Letters: random black boxes on white, repeated rows, a few noisy rows.
        let (w, h) = (700usize, 300usize);
        let mut s = 5u64;
        let mut img = vec![vec![0u8; w]; h];
        for _ in 0..400 {
            let (x, y) = ((rng(&mut s) % (w as u64 - 20)) as usize, (rng(&mut s) % (h as u64 - 20)) as usize);
            let (bw, bh) = (2 + (rng(&mut s) % 12) as usize, 2 + (rng(&mut s) % 15) as usize);
            for yy in y..y + bh {
                for xx in x..x + bw {
                    img[yy][xx] = 1;
                }
            }
        }
        for x in 0..w {
            img[100][x] = (rng(&mut s) % 2) as u8;
        }
        let rows: Vec<Vec<u32>> = img
            .iter()
            .map(|r| {
                let mut ch = Vec::new();
                row_changes(r, &mut ch);
                ch
            })
            .collect();
        let mut m = Bilevel::new(20);
        m.start(w);
        let mut e = Encoder::new();
        let mut out = Vec::new();
        for r in &rows {
            m.code_row(&mut e, Some(r), &mut out);
            assert_eq!(&out, r);
        }
        let data = e.finish();
        assert!(data.len() < w * h / 8 / 8, "{} bytes", data.len());
        let mut m = Bilevel::new(20);
        m.start(w);
        let mut d = Decoder::new(&data);
        for r in &rows {
            m.code_row(&mut d, None, &mut out);
            assert_eq!(&out, r);
        }
    }
}
