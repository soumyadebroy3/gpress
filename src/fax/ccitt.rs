//! CCITT fax coding (ITU-T T.4 one- and two-dimensional, T.6), as PDF's CCITTFaxDecode filter uses it.
//!
//! `parse` decodes a stream strictly and records every choice its encoder made: zero fill bits, EOL codes,
//! the tag bits of 2-D Group 3, the codes after the last row (EOFB, RTC), the bits up to the next byte
//! boundary and any bytes after them. `encode` writes the image back with those choices, following the
//! coding procedure of the recommendations. `parse` keeps a row only if writing it again gives the same
//! bits, so `encode(parse(x))` is `x` for any bytes `x`: the image and the recorded choices together are a
//! lossless form of the stream. Callers that store only the image still compare once more before relying
//! on it.

use super::tables::{tables, Code, Mode, EOL_BITS, EOL_LEN, HORIZONTAL, MAX_RUN_CODE, PASS, VERTICAL};

/// The three ways of coding: K < 0 (T.6, Group 4), K = 0 (T.4 one-dimensional), K > 0 (T.4 two-dimensional,
/// each row tagged as coded one- or two-dimensionally).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    G4,
    G31D,
    G32D,
}

impl Scheme {
    pub fn from_k(k: i64) -> Scheme {
        if k < 0 {
            Scheme::G4
        } else if k == 0 {
            Scheme::G31D
        } else {
            Scheme::G32D
        }
    }
    pub fn id(self) -> u8 {
        match self {
            Scheme::G4 => 0,
            Scheme::G31D => 1,
            Scheme::G32D => 2,
        }
    }
    pub fn from_id(id: u8) -> Option<Scheme> {
        match id {
            0 => Some(Scheme::G4),
            1 => Some(Scheme::G31D),
            2 => Some(Scheme::G32D),
            _ => None,
        }
    }
}

/// Widest row we decode.
pub const MAX_COLUMNS: usize = 1 << 17;

/// A bilevel image as the changing elements of each row: positions where the colour changes (each row starts
/// white), strictly increasing and below the width. 1 is black.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Image {
    pub width: usize,
    pub changes: Vec<u32>,
    /// Row r's changes are `changes[starts[r]..starts[r + 1]]`.
    pub starts: Vec<usize>,
}

impl Image {
    pub fn new(width: usize) -> Image {
        Image { width, changes: Vec::new(), starts: vec![0] }
    }
    pub fn rows(&self) -> usize {
        self.starts.len() - 1
    }
    pub fn row(&self, r: usize) -> &[u32] {
        &self.changes[self.starts[r]..self.starts[r + 1]]
    }
    pub fn push_row(&mut self, changes: &[u32]) {
        self.changes.extend_from_slice(changes);
        self.starts.push(self.changes.len());
    }
    /// The work of a pixel-by-pixel coder on this image, in pixels (see `row_work`).
    pub fn work(&self) -> u64 {
        let mut n = 0u64;
        for r in 0..self.rows() {
            let same = if r == 0 { self.row(0).is_empty() } else { self.row(r) == self.row(r - 1) };
            n += row_work(self.width, same);
        }
        n
    }
}

/// The work of a pixel-by-pixel coder on one row: its width, or for a row equal to the row above (white above
/// the first), which costs one decision, a 256th of it (the row is still copied).
pub fn row_work(width: usize, same: bool) -> u64 {
    if same {
        width as u64 / 256 + 1
    } else {
        width as u64
    }
}

/// One pixel per byte (0 white, 1 black) from a row's changing elements.
pub fn expand_row(changes: &[u32], out: &mut [u8]) {
    let w = out.len();
    let mut x = 0usize;
    let mut colour = 0u8;
    for &c in changes {
        let c = (c as usize).clamp(x, w);
        out[x..c].fill(colour);
        x = c;
        colour ^= 1;
    }
    out[x..].fill(colour);
}

/// A row's changing elements from one pixel per byte (anything but 0 is black).
pub fn row_changes(px: &[u8], out: &mut Vec<u32>) {
    out.clear();
    let mut colour = 0u8;
    for (i, &p) in px.iter().enumerate() {
        let b = (p != 0) as u8;
        if b != colour {
            out.push(i as u32);
            colour = b;
        }
    }
}

