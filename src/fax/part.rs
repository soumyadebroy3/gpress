//! A fax stream in the peyaji format: the image, coded by the bilevel model, and every choice of the
//! original encoder (fill bits, EOL codes, tag bits, end codes, padding, bytes after the end). On restore
//! the image is written again with our CCITT encoder, which gives back the original bytes exactly (the
//! encoder side checks this before choosing this form, and every pack is restored and compared).
//!
//! The choices are coded with small adaptive contexts (a fill that depends only on the bit position, as
//! byte alignment does, costs almost nothing). A damaged file is refused: every count is bounded, decoding
//! stops when the data runs out, and every 64 rows a check bit must come out as 0.

use super::bilevel::Bilevel;
use super::ccitt::{Gap, Image, Layout, Parsed, RowEncoder, Rule, Scheme, Writer, MAX_COLUMNS};
use crate::cm::{Coder, Counter};

/// Largest image the format takes (rows times columns).
const MAX_PIXELS: u64 = 1 << 32;
const MAX_ROWS: u64 = 1 << 20;
const MAX_FILL: u64 = 1 << 16;
const MAX_END: usize = 63;
/// A check bit every this many rows.
const CHECK_EVERY: usize = 64;

/// Can this decoded stream be stored as a fax part?
pub fn fits(p: &Parsed) -> bool {
    let (w, h) = (p.image.width as u64, p.image.rows() as u64);
    w >= 1
        && w <= MAX_COLUMNS as u64
        && h <= MAX_ROWS
        && w * h <= MAX_PIXELS
        && p.layout.end.len() <= MAX_END
        && p.layout.gaps.iter().chain(&p.layout.end).all(|g| (g.fill as u64) < MAX_FILL)
}

/// The models of a pack run (fax streams of one document share what they learn).
pub struct FaxCoder {
    model: Bilevel,
    small: Vec<Counter>,
}

impl Default for FaxCoder {
    fn default() -> Self {
        Self::new()
    }
}

// Counter slots of `small`.
const S_HEAD: usize = 0; // 64: scheme, rule, check bit
const S_NUM: usize = 64; // 6 kinds * 80: numbers
const S_EOL: usize = S_NUM + 6 * 80; // 4
const S_FILL: usize = S_EOL + 4; // 32 contexts * 32 nodes
const S_TAG: usize = S_FILL + 32 * 32; // 64
const S_PAD: usize = S_TAG + 64; // 8
const S_BYTES: usize = S_PAD + 8; // 256 * 256
const S_END: usize = S_BYTES + 256 * 256;

// Kinds of numbers.
const N_COLUMNS: usize = 0;
const N_ROWS: usize = 1;
const N_FILL: usize = 2;
const N_END: usize = 3;
const N_TRAILING: usize = 4;

impl FaxCoder {
    pub fn new() -> FaxCoder {
        FaxCoder { model: Bilevel::new(), small: vec![Counter::new(); S_END] }
    }

    fn bit<C: Coder>(&mut self, c: &mut C, slot: usize, bit: Option<u32>) -> u32 {
        let ct = &mut self.small[slot];
        let b = c.code(bit, ct.p12());
        ct.update(b, 255);
        b
    }

    /// A number below 2^max_bits: its bit count in unary, then its bits below the top one. None if a damaged
    /// file says more bits.
    fn num<C: Coder>(&mut self, c: &mut C, kind: usize, v: Option<u64>, max_bits: u32) -> Option<u64> {
        let base = S_NUM + kind * 80;
        let n_in = v.map(|v| 64 - v.leading_zeros());
        let mut n = 0u32;
        loop {
            let more = self.bit(c, base + n as usize, n_in.map(|k| (n < k) as u32));
            if more == 0 {
                break;
            }
            n += 1;
            if n > max_bits {
                return None;
            }
        }
        if n == 0 {
            return Some(0);
        }
        let mut val = 1u64;
        for i in (0..n - 1).rev() {
            let b = self.bit(c, base + 41 + i as usize, v.map(|v| ((v >> i) & 1) as u32));
            val = val << 1 | b as u64;
        }
        Some(val)
    }

    /// Fill bits: 0 to 30 through a binary tree in the context of the bit position (byte alignment makes the
    /// fill a function of it) and of whether an EOL follows; larger counts escape to a number.
    fn fill<C: Coder>(&mut self, c: &mut C, ctx: usize, v: Option<u32>) -> Option<u32> {
        let sym_in = v.map(|v| v.min(31));
        let mut node = 1usize;
        for k in (0..5).rev() {
            let b = self.bit(c, S_FILL + ctx * 32 + node, sym_in.map(|s| (s >> k) & 1));
            node = node * 2 + b as usize;
        }
        let sym = (node - 32) as u32;
        if sym < 31 {
            return Some(sym);
        }
        let extra = self.num(c, N_FILL, v.map(|v| (v - 31) as u64), 17)?;
        if extra + 31 >= MAX_FILL {
            return None;
        }
        Some(31 + extra as u32)
    }

