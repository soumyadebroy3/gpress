//! A fax stream in the gpress archive: the image, coded by the bilevel model, and every choice of the
//! original encoder (fill bits, EOL codes, tag bits, end codes, padding, bytes after the end). On restore
//! the image is written again with our CCITT encoder, which gives back the original bytes exactly (the
//! encoder side checks this before choosing this form, and every pack is restored and compared).
//!
//! The choices are coded with small adaptive contexts (a fill that depends only on the bit position, as
//! byte alignment does, costs almost nothing). A damaged file is refused: every count is bounded, decoding
//! stops when the data runs out, every 64 rows a check bit must come out as 0, and the work is bounded per
//! image and per file.

use super::bilevel::Bilevel;
use super::ccitt::{encode, parse, row_work, Gap, Image, Layout, Limits, Parsed, RowEncoder, Rule, Scheme, Stop, Writer, MAX_COLUMNS};
use crate::cm::{Coder, Counter};

/// Largest image the format takes (rows times columns).
const MAX_PIXELS: u64 = 1 << 32;
/// Most work (`Image::work`) for one image: keeps the time to pack and to restore bounded for huge images (a
/// restore that goes past it is refused).
const MAX_WORK: u64 = 1 << 30;
/// Most work for the fax streams of a file up to any point of it: FILE_WORK_BASE plus FILE_WORK_PER_BYTE per
/// byte of the file up to there. A few bytes of fax data can describe a huge image, and a file can hold
/// many: this bounds the time to pack a file, and to restore one, by its size (at most about 70 microseconds
/// per byte to pack, half that to restore), whatever length a damaged file claims. Streams past it are coded
/// plainly; a restore that goes past it is refused. The test files need at most 180 per byte, and one small
/// file 1,200 (within the base).
const FILE_WORK_BASE: u64 = 1 << 24;
const FILE_WORK_PER_BYTE: u64 = 512;
/// Work charged for each fax stream besides its rows (setting up the model, the stream's header).
const STREAM_WORK: u64 = 4096;
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
        && p.image.work() <= MAX_WORK
        && p.layout.end.len() <= MAX_END
        && p.layout.gaps.iter().chain(&p.layout.end).all(|g| (g.fill as u64) < MAX_FILL)
}

/// A fax stream (`raw`, with its CCITTFaxDecode parameters) decoded for `code`, or None if it can't be stored
/// this way. Decoded with the stream's byte alignment and, if that leaves data undecoded, also without it
/// (or with it): many encoders write EncodedByteAlign data with the fill before each EOL, so that the EOL
/// ends on a byte boundary, which reads as plain fill bits and EOLs. The less data left over wins. Every
/// choice is recorded either way, and writing the stream again must give `raw`.
pub fn prepare(raw: &[u8], scheme: Scheme, columns: usize, byte_align: bool) -> Option<Parsed> {
    let mut parsed = parse(raw, scheme, columns, byte_align, Limits::default())?;
    if !matches!(parsed.stop, Stop::EndCodes | Stop::EndOfData) {
        if let Some(other) = parse(raw, scheme, columns, !byte_align, Limits::default()) {
            if other.layout.trailing.len() < parsed.layout.trailing.len() {
                parsed = other;
            }
        }
    }
    if parsed.image.rows() == 0 || parsed.layout.trailing.len() * 4 > raw.len() || !fits(&parsed) {
        return None;
    }
    if encode(&parsed.image, &parsed.layout) != raw {
        return None;
    }
    Some(parsed)
}

/// The work the fax streams of a file may take up to byte `pos` of it (see FILE_WORK_PER_BYTE).
pub fn budget(pos: usize) -> u64 {
    FILE_WORK_BASE.saturating_add(FILE_WORK_PER_BYTE.saturating_mul(pos as u64))
}

/// The work of coding an image, as `code` counts it.
pub fn cost(img: &Image) -> u64 {
    start_work(img.width) + img.work()
}

fn start_work(width: usize) -> u64 {
    STREAM_WORK + width as u64 / 64
}

/// The work of a file's fax streams so far, counted as `code` counts it while it codes them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Work {
    spent: u64,
}