/// What comes before a row, or one code after the last row: zero fill bits, then an EOL code or not, then for
/// 2-D Group 3 the tag bit (1: the row that follows is coded one-dimensionally).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gap {
    pub fill: u32,
    pub eol: bool,
    pub tag: bool,
}

/// How an encoder chooses among the 2-D modes when pass mode is possible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// T.4 4.2.1.3: pass mode whenever b2 is left of a1.
    Standard,
    /// Adobe's encoder (Acrobat Distiller and others): it looks along the chain of pass modes the standard
    /// would write; if that chain ends in horizontal mode, or takes more than 5 passes, it writes horizontal
    /// mode at once instead. (Found by decoding Distiller output; it matches every row of the test files.)
    Adobe,
}

impl Rule {
    pub const ALL: [Rule; 2] = [Rule::Standard, Rule::Adobe];
    pub fn id(self) -> u8 {
        match self {
            Rule::Standard => 0,
            Rule::Adobe => 1,
        }
    }
    pub fn from_id(id: u8) -> Option<Rule> {
        Rule::ALL.into_iter().find(|r| r.id() == id)
    }
}

/// Every choice of the original encoder that the image alone doesn't fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub scheme: Scheme,
    /// How 2-D rows choose their modes.
    pub rule: Rule,
    /// One per row.
    pub gaps: Vec<Gap>,
    /// The EOL codes after the last row (EOFB, RTC), each with its fill and tag bits.
    pub end: Vec<Gap>,
    /// The bits after those, up to the next byte boundary.
    pub pad: u8,
    /// The bytes after that.
    pub trailing: Vec<u8>,
}

/// Why decoding stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// After the end codes (two or more EOLs, or one EOL followed only by 0 bits).
    EndCodes,
    /// Only 0 bits were left.
    EndOfData,
    /// The next bits are no row (damaged or unusual data); they are kept as they are, in the layout.
    Undecodable,
    /// A limit was reached.
    Limit,
}

pub struct Parsed {
    pub image: Image,
    pub layout: Layout,
    pub stop: Stop,
    /// Why the last row couldn't be taken, for diagnostics ("" if decoding stopped for another reason).
    pub why: &'static str,
}

/// Bounds on what one stream may decode to (a few bits of data can describe a huge image).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_rows: usize,
    pub max_pixels: u64,
    pub max_changes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_rows: 1 << 20, max_pixels: 1 << 32, max_changes: 1 << 26 }
    }
}

// ---------------------------------------------------------------- bits, most significant bit first

struct Reader<'a> {
    d: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn new(d: &'a [u8]) -> Reader<'a> {
        Reader { d, pos: 0, end: d.len() * 8 }
    }
    /// The next `n` bits (n <= 25); 0 bits past the end.
    #[inline]
    fn peek(&self, n: u32) -> u32 {
        let byte = self.pos >> 3;
        let off = (self.pos & 7) as u32;
        let mut w: u64 = 0;
        for i in 0..5 {
            w = w << 8 | *self.d.get(byte + i).unwrap_or(&0) as u64;
        }
        ((w >> (40 - off - n)) & ((1u64 << n) - 1)) as u32
    }
    #[inline]
    fn left(&self) -> usize {
        self.end - self.pos
    }
    /// Number of 0 bits before the next 1 bit, or None if only 0 bits are left.
    fn zeros(&self) -> Option<usize> {
        let mut p = self.pos;
        while p < self.end {
            let byte = self.d[p >> 3];
            let off = p & 7;
            let rest = (byte << off) as u32;
            if rest != 0 {
                return Some(p + (rest.leading_zeros() as usize - 24) - self.pos);
            }
            p += 8 - off;
        }
        None
    }
}

pub struct Writer {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { out: Vec::new(), acc: 0, n: 0 }
    }
    #[inline]
    fn put(&mut self, bits: u32, len: u32) {
        debug_assert!(len <= 32);
        self.acc = self.acc << len | (bits as u64 & ((1u64 << len) - 1));
        self.n += len;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }
    fn code(&mut self, c: Code) {
        self.put(c.bits, c.len);
    }
    fn zeros(&mut self, mut n: u32) {
        while n > 0 {
            let k = n.min(24);
            self.put(0, k);
            n -= k;
        }
    }
    /// Bits written so far.
    pub fn bit_pos(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }
    /// Fills up the last byte with `pad` (its low bits) and returns the bytes.
    fn finish(mut self, pad: u8) -> Vec<u8> {
        if self.n > 0 {
            let k = 8 - self.n;
            self.put(pad as u32, k);
        }
        self.out
    }
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- runs