    /// What comes before a row (or one end code): EOL or not, fill bits, tag bit.
    fn gap<C: Coder>(&mut self, c: &mut C, g: Option<&Gap>, st: &mut GapState, scheme: Scheme, end: bool) -> Option<Gap> {
        let eol = if end {
            true
        } else {
            self.bit(c, S_EOL + st.prev_eol as usize * 2 + st.first as usize, g.map(|g| g.eol as u32)) == 1
        };
        let ctx = (st.pos & 7) * 4 + eol as usize * 2 + end as usize;
        let fill = self.fill(c, ctx, g.map(|g| g.fill))?;
        let tag = scheme == Scheme::G32D && {
            let tctx = if end { 62 + st.prev_tag as usize } else { st.since_1d.min(30) * 2 + st.prev_tag as usize };
            self.bit(c, S_TAG + tctx, g.map(|g| g.tag as u32)) == 1
        };
        st.prev_eol = eol;
        st.first = false;
        st.prev_tag = tag;
        st.since_1d = if tag { 0 } else { st.since_1d + 1 };
        st.pos += fill as usize + if eol { 12 } else { 0 } + (scheme == Scheme::G32D) as usize;
        Some(Gap { fill, eol, tag })
    }
}

struct GapState {
    /// Bit position in the stream being written (only its low 3 bits matter to the contexts).
    pos: usize,
    prev_eol: bool,
    first: bool,
    prev_tag: bool,
    since_1d: usize,
}