impl Work {
    /// Adds `n`; false if the work up to byte `pos` of the file is then past `budget(pos)`.
    fn add(&mut self, n: u64, pos: usize) -> bool {
        self.spent = self.spent.saturating_add(n);
        self.spent <= budget(pos)
    }

    /// Would coding `p`, a stream starting at byte `at` of the file, stay within the budget at every row, as
    /// `code` checks it? If so, its work is counted (a pack codes only the streams this lets in, so their
    /// restore never fails these checks).
    pub fn admit(&mut self, p: &Parsed, at: usize) -> bool {
        let (img, lay) = (&p.image, &p.layout);
        let mut t = *self;
        if !t.add(start_work(img.width), at) {
            return false;
        }
        let mut wr = Writer::new();
        let mut enc = RowEncoder::new(img.width, lay.scheme, lay.rule);
        for r in 0..img.rows() {
            let same = if r == 0 { img.row(0).is_empty() } else { img.row(r) == img.row(r - 1) };
            enc.row(&mut wr, &lay.gaps[r], img.row(r));
            if !t.add(row_work(img.width, same), at + wr.bit_pos() / 8) {
                return false;
            }
        }
        *self = t;
        true
    }
}

/// The models of a pack run (fax streams of one document share what they learn) and the work of its fax
/// streams so far.
pub struct FaxCoder {
    model: Bilevel,
    small: Vec<Counter>,
    work: Work,
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
    /// `file_len`: the length of the whole file being packed (the decoder knows it too); small files get
    /// small hashed tables (quick to set up, and they have few contexts anyway).
    pub fn new(file_len: usize) -> FaxCoder {
        let bits = (usize::BITS - file_len.leading_zeros()) + 4;
        FaxCoder { model: Bilevel::new(bits), small: vec![Counter::new(); S_END], work: Work::default() }
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
        let tag = if scheme != Scheme::G32D {
            false
        } else if end {
            let t = self.bit(c, S_TAG + 62 + st.prev_tag as usize, g.map(|g| g.tag as u32)) == 1;
            st.prev_tag = t;
            t
        } else {
            self.tag(c, g, st)
        };
        st.prev_eol = eol;
        st.first = false;
        st.pos += fill as usize + if eol { 12 } else { 0 } + (scheme == Scheme::G32D) as usize;
        Some(Gap { fill, eol, tag })
    }