fn read_run(r: &mut Reader, colour: usize, max: usize) -> Result<usize, &'static str> {
    let t = tables();
    let mut run = 0usize;
    loop {
        let (len, v) = t.run_lookup[colour][r.peek(MAX_RUN_CODE) as usize];
        if len == 0 || len as usize > r.left() {
            return Err("bad run code");
        }
        r.pos += len as usize;
        run += v as usize;
        if run > max {
            return Err("run past the end of the row");
        }
        if v < 64 {
            return Ok(run);
        }
    }
}

/// A run as T.4 4.1.1 codes it: make-up codes of 2560 while 2624 or more is left, then one make-up code if
/// 64 or more is left, then a terminating code.
fn put_run(w: &mut Writer, colour: usize, mut run: usize) {
    let t = tables();
    while run >= 2624 {
        w.code(t.ext[12]);
        run -= 2560;
    }
    if run >= 64 {
        let m = run / 64;
        w.code(if m <= 27 { t.makeup[colour][m - 1] } else { t.ext[m - 28] });
        run -= m * 64;
    }
    w.code(t.term[colour][run]);
}

// ---------------------------------------------------------------- rows

fn decode_1d(r: &mut Reader, cur: &mut Vec<u32>, w: usize) -> Result<(), &'static str> {
    let mut x = 0usize;
    let mut colour = 0usize;
    loop {
        let run = read_run(r, colour, w - x)?;
        x += run;
        if x == w {
            return Ok(());
        }
        // A run of length 0 is only written as the first (white) run of a row that starts black.
        if run == 0 && !(x == 0 && colour == 0) {
            return Err("empty run");
        }
        cur.push(x as u32);
        colour ^= 1;
    }
}

fn encode_1d(wr: &mut Writer, cur: &[u32], w: usize) {
    let mut x = 0usize;
    let mut colour = 0usize;
    let mut ci = 0;
    loop {
        let next = cur.get(ci).map_or(w, |&v| v as usize);
        put_run(wr, colour, next - x);
        x = next;
        if x >= w {
            return;
        }
        ci += 1;
        colour ^= 1;
    }
}

/// b1 and b2 of T.4 4.2.1.3.1: the first changing element of the reference row to the right of a0 whose
/// colour is opposite to a0's, and the next one. `rp` is the first index with a position above a0 (it only
/// moves forward as a0 does). A change at an even index starts black.
#[inline]
fn b1_b2(refl: &[u32], rp: &mut usize, a0: i64, colour: usize, w: i64) -> (i64, i64) {
    while *rp < refl.len() && refl[*rp] as i64 <= a0 {
        *rp += 1;
    }
    let mut j = *rp;
    if j & 1 != colour {
        j += 1;
    }
    (refl.get(j).map_or(w, |&v| v as i64), refl.get(j + 1).map_or(w, |&v| v as i64))
}

fn decode_2d(r: &mut Reader, refl: &[u32], cur: &mut Vec<u32>, width: usize) -> Result<(), &'static str> {
    let t = tables();
    let w = width as i64;
    let mut a0: i64 = -1;
    let mut colour = 0usize;
    let mut rp = 0usize;
    loop {
        let (b1, b2) = b1_b2(refl, &mut rp, a0, colour, w);
        let (len, mode) = t.mode_lookup[r.peek(7) as usize];
        if len == 0 || len as usize > r.left() {
            return Err("bad mode code");
        }
        r.pos += len as usize;
        match mode {
            Mode::Pass => {
                // The encoder passes only to a b2 left of a1, so never to the end of the row.
                if b2 >= w {
                    return Err("pass to the end of the row");
                }
                a0 = b2;
            }
            Mode::Horizontal => {
                let start = a0.max(0);
                let r1 = read_run(r, colour, (w - start) as usize)? as i64;
                let a1 = start + r1;
                let r2 = read_run(r, colour ^ 1, (w - a1) as usize)? as i64;
                let a2 = a1 + r2;
                if (a0 >= 0 && r1 == 0) || (a1 < w && r2 == 0) {
                    return Err("empty run");
                }
                if a1 < w {
                    cur.push(a1 as u32);
                }
                if a2 < w {
                    cur.push(a2 as u32);
                }
                a0 = a2;
            }
            Mode::Vertical(d) => {
                let a1 = b1 + d as i64;
                if a1 <= a0 || a1 < 0 || a1 > w {
                    return Err("vertical mode out of order");
                }
                if a1 < w {
                    cur.push(a1 as u32);
                }
                a0 = a1;
                colour ^= 1;
            }
            Mode::Extension => return Err("extension code"),
        }
        if a0 >= w {
            return Ok(());
        }
    }
}