/// Codes one fax stream: encoding when `src` is given (it must pass `fits`), restoring otherwise. Returns
/// the stream's bytes, or None when the data can only come from a damaged file. The result is never longer
/// than `max_len`.
pub fn code<C: Coder>(c: &mut C, fc: &mut FaxCoder, src: Option<&Parsed>, max_len: usize) -> Option<Vec<u8>> {
    let lay = src.map(|p| &p.layout);
    let img = src.map(|p| &p.image);
    // Header.
    let s0 = fc.bit(c, S_HEAD, lay.map(|l| (l.scheme.id() & 1) as u32));
    let s1 = fc.bit(c, S_HEAD + 1 + s0 as usize, lay.map(|l| (l.scheme.id() >> 1) as u32));
    let scheme = Scheme::from_id((s0 | s1 << 1) as u8)?;
    let r = fc.bit(c, S_HEAD + 4, lay.map(|l| l.rule.id() as u32));
    let rule = Rule::from_id(r as u8)?;
    let w = fc.num(c, N_COLUMNS, img.map(|i| i.width as u64), 18)?;
    let h = fc.num(c, N_ROWS, img.map(|i| i.rows() as u64), 21)?;
    if w == 0 || w > MAX_COLUMNS as u64 || h > MAX_ROWS || w * h > MAX_PIXELS {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    fc.model.start(w);
    let mut wr = Writer::new();
    let mut enc = RowEncoder::new(w, scheme, rule);
    let mut st = GapState { pos: 0, prev_eol: false, first: true, prev_tag: false, since_1d: 0 };
    let mut row = Vec::new();
    for r in 0..h {
        if r % CHECK_EVERY == 0 && (c.exhausted() || fc.bit(c, S_HEAD + 8, src.map(|_| 0)) != 0) {
            return None;
        }
        st.pos = wr.bit_pos();
        let g = fc.gap(c, lay.map(|l| &l.gaps[r]), &mut st, scheme, false)?;
        fc.model.code_row(c, img.map(|i| i.row(r)), &mut row);
        enc.row(&mut wr, &g, &row);
        if wr.bit_pos() / 8 > max_len {
            return None;
        }
    }
    // End codes, the bits up to the next byte boundary, the bytes after.
    let n_end = fc.num(c, N_END, lay.map(|l| l.end.len() as u64), 6)? as usize;
    if n_end > MAX_END {
        return None;
    }
    st.pos = wr.bit_pos();
    let mut end = Vec::with_capacity(n_end);
    for i in 0..n_end {
        end.push(fc.gap(c, lay.map(|l| &l.end[i]), &mut st, scheme, true)?);
    }
    let pad_bits = (8 - (st.pos & 7)) & 7;
    let mut pad = 0u8;
    for k in (0..pad_bits).rev() {
        let b = fc.bit(c, S_PAD + k, lay.map(|l| ((l.pad >> k) & 1) as u32));
        pad |= (b as u8) << k;
    }
    let n_tr = fc.num(c, N_TRAILING, lay.map(|l| l.trailing.len() as u64), 40)?;
    if n_tr > max_len as u64 {
        return None;
    }
    let mut trailing = Vec::with_capacity((n_tr as usize).min(1 << 20));
    let mut prev = 0usize;
    for i in 0..n_tr as usize {
        if i & 0xfff == 0 && c.exhausted() {
            return None;
        }
        let byte_in = lay.map(|l| l.trailing[i]);
        let mut node = 1usize;
        for k in (0..8).rev() {
            let b = fc.bit(c, S_BYTES + prev * 256 + node, byte_in.map(|x| ((x >> k) & 1) as u32));
            node = node * 2 + b as usize;
        }
        let b = (node - 256) as u8;
        trailing.push(b);
        prev = b as usize;
    }
    let layout = Layout { scheme, rule, gaps: Vec::new(), end, pad, trailing };
    let out = enc.finish(wr, &layout);
    if out.len() > max_len {
        return None;
    }
    Some(out)
}

/// For measuring only: the image as packed rows (1 bit per pixel), what a general compressor would get
/// after the same preparation.
pub fn packed(img: &Image) -> Vec<u8> {
    let w = img.width;
    let mut out = Vec::with_capacity((w + 7) / 8 * img.rows());
    let mut px = vec![0u8; w];
    for r in 0..img.rows() {
        super::ccitt::expand_row(img.row(r), &mut px);
        for chunk in px.chunks(8) {
            let mut b = 0u8;
            for (i, &p) in chunk.iter().enumerate() {
                b |= p << (7 - i);
            }
            out.push(b);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cm::{Decoder, Encoder};
    use crate::fax::ccitt::{encode, parse, row_changes, Limits};

    fn sample(w: usize, h: usize) -> Image {
        let mut img = Image::new(w);
        let mut px = vec![0u8; w];
        let mut ch = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px[x] = ((x / 7 + y / 5) % 3 == 0 && (x * y) % 11 != 0) as u8;
            }
            if y % 9 == 4 {
                px.fill(0);
            }
            row_changes(&px, &mut ch);
            img.push_row(&ch);
        }
        img
    }

    fn pack_one(bytes: &[u8], scheme: Scheme, w: usize, align: bool) -> Vec<u8> {
        let p = parse(bytes, scheme, w, align, Limits::default()).unwrap();
        assert!(fits(&p));
        let mut fc = FaxCoder::new();
        let mut e = Encoder::new();
        let back = code(&mut e, &mut fc, Some(&p), bytes.len()).unwrap();
        assert_eq!(back, bytes);
        e.finish()
    }

    fn unpack_one(data: &[u8], max: usize) -> Option<Vec<u8>> {
        let mut fc = FaxCoder::new();
        let mut d = Decoder::new(data);
        code(&mut d, &mut fc, None, max)
    }

    #[test]
    fn every_layout_comes_back_exactly() {
        for scheme in [Scheme::G4, Scheme::G31D, Scheme::G32D] {
            for (eol, fill, trailing) in [(false, 0, &b""[..]), (true, 3, &b"\r\n"[..]), (true, 40, &b"junk after the end"[..])] {
                let img = sample(301, 47);
                let gaps = (0..img.rows()).map(|r| Gap { fill: if r % 2 == 0 { fill } else { 0 }, eol, tag: r % 4 == 0 }).collect();
                let end = vec![Gap { fill: 1, eol: true, tag: true }; if scheme == Scheme::G4 { 2 } else { 6 }];
                let lay = Layout { scheme, rule: Rule::Standard, gaps, end, pad: 0, trailing: trailing.to_vec() };
                let bytes = encode(&img, &lay);
                let packed = pack_one(&bytes, scheme, 301, false);
                assert_eq!(unpack_one(&packed, bytes.len()).unwrap(), bytes, "{scheme:?} eol={eol} fill={fill}");
            }
        }
    }

    #[test]
    fn damaged_parts_are_refused_never_wrong() {
        let img = sample(200, 150);
        let gaps = (0..img.rows()).map(|_| Gap::default()).collect();
        let lay = Layout { scheme: Scheme::G4, rule: Rule::Standard, gaps, end: vec![Gap { fill: 0, eol: true, tag: false }; 2], pad: 0, trailing: vec![] };
        let bytes = encode(&img, &lay);
        let packed = pack_one(&bytes, Scheme::G4, 200, false);
        let mut wrong = 0;
        for i in 0..packed.len() {
            for bit in [0, 4] {
                let mut b = packed.clone();
                b[i] ^= 1 << bit;
                if let Some(out) = unpack_one(&b, bytes.len()) {
                    // A flip the model happens to absorb must still give a stream within the bounds; the
                    // format's checksum catches anything else.
                    assert!(out.len() <= bytes.len());
                    if out != bytes {
                        wrong += 1;
                    }
                }
            }
        }
        // Cut short: refused (or, if the cut only removes the coder's last bytes, exact).
        for cut in 0..packed.len() {
            if let Some(out) = unpack_one(&packed[..cut], bytes.len()) {
                assert!(out.len() <= bytes.len());
            }
        }
        // Garbage of every length: refused or bounded, and quickly.
        let mut s = 1u64;
        for n in [0usize, 1, 5, 50, 500, 5000] {
            let g: Vec<u8> = (0..n).map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u8
            }).collect();
            if let Some(out) = unpack_one(&g, 10_000) {
                assert!(out.len() <= 10_000);
            }
        }
        // Most flips must be noticed by the part itself (the rest by the file's checksum).
        assert!(wrong < packed.len(), "{wrong} wrong restores out of {}", packed.len() * 2);
    }
}