    /// The tag bit of a 2-D Group 3 row (1: coded one-dimensionally), in the context of how many rows ago
    /// the last one-dimensional row was (encoders write one every K rows).
    fn tag<C: Coder>(&mut self, c: &mut C, g: Option<&Gap>, st: &mut GapState) -> bool {
        let tctx = st.since_1d.min(30) * 2 + st.prev_tag as usize;
        let t = self.bit(c, S_TAG + tctx, g.map(|g| g.tag as u32)) == 1;
        st.prev_tag = t;
        st.since_1d = if t { 0 } else { st.since_1d + 1 };
        t
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

/// Codes one fax stream, which starts at byte `at` of the file: encoding when `src` is given (it must pass
/// `fits`, and `Work::admit` must have let it in), restoring otherwise. Returns the stream's bytes, or None
/// when the data can only come from a damaged file. The result is never longer than `max_len`.
pub fn code<C: Coder>(c: &mut C, fc: &mut FaxCoder, src: Option<&Parsed>, at: usize, max_len: usize) -> Option<Vec<u8>> {
    let lay = src.map(|p| &p.layout);
    let img = src.map(|p| &p.image);
    // Header.
    let s0 = fc.bit(c, S_HEAD, lay.map(|l| (l.scheme.id() & 1) as u32));
    let s1 = fc.bit(c, S_HEAD + 1 + s0 as usize, lay.map(|l| (l.scheme.id() >> 1) as u32));
    let scheme = Scheme::from_id((s0 | s1 << 1) as u8)?;
    let r = fc.bit(c, S_HEAD + 4, lay.map(|l| l.rule.id() as u32));
    let rule = Rule::from_id(r as u8)?;
    // No row has an EOL or fill bits (the usual Group 4 stream): then only tag bits are coded per row.
    let plain = fc.bit(c, S_HEAD + 5, lay.map(|l| l.gaps.iter().all(|g| !g.eol && g.fill == 0) as u32)) == 1;
    let w = fc.num(c, N_COLUMNS, img.map(|i| i.width as u64), 18)?;
    let h = fc.num(c, N_ROWS, img.map(|i| i.rows() as u64), 21)?;
    if w == 0 || w > MAX_COLUMNS as u64 || h > MAX_ROWS || w * h > MAX_PIXELS {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    // The work is counted and checked as `Work::admit` does it.
    if !fc.work.add(start_work(w), at) {
        return None;
    }
    fc.model.start(w);
    let mut wr = Writer::new();
    let mut enc = RowEncoder::new(w, scheme, rule);
    let mut st = GapState { pos: 0, prev_eol: false, first: true, prev_tag: false, since_1d: 0 };
    let mut row = Vec::new();
    let mut work = 0u64;
    for r in 0..h {
        if r % CHECK_EVERY == 0 && (c.exhausted() || fc.bit(c, S_HEAD + 8, src.map(|_| 0)) != 0) {
            return None;
        }
        st.pos = wr.bit_pos();
        let g = if plain {
            let tag = scheme == Scheme::G32D && fc.tag(c, lay.map(|l| &l.gaps[r]), &mut st);
            Gap { fill: 0, eol: false, tag }
        } else {
            fc.gap(c, lay.map(|l| &l.gaps[r]), &mut st, scheme, false)?
        };
        let rw = row_work(w, fc.model.code_row(c, img.map(|i| i.row(r)), &mut row));
        enc.row(&mut wr, &g, &row);
        work += rw;
        if work > MAX_WORK || !fc.work.add(rw, at + wr.bit_pos() / 8) {
            return None;
        }
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
    let mut out = Vec::with_capacity(w.div_ceil(8) * img.rows());
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
        let mut fc = FaxCoder::new(bytes.len());
        let mut e = Encoder::new();
        let back = code(&mut e, &mut fc, Some(&p), 0, bytes.len()).unwrap();
        assert_eq!(back, bytes);
        e.finish()
    }

    fn unpack_one(data: &[u8], max: usize) -> Option<Vec<u8>> {
        let mut fc = FaxCoder::new(max);
        let mut d = Decoder::new(data);
        code(&mut d, &mut fc, None, 0, max)
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

    #[test]
    fn restore_refuses_work_past_the_budget() {
        // 130 rows of 131072 pixels, none like the row above: more than 2^24 of work from 280 bytes.
        let mut img = Image::new(131072);
        for r in 0..130 {
            img.push_row(if r % 2 == 0 { &[10, 11][..] } else { &[] });
        }
        let gaps = (0..img.rows()).map(|_| Gap::default()).collect();
        let lay = Layout { scheme: Scheme::G4, rule: Rule::Standard, gaps, end: vec![Gap { fill: 0, eol: true, tag: false }; 2], pad: 0, trailing: vec![] };
        let bytes = encode(&img, &lay);
        let p = parse(&bytes, Scheme::G4, 131072, false, Limits::default()).unwrap();
        let c = cost(&img);
        assert!(fits(&p) && budget(bytes.len()) < c);
        // Far enough into a file the budget has room for it; at the start of one, or a little less far, not.
        let per_byte = budget(1) - budget(0);
        let far = ((c - budget(0)) / per_byte) as usize + 1;
        let near = far - bytes.len() - 2;
        assert!(Work::default().admit(&p, far));
        assert!(!Work::default().admit(&p, near) && !Work::default().admit(&p, 0));
        // The budget is shared: right after it, a second one doesn't fit.
        let mut wk = Work::default();
        assert!(wk.admit(&p, far) && !wk.admit(&p, far + bytes.len()));
        // The restore makes the same checks, at the place in the file it has really restored up to (not a
        // length the file claims).
        let mut e = Encoder::new();
        assert_eq!(code(&mut e, &mut FaxCoder::new(far), Some(&p), far, bytes.len()).as_deref(), Some(&bytes[..]));
        let packed = e.finish();
        let restore = |at: usize| code(&mut Decoder::new(&packed), &mut FaxCoder::new(far), None, at, bytes.len());
        assert_eq!(restore(far).as_deref(), Some(&bytes[..]));
        assert_eq!(restore(near), None);
        assert_eq!(restore(0), None);
    }
}