/// Adobe's choice when pass mode is possible: follow the passes the standard would write; horizontal mode
/// if they end in horizontal mode or are more than 5.
fn adobe_wants_horizontal(refl: &[u32], rp: usize, a0: i64, a1: i64, colour: usize, w: i64) -> bool {
    let (mut x0, mut rp, mut k) = (a0, rp, 0);
    loop {
        let (c1, c2) = b1_b2(refl, &mut rp, x0, colour, w);
        if c2 < a1 {
            x0 = c2;
            k += 1;
            if k > 5 {
                return true;
            }
            continue;
        }
        return (a1 - c1).abs() > 3;
    }
}

/// T.4 4.2.1.3 (and T.6): pass mode when b2 is left of a1, vertical mode when a1 is within 3 of b1,
/// horizontal mode otherwise (with Adobe's variant of the first, see `Rule`).
fn encode_2d(wr: &mut Writer, refl: &[u32], cur: &[u32], width: usize, rule: Rule) {
    let w = width as i64;
    let mut a0: i64 = -1;
    let mut colour = 0usize;
    let mut rp = 0usize;
    let mut ci = 0usize;
    loop {
        let (b1, b2) = b1_b2(refl, &mut rp, a0, colour, w);
        let a1 = cur.get(ci).map_or(w, |&v| v as i64);
        if b2 < a1 && !(rule == Rule::Adobe && adobe_wants_horizontal(refl, rp, a0, a1, colour, w)) {
            wr.code(PASS);
            a0 = b2;
        } else if b2 >= a1 && (a1 - b1).abs() <= 3 {
            wr.code(VERTICAL[(a1 - b1 + 3) as usize]);
            a0 = a1;
            ci += 1;
            colour ^= 1;
        } else {
            let a2 = cur.get(ci + 1).map_or(w, |&v| v as i64);
            wr.code(HORIZONTAL);
            put_run(wr, colour, (a1 - a0.max(0)) as usize);
            put_run(wr, colour ^ 1, (a2 - a1) as usize);
            a0 = a2;
            ci += 2;
        }
        if a0 >= w {
            return;
        }
    }
}

// ---------------------------------------------------------------- whole streams

enum GapRead {
    Got(Gap),
    /// Only 0 bits are left.
    End,
    /// Something else (byte-alignment bits that aren't 0, or the data ends inside the gap).
    Bad,
}

fn read_gap(r: &mut Reader, scheme: Scheme, byte_align: bool) -> GapRead {
    let z = match r.zeros() {
        Some(z) => z,
        None => return GapRead::End,
    };
    let mut g = Gap::default();
    // With byte alignment the bits up to the next byte boundary are fill (they must be 0), and an EOL counts
    // only if it starts there or later. (Data with the fill before each EOL, so that the EOL ends on a byte
    // boundary, reads as unaligned fill bits and EOLs: see `part::prepare`.)
    let a = if byte_align { (8 - (r.pos & 7)) & 7 } else { 0 };
    if z >= a + 11 {
        g.fill = (z - 11) as u32;
        g.eol = true;
        r.pos += z + 1;
    } else if byte_align {
        if z < a {
            return GapRead::Bad;
        }
        g.fill = a as u32;
        r.pos += a;
    }
    if scheme == Scheme::G32D {
        if r.left() == 0 {
            return GapRead::Bad;
        }
        g.tag = r.peek(1) == 1;
        r.pos += 1;
    }
    GapRead::Got(g)
}

fn put_gap(wr: &mut Writer, scheme: Scheme, g: &Gap) {
    wr.zeros(g.fill);
    if g.eol {
        wr.put(EOL_BITS, EOL_LEN);
    }
    if scheme == Scheme::G32D {
        wr.put(g.tag as u32, 1);
    }
}

/// Do the bits written to `w` equal the bits of `data` from bit `start` on?
fn same_bits(data: &[u8], start: usize, w: &Writer) -> bool {
    let mut r = Reader::new(data);
    r.pos = start;
    if r.left() < w.bit_pos() {
        return false;
    }
    for &b in &w.out {
        if r.peek(8) != b as u32 {
            return false;
        }
        r.pos += 8;
    }
    w.n == 0 || r.peek(w.n) == (w.acc & ((1u64 << w.n) - 1)) as u32
}

/// Decodes a CCITT stream and records its layout. Never fails on any bytes, and `encode` always gives the
/// same bytes back: every row is written again as soon as it is read and kept only if its bits come back
/// exactly (so rows that follow the coding procedure of the recommendations); from the first row that
/// doesn't, everything stays in `layout.pad` and `layout.trailing` as it is (see `stop`). Returns None only
/// for impossible parameters.
pub fn parse(data: &[u8], scheme: Scheme, columns: usize, byte_align: bool, limits: Limits) -> Option<Parsed> {
    if columns == 0 || columns > MAX_COLUMNS {
        return None;
    }
    let mut r = Reader::new(data);
    let mut image = Image::new(columns);
    let mut gaps = Vec::new();
    let mut end = Vec::new();
    let mut prev: Vec<u32> = Vec::new();
    let mut cur: Vec<u32> = Vec::new();
    // The rules every row so far is consistent with (bit i: Rule::ALL[i]).
    let mut rules: u32 = (1 << Rule::ALL.len()) - 1;
    let mut why = "";
    let stop;
    loop {
        let save = r.pos;
        let g = match read_gap(&mut r, scheme, byte_align) {
            GapRead::Got(g) => g,
            GapRead::End => {
                r.pos = save;
                stop = Stop::EndOfData;
                break;
            }
            GapRead::Bad => {
                r.pos = save;
                stop = Stop::Undecodable;
                break;
            }
        };
        if g.eol {
            // Two EOLs in a row end the data (no row is empty of codes); so does one EOL with only 0 bits after.
            let save2 = r.pos;
            match read_gap(&mut r, scheme, byte_align) {
                GapRead::Got(g2) if g2.eol => {
                    end.push(g);
                    end.push(g2);
                    loop {
                        let s3 = r.pos;
                        match read_gap(&mut r, scheme, byte_align) {
                            GapRead::Got(g3) if g3.eol => end.push(g3),
                            _ => {
                                r.pos = s3;
                                break;
                            }
                        }
                    }
                    stop = Stop::EndCodes;
                    break;
                }
                GapRead::End => {
                    r.pos = save2;
                    end.push(g);
                    stop = Stop::EndCodes;
                    break;
                }
                _ => r.pos = save2,
            }
        }
        let rows = image.rows();
        if rows >= limits.max_rows || (rows as u64 + 1) * columns as u64 > limits.max_pixels || image.changes.len() > limits.max_changes {
            r.pos = save;
            stop = Stop::Limit;
            break;
        }
        let two_d = match scheme {
            Scheme::G4 => true,
            Scheme::G31D => false,
            Scheme::G32D => !g.tag,
        };
        cur.clear();
        let ok = if two_d { decode_2d(&mut r, &prev, &mut cur, columns) } else { decode_1d(&mut r, &mut cur, columns) };
        let mut still = 0;
        if ok.is_ok() {
            for (i, &rule) in Rule::ALL.iter().enumerate() {
                if rules >> i & 1 == 1 {
                    let mut w = Writer::new();
                    write_row(&mut w, scheme, rule, &g, &prev, &cur, columns);
                    if w.bit_pos() == r.pos - save && same_bits(data, save, &w) {
                        still |= 1 << i;
                    }
                }
            }
        }
        if still == 0 {
            why = match ok {
                Err(e) => e,
                Ok(()) => "row coded differently from the standard procedure",
            };
            r.pos = save;
            stop = Stop::Undecodable;
            break;
        }
        rules = still;
        gaps.push(g);
        image.push_row(&cur);
        std::mem::swap(&mut prev, &mut cur);
    }
    let rule = Rule::ALL[rules.trailing_zeros() as usize];
    let p = r.pos;
    let pad_bits = ((8 - (p & 7)) & 7) as u32;
    let pad = if pad_bits > 0 { r.peek(pad_bits) as u8 } else { 0 };
    let trailing = data[p.div_ceil(8)..].to_vec();
    Some(Parsed { image, layout: Layout { scheme, rule, gaps, end, pad, trailing }, stop, why })
}

/// One row with what comes before it; `prev` is the reference row (the row above).
fn write_row(wr: &mut Writer, scheme: Scheme, rule: Rule, gap: &Gap, prev: &[u32], changes: &[u32], width: usize) {
    put_gap(wr, scheme, gap);
    let two_d = match scheme {
        Scheme::G4 => true,
        Scheme::G31D => false,
        Scheme::G32D => !gap.tag,
    };
    if two_d {
        encode_2d(wr, prev, changes, width, rule);
    } else {
        encode_1d(wr, changes, width);
    }
}

/// Writes an image with a recorded layout. `layout.gaps` must have one entry per row.
pub fn encode(image: &Image, layout: &Layout) -> Vec<u8> {
    let mut wr = Writer::new();
    let mut enc = RowEncoder::new(image.width, layout.scheme, layout.rule);
    for r in 0..image.rows() {
        enc.row(&mut wr, &layout.gaps[r], image.row(r));
    }
    enc.finish(wr, layout)
}

/// Writes rows one at a time (for callers that produce rows as they go).
pub struct RowEncoder {
    width: usize,
    scheme: Scheme,
    rule: Rule,
    prev: Vec<u32>,
}

impl RowEncoder {
    pub fn new(width: usize, scheme: Scheme, rule: Rule) -> RowEncoder {
        RowEncoder { width, scheme, rule, prev: Vec::new() }
    }

    pub fn row(&mut self, wr: &mut Writer, gap: &Gap, changes: &[u32]) {
        write_row(wr, self.scheme, self.rule, gap, &self.prev, changes, self.width);
        self.prev.clear();
        self.prev.extend_from_slice(changes);
    }
    /// The end codes, the padding bits and the trailing bytes.
    pub fn finish(&mut self, mut wr: Writer, layout: &Layout) -> Vec<u8> {
        for g in &layout.end {
            put_gap(&mut wr, self.scheme, g);
        }
        let mut out = wr.finish(layout.pad);
        out.extend_from_slice(&layout.trailing);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// A test image: text-like blobs, rows that repeat, full black and full white rows, edges at both ends.
    fn sample(w: usize, h: usize, seed: u64) -> Image {
        let mut s = seed;
        let mut img = Image::new(w);
        let mut px = vec![0u8; w];
        let mut ch = Vec::new();
        for y in 0..h {
            match y % 11 {
                0 => px.fill(0),
                1 => px.fill(1),
                2 | 3 => {} // same as the row above
                _ => {
                    for x in 0..w {
                        let r = rng(&mut s);
                        // Mostly keep the pixel above; sometimes flip; runs of black.
                        if r.is_multiple_of(7) {
                            px[x] ^= 1;
                        } else if r.is_multiple_of(13) && x + 5 < w {
                            px[x..x + 5].fill(1);
                        }
                    }
                    if y % 5 == 0 {
                        px[0] = 1;
                        px[w - 1] = 1;
                    }
                }
            }
            row_changes(&px, &mut ch);
            img.push_row(&ch);
        }
        img
    }

    fn layout_for(img: &Image, scheme: Scheme, eol: bool, align: bool, rule: Rule) -> Layout {
        let mut gaps = Vec::new();
        for r in 0..img.rows() {
            gaps.push(Gap { fill: 0, eol, tag: scheme == Scheme::G32D && r % 3 == 0 });
        }
        let end = match scheme {
            Scheme::G4 => vec![Gap { fill: 0, eol: true, tag: false }; 2],
            Scheme::G32D => vec![Gap { fill: 0, eol: true, tag: true }; 6],
            Scheme::G31D => vec![Gap { fill: 0, eol: true, tag: false }; 6],
        };
        let mut l = Layout { scheme, rule, gaps, end, pad: 0, trailing: Vec::new() };
        if align {
            // Byte-aligned rows: the fill makes each row (or its EOL), and each end code, start at a byte boundary.
            let mut wr = Writer::new();
            let mut enc = RowEncoder::new(img.width, scheme, rule);
            for r in 0..img.rows() {
                let pos = wr.bit_pos();
                l.gaps[r].fill = ((8 - (pos & 7)) & 7) as u32;
                let g = l.gaps[r];
                enc.row(&mut wr, &g, img.row(r));
            }
            for e in 0..l.end.len() {
                let pos = wr.bit_pos();
                l.end[e].fill = ((8 - (pos & 7)) & 7) as u32;
                put_gap(&mut wr, scheme, &l.end[e]);
            }
        }
        l
    }

    #[test]
    fn round_trips_every_scheme_and_layout() {
        for &(w, h) in &[(1usize, 3usize), (7, 5), (8, 8), (9, 17), (63, 40), (64, 9), (65, 30), (1728, 20), (2561, 6), (2623, 4), (2624, 4), (5300, 6)] {
            for scheme in [Scheme::G4, Scheme::G31D, Scheme::G32D] {
                for (eol, align) in [(false, false), (true, false), (false, true), (true, true)] {
                    for rule in Rule::ALL {
                        let img = sample(w, h, (w * 31 + h) as u64);
                        let mut lay = layout_for(&img, scheme, eol, align, rule);
                        let bytes = encode(&img, &lay);
                        let p = parse(&bytes, scheme, w, align, Limits::default()).unwrap();
                        let what = format!("{w}x{h} {scheme:?} eol={eol} align={align} {rule:?}");
                        assert_eq!(p.stop, Stop::EndCodes, "{what}");
                        assert_eq!(p.image, img, "{what}");
                        // The rule found is the first one every row agrees with.
                        lay.rule = p.layout.rule;
                        assert_eq!(p.layout, lay, "{what}");
                        assert_eq!(encode(&p.image, &p.layout), bytes, "{what}");
                    }
                }
            }
        }
    }

    /// Rows where Adobe's encoder and the standard procedure differ: a reference row of many short runs under a
    /// long run (a chain of more than 5 passes), and a pass chain that ends in horizontal mode.
    fn adobe_sample() -> Image {
        let mut img = Image::new(200);
        let dots: Vec<u32> = (10..190).step_by(3).collect(); // runs of 3 pixels
        img.push_row(&dots);
        img.push_row(&[5, 180]); // one long black run under the dots
        img.push_row(&[5, 7, 100, 101]);
        img.push_row(&dots);
        img.push_row(&[40, 41]);
        img
    }

    #[test]
    fn adobe_rule_is_found_and_kept() {
        let img = adobe_sample();
        for rule in Rule::ALL {
            let lay = layout_for(&img, Scheme::G4, false, false, rule);
            let bytes = encode(&img, &lay);
            let p = parse(&bytes, Scheme::G4, 200, false, Limits::default()).unwrap();
            assert_eq!(p.stop, Stop::EndCodes);
            assert_eq!(p.image, img);
            assert_eq!(p.layout.rule, rule);
            assert_eq!(encode(&p.image, &p.layout), bytes);
        }
        let std = encode(&img, &layout_for(&img, Scheme::G4, false, false, Rule::Standard));
        let adobe = encode(&img, &layout_for(&img, Scheme::G4, false, false, Rule::Adobe));
        assert_ne!(std, adobe);
    }

    #[test]
    fn all_white_all_black_and_long_runs() {
        for w in [1usize, 2, 63, 64, 65, 1727, 1728, 1792, 2560, 2623, 2624, 2625, 5184, 7000] {
            let mut img = Image::new(w);
            img.push_row(&[]); // white
            img.push_row(&[0]); // black
            img.push_row(&[0]);
            img.push_row(&[]);
            if w > 2 {
                img.push_row(&[1, (w - 1) as u32]);
            }
            for scheme in [Scheme::G4, Scheme::G31D, Scheme::G32D] {
                let lay = layout_for(&img, scheme, false, false, Rule::Standard);
                let bytes = encode(&img, &lay);
                let p = parse(&bytes, scheme, w, false, Limits::default()).unwrap();
                assert_eq!(p.image, img, "{w} {scheme:?}");
                assert_eq!(encode(&p.image, &p.layout), bytes);
            }
        }
    }

    #[test]
    fn known_bits() {
        // One white row of 1728 in 1-D: make-up 1728 (010011011) and terminating 0 (00110101).
        let mut img = Image::new(1728);
        img.push_row(&[]);
        let lay = Layout { scheme: Scheme::G31D, rule: Rule::Standard, gaps: vec![Gap::default()], end: vec![], pad: 0, trailing: vec![] };
        let b = encode(&img, &lay);
        assert_eq!(b, vec![0b0100_1101, 0b1001_1010, 0b1000_0000]);
        // Group 4: a white row on a white reference is V0 ("1"); EOFB is two EOLs.
        let lay = Layout { scheme: Scheme::G4, rule: Rule::Standard, gaps: vec![Gap::default()], end: vec![Gap { fill: 0, eol: true, tag: false }; 2], pad: 0, trailing: vec![] };
        let b = encode(&img, &lay);
        assert_eq!(b, vec![0b1000_0000, 0b0000_1000, 0b0000_0000, 0b1000_0000]);
    }

    #[test]
    fn keeps_whatever_follows_and_damaged_data() {
        let img = sample(100, 12, 5);
        let mut lay = layout_for(&img, Scheme::G4, false, false, Rule::Adobe);
        lay.pad = 0b10_1101 & 0x3f;
        lay.trailing = b"\r\n%junk".to_vec();
        let bytes = encode(&img, &lay);
        let p = parse(&bytes, Scheme::G4, 100, false, Limits::default()).unwrap();
        assert_eq!(encode(&p.image, &p.layout), bytes);
        // Cut short, bits flipped, garbage: parsing never fails, and the layout keeps what isn't understood
        // (a flipped bit can still decode, but not to rows whose bits come back the same).
        let mut s = 77u64;
        for k in 0..3000 {
            let mut b = bytes.clone();
            match k % 3 {
                0 => b.truncate((rng(&mut s) as usize) % bytes.len()),
                1 => {
                    let i = (rng(&mut s) as usize) % b.len();
                    b[i] ^= 1 << (rng(&mut s) % 8);
                }
                _ => b = (0..200).map(|_| rng(&mut s) as u8).collect(),
            }
            for scheme in [Scheme::G4, Scheme::G31D, Scheme::G32D] {
                let p = parse(&b, scheme, 100, k % 2 == 0, Limits::default()).unwrap();
                assert_eq!(encode(&p.image, &p.layout), b, "case {k} {scheme:?}");
            }
        }
    }

    #[test]
    fn limits_hold() {
        // Every row white: one bit per row in Group 4. A limit stops the rows; the rest is kept as bytes.
        let bytes = vec![0xffu8; 1000];
        let p = parse(&bytes, Scheme::G4, 50_000, false, Limits { max_rows: 100, max_pixels: 1 << 40, max_changes: 1 << 20 }).unwrap();
        assert_eq!(p.stop, Stop::Limit);
        assert_eq!(p.image.rows(), 100);
        assert_eq!(encode(&p.image, &p.layout), bytes);
        assert!(parse(&bytes, Scheme::G4, 0, false, Limits::default()).is_none());
        assert!(parse(&bytes, Scheme::G4, MAX_COLUMNS + 1, false, Limits::default()).is_none());
    }

    #[test]
    fn work_counts_rows_by_what_coding_them_costs() {
        let mut img = Image::new(1000);
        img.push_row(&[]); // white, like the white row above the image: 1000 / 256 + 1
        img.push_row(&[0]); // differs: 1000
        img.push_row(&[0]); // the same: 4
        img.push_row(&[5, 9]); // differs: 1000
        assert_eq!(img.work(), 4 + 1000 + 4 + 1000);
        assert_eq!(row_work(100, true), 1);
        assert_eq!(row_work(131072, true), 513);
    }

    #[test]
    fn rows_expand_and_collapse() {
        let mut out = vec![0u8; 10];
        expand_row(&[0, 3, 9], &mut out);
        assert_eq!(out, vec![1, 1, 1, 0, 0, 0, 0, 0, 0, 1]);
        let mut ch = Vec::new();
        row_changes(&out, &mut ch);
        assert_eq!(ch, vec![0, 3, 9]);
    }
}
