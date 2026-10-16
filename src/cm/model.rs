//! The PDF-aware model: generic byte contexts, a long-match model, an "expected text" model, and
//! contexts that understand PDF syntax, page drawing instructions (content streams) and image rows.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Meta = 0,
    Syntax = 1,
    Content = 2,
    Image = 3,
    Font = 4,
    Data = 5,
    Raw = 6,
    /// The private part of an opened Type 1 font: PostScript text with charstrings (binary strings).
    Charstring = 7,
}

/// Number of kinds: per-kind tables are this many times larger.
const KINDS: usize = 8;

#[derive(Clone, Copy, Debug, Default)]
pub struct Geom {
    /// Bytes per row (0 when unknown).
    pub stride: usize,
    /// Bytes per pixel (at least 1).
    pub bpp: usize,
    /// Samples are symbols, not intensities (palette indices, or several pixels packed in a byte): modelled
    /// by their exact neighbours instead of as differences from a prediction.
    pub exact: bool,
}

const NCTX: usize = 10;
const NIN: usize = NCTX + 3;

#[inline]
fn h2(a: u32, b: u32) -> u32 {
    (a.wrapping_mul(0x2F0F_3A4B) ^ b.wrapping_mul(0x9E37_79B1)).rotate_left(13).wrapping_mul(0x85EB_CA6B)
}

#[inline]
fn hb(h: u32, b: u8) -> u32 {
    (h ^ b as u32).wrapping_mul(0x0100_0193)
}

/// Bit-history states: (n0, n1, last bit) with counts up to 7; the opposite count is halved when it is
/// large, so recent bits weigh more. 128 states; state 0 is "nothing seen".
pub struct States {
    next: [[u8; 2]; 128],
    init_p: [i32; 128],
}

fn st_unpack(s: usize) -> (usize, usize, usize) {
    (s >> 4 & 7, s >> 1 & 7, s & 1)
}

fn st_pack(n0: usize, n1: usize, last: usize) -> usize {
    n0 << 4 | n1 << 1 | last
}

impl States {
    pub fn new() -> States {
        let mut next = [[0u8; 2]; 128];
        let mut init_p = [2048i32; 128];
        for s in 0..128 {
            let (n0, n1, _) = st_unpack(s);
            let up = |a: usize, b: usize| -> (usize, usize) {
                // a: count of the bit seen (incremented), b: opposite count (discounted)
                ((a + 1).min(7), if b > 2 { b.div_ceil(2) } else { b })
            };
            let (a1, b1) = up(n1, n0);
            next[s][1] = st_pack(b1, a1, 1) as u8;
            let (a0, b0) = up(n0, n1);
            next[s][0] = st_pack(a0, b0, 0) as u8;
            init_p[s] = (((n1 as i32 * 2 + 1) * 4096) / ((n0 + n1) as i32 * 2 + 2)).clamp(64, 4032);
        }
        States { next, init_p }
    }
}

impl Default for States {
    fn default() -> Self {
        Self::new()
    }
}

struct Table {
    t: Vec<[u8; 16]>,
    mask: usize,
}

impl Table {
    fn new(bits: u32) -> Table {
        Table { t: vec![[0u8; 16]; 1 << bits], mask: (1 << bits) - 1 }
    }
    /// The bucket for hash `h` (cleared if another context had it) and a copy of it.
    #[inline]
    fn find(&mut self, h: u32) -> (usize, [u8; 16]) {
        let i = (h as usize) & self.mask;
        let chk = ((h >> 24) as u8) | 1;
        let b = &mut self.t[i];
        if b[0] != chk {
            *b = [0u8; 16];
            b[0] = chk;
        }
        (i, *b)
    }
    /// A bucket's check byte, read early and changing nothing, so that the `find` a little later finds the
    /// bucket in the cache (safe code has no prefetch; an early read is the nearest thing).
    #[inline]
    fn peek(&self, h: u32) -> u8 {
        self.t[(h as usize) & self.mask][0]
    }
}

/// Tokenizer state over PDF syntax and content streams.
#[derive(Default, Clone)]
struct Tok {
    in_tok: bool,
    tok: Vec<u8>,
    /// Hash of `tok`, kept up to date as bytes are added (hashing the whole token at every byte would take
    /// time growing with the square of its length).
    tok_hash: u32,

    name_hash: u32,
    since_name: u32,
    in_string: u32,
    // content streams
    operands: Vec<Vec<u8>>,
    prev_operands: Vec<Vec<u8>>,
    /// Hash of each of `prev_operands`, worked out once.
    prev_hashes: Vec<u32>,
    prev_op: u32,
    prev_op2: u32,
    // syntax: object numbering
    last_nums: [i64; 2],
    last_obj: i64,
    prev_token: Vec<u8>,
    xref_state: u8, // 0 none, 1 after "xref", 2 in entries
    xref_next: i64,
    xref_left: i64,
}

/// Inside an opened Type 1 private part: the text, and the charstrings in it ("length RD " and that many
/// bytes) whose tokens are followed, so the model knows what the next byte is.
#[derive(Default, Clone)]
struct Cs {
    /// Text: the token being read and the two before it.
    tok: Vec<u8>,
    prev: [Vec<u8>; 2],
    len_iv: i64,
    /// Charstring bytes left (0: in the text), and read so far.
    left: usize,
    pos: usize,
    /// What the next charstring byte is (CS_*), and the first byte of a longer number.
    phase: u8,
    lead: u8,
    acc: i32,
    /// Numbers since the last command, the last two numbers and commands, the name (or Subrs index) before
    /// the charstring.
    depth: u32,
    n1: i32,
    n2: i32,
    c1: u32,
    c2: u32,
    name: u32,
    /// The numbers given to the pending command so far, and to the last command.
    ops: [i32; 8],
    last: [i32; 8],
    nlast: u32,
}

const CS_TOKEN: u8 = 0;
const CS_POS2: u8 = 1;
const CS_NEG2: u8 = 2;
const CS_ESC: u8 = 3;
const CS_RANDOM: u8 = 4;
/// CS_INT..CS_INT + 3: the four bytes of a 32-bit number.
const CS_INT: u8 = 5;

impl Cs {
    fn new() -> Cs {
        Cs { len_iv: 4, ..Default::default() }
    }
    fn number(&mut self, v: i32) {
        self.n2 = self.n1;
        self.n1 = v;
        if self.depth < 8 {
            self.ops[self.depth as usize] = v;
        }
        self.depth += 1;
        self.phase = CS_TOKEN;
    }
    fn command(&mut self, c: u32) {
        self.c2 = self.c1;
        self.c1 = c;
        self.last = self.ops;
        self.nlast = self.depth;
        self.depth = 0;
        self.phase = CS_TOKEN;
    }
}

/// A decimal integer token (optional sign, at most 9 digits).
fn int_token(t: &[u8]) -> Option<i64> {
    let (neg, d) = match t.first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if d.is_empty() || d.len() > 9 || !d.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let v = d.iter().fold(0i64, |v, &c| v * 10 + (c - b'0') as i64);
    Some(if neg { -v } else { v })
}

/// Kind of charstring command: curve, line, move, hint, other (and none).
fn cmd_class(c: u32) -> usize {
    match c {
        8 | 30 | 31 => 1,
        5..=7 => 2,
        21 | 22 | 4 => 3,
        1 | 3 | 33 | 34 => 4,
        0 => 0,
        _ => 5,
    }
}

/// Sign and size class of a charstring number.
fn num_class(v: i32) -> u32 {
    (32 - v.unsigned_abs().leading_zeros()).min(15) | ((v < 0) as u32) << 4
}

pub struct Model {
    hist: Vec<u8>,
    /// For residual-coded images: the residuals (byte less its prediction) of the last positions before
    /// `rh_end`, in the current segment only (a residual is never read across a segment boundary, nor more than
    /// a row and a pixel back). A ring: position `p` is at `(p - seg_start) & rmask`, and `rmask + 1` is a power
    /// of two at least a row and a pixel. It grows only as the segment does, so a huge row length costs nothing
    /// until its bytes come, and its room is freed with the segment unless the next one can use all of it. Only
    /// `push_resid`, `grow_ring`, `fill_row`, `fill_residuals` and `set_kind` change these (`resid_at`,
    /// `hash_contexts` and a check in `code_full` read them).
    rhist: Vec<u8>,
    rmask: usize,
    rh_end: usize,
    /// Prediction subtracted from the current byte before coding (residual-coded images only). After a byte
    /// that skipped the full model it is stale until `contexts` works it out again.
    pred: u8,
    kind: Kind,
    geom: Geom,
    /// Bytes are coded as residuals (images with known rows, not symbols): set with the kind.
    resid: bool,
    /// The kind has a tracker (syntax, content, charstrings): set with the kind.
    tracks: bool,
    seg_start: usize,
    /// Column of the next byte in an image row: (hist.len() - seg_start) % geom.stride, kept up to date byte
    /// by byte (a division per byte is slow).
    col: usize,
    /// Original-file offset of the current syntax segment's first byte.
    seg_orig: u64,
    c0: u32,
    nib: u32,
    c4: u32,
    c8: u32,
    word: u32,
    hashes: [u32; NCTX],
    tables: [Table; NCTX],
    slot: [usize; NCTX],
    /// The hashes of the next slots, worked out (and their buckets read) at a nibble's last bit.
    next: [u32; NCTX],
    /// The current bucket of each context: a copy of `tables[i].t[slot[i]]`, the one that counts while the
    /// slot holds (written back before the slot changes, see `reslot`).
    cur: [[u8; 16]; NCTX],
    states: States,
    /// One map per kind and context (kind * NCTX + context), from bit-history state (always below 128) to
    /// probability.
    smaps: Box<[[Counter; 128]; KINDS * NCTX]>,
    // long matches
    mm_table: Box<[u32; MM_SIZE]>,
    mm_ptr: usize,
    mm_len: usize,
    mm_sm: StateMap<128>,
    mm_miss: bool,
    // expected text
    exp: Vec<u8>,
    exp_i: usize,
    exp_hits: u32,
    exp_sm: StateMap<128>,
    exp_miss: bool,
    mx: Mixer<NIN>,
    a1: Apm<{ 256 * KINDS }>,
    a2: Apm<65536>,
    tok: Tok,
    cs: Cs,
    /// Original offsets of objects seen ("N G obj"), for predicting cross-reference tables.
    obj_offsets: std::collections::HashMap<i64, (u64, i64)>,
    // Long repeats: one almost-certain decision ("same byte as the match") instead of eight.
    rep: [Counter; REP_N],
    rep_last: u32,
    /// Hash contexts are stale after bytes that skipped the full model; recomputed before the next full one.
    ctx_dirty: bool,
    /// Off while priming (the priming text is run twice to train the full model).
    pub bypass_on: bool,
    /// Plain coding for data that is already compressed: 8 even-odds bits per byte, history kept.
    flat: bool,
    /// Bytes coded by one long-repeat decision (for statistics).
    pub bypassed: u64,
}

/// What stays the same for the eight bits of a byte (see `Model::byte_cx`).
struct ByteCx {
    kind: usize,
    /// A long match: the matched byte (less the prediction) with a leading 1, the base of its state-map
    /// context, and its mixer state while it holds.
    mm_on: bool,
    mm_byte: u32,
    mm_cx: usize,
    mm_state: usize,
    /// Expected text: the expected byte with a leading 1 and the base of its state-map context.
    exp_on: bool,
    exp_byte: u32,
    exp_cx: usize,
    /// The mixer selector of the byte's first bit (without the match state), and the bases of the APM contexts.
    sel: usize,
    a1: usize,
    a2: usize,
}

// Fixed settings of the format. They are part of it: the decoder must use exactly the same values as the
// encoder, so they are constants (never read from the environment).
const STATE_LIMIT: u32 = 1023;
const MATCH_MIN: usize = 5;
/// A match at least this long codes the next byte with one decision (measured: 32, 64, 128 and 256; 128 is
/// the smallest on a random sample and on huge repetitive images).
const LONG_MIN: usize = 128;
const REP_LIMIT: u32 = 1023;
/// Long-repeat counters: kind x match length class x last decision x run.
const REP_N: usize = KINDS * 16 * 2 * 2;
/// The match table: 2^20 positions, found by the top bits of the hash of the last MATCH_MIN bytes.
const MM_BITS: u32 = 20;
const MM_SIZE: usize = 1 << MM_BITS;
/// Mixer weight sets for charstring bytes: at a token start 7 operand counts x 6 kinds of last command, plus 3
/// other places (end of a number, escaped command, other), each x 8 bits x 3 match states.
const CS_SEL: usize = 45 * 8 * 3;

/// For the word hash: each letter or digit in lower case, 0 for any other byte.
static WORD_BYTE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        if b.is_ascii_alphanumeric() {
            t[i] = b.to_ascii_lowercase();
        }
        i += 1;
    }
    t
};

/// The difference of two bytes in classes (`Q[d + 255]` for -255..255): 0, 1, 2, then up to 4, 8, 16, 32 and
/// more, plus 8 below 0.
static Q: [u8; 512] = {
    let mut t = [0u8; 512];
    let mut i = 0;
    while i < 511 {
        let d = i as i32 - 255;
        let m = match d.unsigned_abs() {
            0 => 0,
            1 => 1,
            2 => 2,
            3..=4 => 3,
            5..=8 => 4,
            9..=16 => 5,
            17..=32 => 6,
            _ => 7,
        };
        t[i] = if d < 0 { m + 8 } else { m };
        i += 1;
    }
    t
};

/// LOCO-I's median edge prediction from the left, upper and upper-left samples.
#[inline]
fn loco(w: i32, n: i32, nw: i32) -> i32 {
    if nw >= w.max(n) {
        w.min(n)
    } else if nw <= w.min(n) {
        w.max(n)
    } else {
        w + n - nw
    }
}

/// `loco` on bytes: the same value (in the third case it lies between `w` and `n`).
#[inline(always)]
fn loco8(w: u8, n: u8, nw: u8) -> u8 {
    let (lo, hi) = (w.min(n), w.max(n));
    if nw >= hi {
        lo
    } else if nw <= lo {
        hi
    } else {
        w.wrapping_add(n).wrapping_sub(nw)
    }
}

impl Model {
    pub fn new(table_bits: u32) -> Model {
        let tables = std::array::from_fn(|_| Table::new(table_bits));
        let states = States::new();
        let smap: [Counter; 128] = std::array::from_fn(|st| Counter((states.init_p[st] as u32) << 20));
        let mut m = Model {
            hist: Vec::new(),
            rhist: Vec::new(),
            rmask: 0,
            rh_end: 0,
            pred: 0,
            kind: Kind::Meta,
            geom: Geom { bpp: 1, ..Geom::default() },
            resid: false,
            tracks: false,
            seg_start: 0,
            col: 0,
            seg_orig: 0,
            c0: 1,
            nib: 1,
            c4: 0,
            c8: 0,
            word: 0,
            hashes: [0; NCTX],
            tables,
            slot: [0; NCTX],
            next: [0; NCTX],
            cur: [[0; 16]; NCTX],
            states,
            smaps: super::boxed(smap),
            mm_table: super::boxed(0),
            mm_ptr: 0,
            mm_len: 0,
            mm_sm: StateMap::new(1023),
            mm_miss: false,
            exp: Vec::new(),
            exp_i: 0,
            exp_hits: 0,
            exp_sm: StateMap::new(1023),
            exp_miss: false,
            mx: Mixer::new(KINDS * 8 * 3 + CS_SEL),
            a1: Apm::new(7),
            a2: Apm::new(7),
            tok: Tok::default(),
            cs: Cs::new(),
            obj_offsets: std::collections::HashMap::new(),
            rep: [Counter::new(); REP_N],
            rep_last: 1,
            ctx_dirty: false,
            bypass_on: true,
            flat: false,
            bypassed: 0,
        };
        // The last mixer input is a constant.
        m.mx.x[NIN - 1] = 256;
        m.contexts();
        m
    }

    /// After priming: forget object positions learned from the priming text.
    pub fn forget_document(&mut self) {
        self.obj_offsets.clear();
        self.tok = Tok::default();
        self.exp.clear();
        self.exp_i = 0;
    }

    /// Switches the kind of data that follows (at a byte boundary).
    pub fn set_kind(&mut self, kind: Kind, geom: Geom, orig_offset: u64) {
        self.kind = kind;
        // At least one byte per pixel (every use took the larger of it and 1).
        self.geom = Geom { bpp: geom.bpp.max(1), ..geom };
        self.resid = kind == Kind::Image && geom.stride > 0 && !geom.exact;
        self.tracks = matches!(kind, Kind::Syntax | Kind::Content | Kind::Charstring);
        self.seg_start = self.hist.len();
        // The ring holds a row and a pixel, the farthest back `contexts` reads (all of the segment if no power
        // of two that large fits, which the row lengths of `pack` never reach).
        let back = self.geom.stride.saturating_add(self.geom.bpp);
        self.rmask = if self.resid { back.checked_next_power_of_two().map_or(usize::MAX, |r| r - 1) } else { 0 };
        // Room this segment can't use goes, so a wide image's ring doesn't stay for the rest of the file.
        let full = if self.resid { self.rmask.saturating_add(1) } else { 0 };
        if self.rhist.capacity() > full {
            self.rhist = Vec::new();
        } else {
            self.rhist.clear();
        }
        self.rh_end = self.seg_start;
        self.col = 0;
        self.seg_orig = orig_offset;
        self.exp.clear();
        self.cs = Cs::new();
        let keep_obj = (self.tok.last_obj, self.tok.xref_state, self.tok.xref_next, self.tok.xref_left);
        self.tok = Tok::default();
        self.tok.last_obj = keep_obj.0;
        if kind == Kind::Syntax {
            self.tok.xref_state = keep_obj.1;
            self.tok.xref_next = keep_obj.2;
            self.tok.xref_left = keep_obj.3;
        }
        self.contexts();
    }

    /// Plain coding for the part that follows (already-compressed data), or back to the full model.
    pub fn set_flat(&mut self, flat: bool) {
        self.flat = flat;
    }

    /// Codes one byte (`byte` when encoding) and returns it.
    #[inline(never)]
    pub fn code_byte<C: Coder>(&mut self, c: &mut C, byte: Option<u8>) -> u8 {
        self.code_one(c, byte)
    }

    /// Codes `n` bytes (those of `data`, which holds them, when encoding) and hands each to `keep`: the same as
    /// `code_byte` for each of them, with the long-repeat shortcut inside the loop.
    pub fn code_bytes<C: Coder>(&mut self, c: &mut C, data: Option<&[u8]>, n: usize, mut keep: impl FnMut(u8)) {
        match data {
            Some(d) => {
                for &x in &d[..n] {
                    keep(self.code_one(c, Some(x)));
                }
            }
            None => {
                for _ in 0..n {
                    keep(self.code_one(c, None));
                }
            }
        }
    }

    #[inline(always)]
    fn code_one<C: Coder>(&mut self, c: &mut C, byte: Option<u8>) -> u8 {
        // A long repeat: one almost-certain decision for the whole byte.
        if self.bypass_on && self.mm_len >= LONG_MIN && !self.mm_miss {
            let pb = self.hist[self.mm_ptr];
            // The last byte is c4's low byte: the history holds at least mm_len bytes.
            let run = (self.c4 as u8 == pb) as usize;
            let lb = ((usize::BITS - self.mm_len.leading_zeros()) as usize).min(15);
            // Below REP_N (kind below 8, lb at most 15, the last decision and run 0 or 1): the mask changes
            // nothing but spares the bounds check.
            let cx = (((self.kind as usize * 16 + lb) * 2 + self.rep_last as usize) * 2 + run) & (REP_N - 1);
            let p = self.rep[cx].p16();
            let hit = c.code16(byte.map(|b| (b == pb) as u32), p);
            self.rep[cx].update(hit, REP_LIMIT);
            self.rep_last = hit;
            if hit == 1 {
                self.bypassed += 1;
                self.repeat_byte(pb);
                return pb;
            }
            // Not the matched byte: the match model has nothing to say about this one.
            self.mm_miss = true;
        }
        self.code_full(c, byte)
    }

    /// A byte through the full model, or plainly.
    #[inline(never)]
    fn code_full<C: Coder>(&mut self, c: &mut C, byte: Option<u8>) -> u8 {
        if self.flat {
            let mut v = 0u32;
            for k in (0..8).rev() {
                v = v << 1 | c.code16(byte.map(|b| ((b >> k) & 1) as u32), 32768);
            }
            let b = v as u8;
            self.skip_byte(b);
            return b;
        }
        if self.ctx_dirty {
            self.contexts();
        }
        let bc = self.byte_cx();
        let sym = byte.map(|b| b.wrapping_sub(self.pred));
        let mut b = 0;
        for k in (0..8).rev() {
            let bit = sym.map(|b| ((b >> k) & 1) as u32);
            let (p, states) = self.predict(&bc, k);
            let got = c.code(bit, p);
            self.update_side(&bc, k, got);
            if k == 0 {
                // The byte is complete. Its bookkeeping and the next byte's hashes come before the last bit's
                // updates, which touch none of what they read or write, so the next buckets are read early.
                let r = ((self.c0 << 1 | got) & 0xff) as u8;
                b = r.wrapping_add(self.pred);
                if self.resid {
                    debug_assert_eq!(self.rh_end, self.hist.len());
                    self.push_resid(r);
                }
                self.byte_done(b);
                self.hash_contexts();
                self.peek_next(1);
            }
            self.update(&bc, k, &states, got);
        }
        self.c0 = 1;
        self.nib = 1;
        for i in 0..NCTX {
            self.reslot(i, self.next[i]);
        }
        b
    }

    /// What stays the same for the eight bits of a byte, worked out once.
    #[inline(always)]
    fn byte_cx(&self) -> ByteCx {
        let kind = self.kind as usize & (KINDS - 1);
        // For residual-coded images the match predicts the residual the matched byte would give.
        let mm_byte = self.hist.get(self.mm_ptr).map_or(0, |&b| b.wrapping_sub(self.pred) as u32 | 256);
        let exp_byte = self.exp.get(self.exp_i).map_or(0, |&b| b as u32 | 256);
        let sel = if self.kind == Kind::Charstring && self.cs.left > 0 {
            let at = match self.cs.phase {
                CS_TOKEN => (self.cs.depth as usize).min(6) * 6 + cmd_class(self.cs.c1),
                CS_POS2 | CS_NEG2 => 42,
                CS_ESC => 43,
                _ => 44,
            };
            KINDS * 8 * 3 + at * 8 * 3
        } else {
            kind * 8 * 3
        };
        ByteCx {
            kind,
            mm_on: self.mm_len > 0,
            mm_byte,
            mm_cx: self.mm_len.min(63) * 2,
            mm_state: 1 + (self.mm_len >= 24) as usize,
            exp_on: self.exp_i < self.exp.len() && !self.resid,
            exp_byte,
            exp_cx: self.exp_hits.min(15) as usize * 8 + (self.kind as usize % 4),
            sel,
            a1: 256 * kind,
            a2: ((self.c4 & 0xff) as usize) << 8,
        }
    }

    /// The probability of the next bit (`k`: its place in the byte, 7 first), and the contexts' states.
    #[inline(always)]
    fn predict(&mut self, bc: &ByteCx, k: u32) -> (i32, [u8; NCTX]) {
        // Nodes are 1..15, states below 128 and kinds below 8: the masks change nothing but spare the bounds
        // checks.
        let node = self.nib as usize & 15;
        let kb = (bc.kind & (KINDS - 1)) * NCTX;
        let sm = &self.smaps[kb..kb + NCTX];
        let x = &mut self.mx.x;
        let mut states = [0u8; NCTX];
        for i in 0..NCTX {
            let state = self.cur[i][node] & 127;
            states[i] = state;
            let p = sm[i][state as usize].p12();
            x[i] = if state == 0 { 0 } else { stretch(p) as i16 };
        }
        // Long match.
        let mut mm_state = 0;
        x[NCTX] = if bc.mm_on && !self.mm_miss {
            let expect = (bc.mm_byte >> k) & 1;
            mm_state = bc.mm_state;
            stretch(self.mm_sm.p(bc.mm_cx + expect as usize)) as i16
        } else {
            0
        };
        // Expected text.
        x[NCTX + 1] = if bc.exp_on && !self.exp_miss {
            let expect = (bc.exp_byte >> k) & 1;
            stretch(self.exp_sm.p(bc.exp_cx + expect as usize * 4)) as i16
        } else {
            0
        };
        let pm = self.mx.mix(bc.sel + (7 - k as usize) * 3 + mm_state);
        // The APMs refine the mixer's probability, stretched once for both.
        let st = stretch(pm);
        let p1 = self.a1.p(st, bc.a1 + self.c0 as usize);
        let p2 = self.a2.p(st, bc.a2 | self.c0 as usize);
        (((pm + p1 + 2 * p2 + 2) >> 2).clamp(1, 4095), states)
    }

    /// Learns the coded bit `k` in the match and expected-text models; at the first nibble's last bit, works out
    /// the next slots and reads their buckets.
    #[inline(always)]
    fn update_side(&mut self, bc: &ByteCx, k: u32, bit: u32) {
        if bc.mm_on && !self.mm_miss {
            self.mm_sm.update(bit);
            if (bc.mm_byte >> k) & 1 != bit {
                self.mm_miss = true;
            }
        }
        if bc.exp_on && !self.exp_miss {
            self.exp_sm.update(bit);
            if (bc.exp_byte >> k) & 1 != bit {
                self.exp_miss = true;
            }
        }
        if k == 4 {
            // Read before the updates that follow, so that their cache misses overlap that work.
            self.peek_next(self.c0 << 1 | bit);
        }
    }

    /// Learns the coded bit `k` in the contexts, the mixer and the APMs; `states` are the contexts' states
    /// `predict` gave.
    #[inline(always)]
    fn update(&mut self, bc: &ByteCx, k: u32, states: &[u8; NCTX], bit: u32) {
        let node = self.nib as usize & 15;
        let kb = (bc.kind & (KINDS - 1)) * NCTX;
        let sm = &mut self.smaps[kb..kb + NCTX];
        for i in 0..NCTX {
            let state = states[i] as usize & 127;
            sm[i][state].update(bit, STATE_LIMIT);
            self.cur[i][node] = self.states.next[state][bit as usize & 1];
        }
        self.mx.update(bit);
        self.a1.update(bit);
        self.a2.update(bit);
        self.c0 = (self.c0 << 1) | bit;
        self.nib = (self.nib << 1) | bit;
        if k == 4 {
            // The first nibble is done: the buckets of the second (worked out by `update_side`).
            self.nib = 1;
            for i in 0..NCTX {
                self.reslot(i, self.next[i]);
            }
        }
    }

    /// The hashes of the next slots, for a nibble that starts with `c0`, and an early read of their buckets.
    #[inline(always)]
    fn peek_next(&mut self, c0: u32) {
        let mut any = 0;
        for i in 0..NCTX {
            self.next[i] = h2(self.hashes[i], c0);
            any |= self.tables[i].peek(self.next[i]);
        }
        // Keeps the reads, whose values nothing uses.
        std::hint::black_box(any);
    }

    /// Writes the current bucket of context `i` back to its table, then finds the one for hash `h` and copies
    /// it (the write comes first: the find may pick the same bucket).
    #[inline(always)]
    fn reslot(&mut self, i: usize, h: u32) {
        let t = &mut self.tables[i];
        t.t[self.slot[i]] = self.cur[i];
        let (s, b) = t.find(h);
        self.slot[i] = s;
        self.cur[i] = b;
    }

    /// A byte the full model or plain coding gave: history, matches, expected text and the trackers follow it.
    #[inline(always)]
    fn byte_done(&mut self, b: u8) {
        self.push_byte(b);
        // Long match bookkeeping.
        if self.mm_len > 0 {
            if !self.mm_miss && self.hist[self.mm_ptr] == b {
                self.mm_len += 1;
                self.mm_ptr += 1;
            } else {
                self.mm_len = 0;
            }
        }
        self.mm_miss = false;
        self.match_table();
        self.after_byte(b);
    }

    /// A byte the long-repeat decision gave: the match's next byte with no miss before it (or the decision
    /// would not have been coded), so the match goes on. The same as `skip_byte` for that byte.
    #[inline(always)]
    fn repeat_byte(&mut self, b: u8) {
        self.push_byte(b);
        self.mm_len += 1;
        self.mm_ptr += 1;
        self.match_table();
        self.after_byte(b);
        self.ctx_dirty = true;
    }

    /// A byte coded plainly: the hash contexts (and in images the residual and the next prediction) are
    /// worked out only when the full model is needed again.
    #[inline(always)]
    fn skip_byte(&mut self, b: u8) {
        self.byte_done(b);
        self.ctx_dirty = true;
    }

    /// The history, the image column, the last bytes and the word follow the byte.
    #[inline(always)]
    fn push_byte(&mut self, b: u8) {
        self.hist.push(b);
        if self.geom.stride > 0 {
            self.col += 1;
            if self.col == self.geom.stride {
                self.col = 0;
            }
        }
        self.c8 = (self.c8 << 8) | (self.c4 >> 24);
        self.c4 = (self.c4 << 8) | b as u32;
        let w = WORD_BYTE[b as usize];
        self.word = if w != 0 { hb(self.word, w) } else { 0 };
    }

    /// The hash of the last MATCH_MIN bytes, oldest first: c8's low byte, then c4's four.
    #[inline(always)]
    fn match_hash(&self) -> u32 {
        let c = self.c4;
        hb(hb(hb(hb(hb(0, self.c8 as u8), (c >> 24) as u8), (c >> 16) as u8), (c >> 8) as u8), c as u8)
    }

    /// The match table learns the position after the last MATCH_MIN bytes, and gives a match when there is none.
    #[inline(always)]
    fn match_table(&mut self) {
        let n = self.hist.len();
        if n >= MATCH_MIN {
            let idx = (self.match_hash() >> (32 - MM_BITS)) as usize & (MM_SIZE - 1);
            if self.mm_len == 0 {
                self.match_start(self.mm_table[idx] as usize, n);
            }
            self.mm_table[idx] = n as u32;
        }
    }

    /// A match at `cand` when the bytes before it are the last bytes of the history: at least MATCH_MIN of
    /// them, compared up to 64 back.
    #[inline]
    fn match_start(&mut self, cand: usize, n: usize) {
        if cand > 0 && cand < n {
            let h = &self.hist[..n];
            let l = h[..cand].iter().rev().zip(h.iter().rev()).take(64).take_while(|(a, b)| a == b).count();
            if l >= MATCH_MIN {
                self.mm_len = l;
                self.mm_ptr = cand;
            }
        }
    }

    /// Expected text and the trackers follow the byte.
    #[inline(always)]
    fn after_byte(&mut self, b: u8) {
        if self.exp_i < self.exp.len() {
            if !self.exp_miss && self.exp[self.exp_i] == b {
                self.exp_i += 1;
                self.exp_hits += 1;
            } else {
                self.exp.clear();
                self.exp_i = 0;
                self.exp_hits = 0;
            }
        }
        self.exp_miss = false;
        if self.tracks {
            match self.kind {
                Kind::Syntax => self.syntax_byte(b),
                Kind::Content => self.content_byte(b),
                _ => self.charstring_byte(b),
            }
        }
    }

    /// Follows the text of a Type 1 private part to its charstrings, and the tokens inside them.
    #[inline(never)]
    fn charstring_byte(&mut self, b: u8) {
        let cs = &mut self.cs;
        if cs.left > 0 {
            cs.left -= 1;
            cs.pos += 1;
            match cs.phase {
                CS_RANDOM => {
                    if cs.pos as i64 >= cs.len_iv {
                        cs.phase = CS_TOKEN;
                    }
                }
                CS_TOKEN => match b {
                    12 => cs.phase = CS_ESC,
                    0..=31 => cs.command(b as u32),
                    32..=246 => cs.number(b as i32 - 139),
                    247..=254 => {
                        cs.phase = if b < 251 { CS_POS2 } else { CS_NEG2 };
                        cs.lead = b;
                    }
                    _ => {
                        cs.phase = CS_INT;
                        cs.acc = 0;
                    }
                },
                CS_POS2 => cs.number((cs.lead as i32 - 247) * 256 + b as i32 + 108),
                CS_NEG2 => cs.number(-(cs.lead as i32 - 251) * 256 - b as i32 - 108),
                CS_ESC => cs.command(32 + b as u32),
                _ => {
                    cs.acc = cs.acc << 8 | b as i32;
                    cs.phase += 1;
                    if cs.phase == CS_INT + 4 {
                        let v = cs.acc;
                        cs.number(v);
                    }
                }
            }
            return;
        }
        let is_ws = matches!(b, 0 | 9 | 10 | 12 | 13 | 32);
        if !is_ws && !matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%') {
            // Only short tokens matter (RD, -|, /lenIV, numbers).
            if cs.tok.len() < 32 {
                cs.tok.push(b);
            }
            return;
        }
        if !cs.tok.is_empty() {
            let t = std::mem::take(&mut cs.tok);
            // "length RD " (or -|): the charstring starts after this one white-space byte.
            if (t == b"RD" || t == b"-|") && is_ws {
                if let Some(n) = int_token(&cs.prev[1]).filter(|&n| n > 0) {
                    let name = cs.prev[0].iter().fold(0x811C_9DC5, |h, &x| hb(h, x));
                    *cs = Cs { len_iv: cs.len_iv, left: n as usize, name, ..Cs::new() };
                    cs.phase = if cs.len_iv > 0 { CS_RANDOM } else { CS_TOKEN };
                    return;
                }
            }
            if cs.prev[1] == b"/lenIV" {
                if let Some(v) = int_token(&t) {
                    cs.len_iv = v;
                }
            }
            cs.prev[0] = std::mem::replace(&mut cs.prev[1], t);
        }
        if b == b'/' {
            cs.tok.push(b);
        }
    }

    /// The residual of position `p`: one of the last `rmask + 1` stored, from this segment.
    #[inline]
    fn resid_at(&self, p: usize) -> u8 {
        self.rhist[(p - self.seg_start) & self.rmask]
    }

    /// Stores the residual of position `rh_end`.
    #[inline]
    fn push_resid(&mut self, r: u8) {
        let i = (self.rh_end - self.seg_start) & self.rmask;
        if i >= self.rhist.len() {
            self.grow_ring(i + 1);
        }
        self.rhist[i] = r;
        self.rh_end += 1;
    }

    /// Makes the ring at least `n` bytes long, or its full `rmask + 1`. Its room grows by doubling (a ring filled
    /// byte by byte grows in few steps), never past the full size.
    #[inline(never)]
    fn grow_ring(&mut self, n: usize) {
        let full = self.rmask.saturating_add(1);
        let n = n.min(full);
        if self.rhist.len() < n {
            if self.rhist.capacity() < n {
                let room = n.max(2 * self.rhist.capacity()).min(full);
                self.rhist.reserve_exact(room - self.rhist.len());
            }
            self.rhist.resize(n, 0);
        }
    }

    /// Stores the residuals of positions `p..end` (`p` is `rh_end`), all in one row, past its first pixel and
    /// below the first row: the left, upper and upper-left bytes are all in the segment.
    fn fill_row(&mut self, p: usize, end: usize) {
        let (s, bpp) = (self.geom.stride, self.geom.bpp);
        let h = &self.hist;
        let (cur, w, n, nw) = (&h[p..end], &h[p - bpp..end - bpp], &h[p - s..end - s], &h[p - s - bpp..end - s - bpp]);
        // The ring holds these positions (see `fill_residuals`); they may go past its end and on at its start.
        let i = (p - self.seg_start) & self.rmask;
        let k = (end - p).min(self.rhist.len() - i);
        for (at, from, to) in [(i, 0, k), (0, k, end - p)] {
            let dst = &mut self.rhist[at..at + (to - from)];
            let src = cur[from..to].iter().zip(&w[from..to]).zip(&n[from..to]).zip(&nw[from..to]);
            for (d, (((&c, &w), &n), &nw)) in dst.iter_mut().zip(src) {
                *d = c.wrapping_sub(loco8(w, n, nw));
            }
        }
        self.rh_end = end;
    }

    /// The residuals the contexts may read (a row and a pixel back) that are not stored yet: bytes coded by a
    /// long repeat or plainly store none. A residual is the byte less the prediction `contexts` makes at its
    /// position (LOCO-I's, from `neighbours_at`), which depends on the history only, so these are the values
    /// those bytes would have stored one by one.
    #[inline(never)]
    fn fill_residuals(&mut self) {
        let pos = self.hist.len();
        let lo = self.seg_start;
        let (s, bpp) = (self.geom.stride, self.geom.bpp);
        // Those more than a row and a pixel back are never read: a longer gap is skipped.
        self.rh_end = self.rh_end.max(pos.saturating_sub(s.saturating_add(bpp)).max(lo));
        self.grow_ring(pos - lo);
        let mut p = self.rh_end;
        // One division per fill.
        let mut col = (p - lo) % s;
        while p < pos {
            if col < bpp || p - lo < s {
                // The first pixel of a row, or the first row: the general code.
                let (w, n, nw, _) = self.neighbours_at(p, col);
                let r = self.hist[p].wrapping_sub(loco(w, n, nw) as u8);
                self.push_resid(r);
                p += 1;
                col += 1;
            } else {
                // The rest of the row, up to the next byte.
                let end = pos.min(p + (s - col));
                self.fill_row(p, end);
                col += end - p;
                p = end;
            }
            if col == s {
                col = 0;
            }
        }
    }

    /// For images with known rows: the left, upper, upper-left and upper-right samples of the image byte at
    /// `p`, in column `col` (LOCO-I's median edge prediction is made from the first three).
    #[inline]
    fn neighbours_at(&self, p: usize, col: usize) -> (i32, i32, i32, i32) {
        let s = self.geom.stride;
        let bpp = self.geom.bpp;
        let lo = self.seg_start;
        let at = |back: usize| -> i32 {
            if p >= back && p - back >= lo {
                self.hist[p - back] as i32
            } else {
                0
            }
        };
        let first_row = p - lo < s;
        let w = if col >= bpp {
            at(bpp)
        } else if !first_row {
            at(s)
        } else {
            0
        };
        let n = if !first_row { at(s) } else { w };
        let nw = if !first_row && col >= bpp { at(s + bpp) } else { n };
        let ne = if !first_row && col + bpp < s { at(s - bpp) } else { n };
        (w, n, nw, ne)
    }

    /// For symbol images: the channel and the exact left, upper, upper-left, upper-right, second-left and
    /// second-upper bytes of the next byte (0 outside the image).
    fn exact_neighbours(&self) -> [u32; 7] {
        let pos = self.hist.len();
        let s = self.geom.stride;
        let bpp = self.geom.bpp;
        let lo = self.seg_start;
        let at = |back: usize| -> u32 {
            if pos >= back && pos - back >= lo {
                self.hist[pos - back] as u32
            } else {
                0
            }
        };
        let col = self.col;
        debug_assert_eq!(col, (pos - lo) % s);
        let up = pos - lo >= s;
        let w = if col >= bpp { at(bpp) } else { 0 };
        let ww = if col >= 2 * bpp { at(2 * bpp) } else { 0 };
        let n = if up { at(s) } else { 0 };
        let nw = if up && col >= bpp { at(s + bpp) } else { 0 };
        let ne = if up && col + bpp < s { at(s - bpp) } else { 0 };
        let nn = if pos - lo >= 2 * s { at(2 * s) } else { 0 };
        let ch = if bpp == 1 { 0 } else { (col % bpp) as u32 };
        [ch, w, n, nw, ne, ww, nn]
    }

    fn tok_push(&mut self, b: u8) {
        self.tok.tok.push(b);
        self.tok.tok_hash = hb(self.tok.tok_hash, b);
    }

    fn token_end(&mut self) -> Option<Vec<u8>> {
        if self.tok.in_tok {
            self.tok.in_tok = false;
            self.tok.tok_hash = 0;
            let t = std::mem::take(&mut self.tok.tok);
            Some(t)
        } else {
            None
        }
    }

    #[inline(never)]
    fn syntax_byte(&mut self, b: u8) {
        let is_ws = matches!(b, 0 | 9 | 10 | 12 | 13 | 32);
        let is_delim = matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%');
        if self.tok.in_string > 0 {
            if b == b')' {
                self.tok.in_string -= 1;
            } else if b == b'(' {
                self.tok.in_string += 1;
            }
            return;
        }
        if is_ws || is_delim {
            if let Some(t) = self.token_end() {
                self.syntax_token(&t);
            }
            if b == b'/' {
                self.tok.in_tok = true;
                self.tok_push(b);
            } else if b == b'(' {
                self.tok.in_string = 1;
            }
            if b == b'\n' {
                self.syntax_newline();
            }
        } else {
            self.tok.in_tok = true;
            self.tok_push(b);
        }
        self.tok.since_name = (self.tok.since_name + 1).min(15);
    }

    fn syntax_token(&mut self, t: &[u8]) {
        if t.first() == Some(&b'/') {
            self.tok.name_hash = t.iter().fold(0x811C_9DC5, |h, &x| hb(h, x));
            self.tok.since_name = 0;
        }
        let num: Option<i64> = if !t.is_empty() && t.iter().all(|c| c.is_ascii_digit()) && t.len() < 12 {
            std::str::from_utf8(t).ok().and_then(|s| s.parse().ok())
        } else {
            None
        };
        if t == b"obj" {
            let (n, g) = (self.tok.last_nums[0], self.tok.last_nums[1]);
            self.tok.last_obj = n;
            // Where "n g obj" started in the original file: back over the two numbers and spaces.
            let start = self.obj_header_start();
            self.obj_offsets.insert(n, (start, g));
        }
        if t == b"xref" {
            self.tok.xref_state = 1;
        }
        if let Some(v) = num {
            self.tok.last_nums = [self.tok.last_nums[1], v];
        }
        self.tok.prev_token = t.to_vec();
    }

    fn obj_header_start(&self) -> u64 {
        // hist: ... N G obj  — walk back over "obj", spaces, digits, spaces, digits.
        let end = self.hist.len() - 1; // the delimiter after "obj"
        let mut i = end.saturating_sub(3);
        let mut phase = 0;
        while i > self.seg_start {
            let c = self.hist[i - 1];
            let digit = c.is_ascii_digit();
            let ws = matches!(c, 9 | 10 | 12 | 13 | 32 | 0);
            match phase {
                0 | 2 if ws => {}
                0 | 2 if digit => phase += 1,
                1 | 3 if digit => {}
                1 if ws => phase = 2,
                _ => break,
            }
            i -= 1;
        }
        self.seg_orig + (i - self.seg_start) as u64
    }

    fn syntax_newline(&mut self) {
        // After "endobj": the next object is probably number + 1.
        if self.tok.prev_token == b"endobj" && self.tok.xref_state == 0 {
            self.exp = format!("{} 0 obj", self.tok.last_obj + 1).into_bytes();
            self.exp_i = 0;
        }
        match self.tok.xref_state {
            1 if self.tok.prev_token != b"xref" => {
                // "start count" line ended.
                let (s, c) = (self.tok.last_nums[0], self.tok.last_nums[1]);
                self.tok.xref_next = s;
                self.tok.xref_left = c;
                self.tok.xref_state = 2;
                self.expect_xref_entry();
            }
            2 => {
                if self.tok.prev_token == b"n" || self.tok.prev_token == b"f" {
                    self.tok.xref_next += 1;
                    self.tok.xref_left -= 1;
                    if self.tok.xref_left > 0 {
                        self.expect_xref_entry();
                    } else {
                        self.tok.xref_state = 1;
                    }
                } else if self.tok.prev_token == b"trailer" {
                    self.tok.xref_state = 0;
                }
            }
            _ => {}
        }
    }

    fn expect_xref_entry(&mut self) {
        let n = self.tok.xref_next;
        if n == 0 {
            self.exp = b"0000000000 65535 f".to_vec();
        } else if let Some(&(off, g)) = self.obj_offsets.get(&n) {
            self.exp = format!("{off:010} {g:05} n").into_bytes();
        } else {
            self.exp.clear();
        }
        self.exp_i = 0;
    }

    #[inline(never)]
    fn content_byte(&mut self, b: u8) {
        let is_ws = matches!(b, 0 | 9 | 10 | 12 | 13 | 32);
        let is_delim = matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%');
        if self.tok.in_string > 0 {
            if b == b')' {
                self.tok.in_string -= 1;
            } else if b == b'(' {
                self.tok.in_string += 1;
            }
            return;
        }
        if is_ws || is_delim {
            if let Some(t) = self.token_end() {
                let is_operand = t[0] == b'/' || t[0].is_ascii_digit() || t[0] == b'-' || t[0] == b'.' || t[0] == b'+';
                if is_operand {
                    self.tok.operands.push(t);
                } else {
                    let h = t.iter().fold(0x811C_9DC5, |h, &x| hb(h, x));
                    self.tok.prev_op2 = self.tok.prev_op;
                    self.tok.prev_op = h;
                    self.tok.prev_operands = std::mem::take(&mut self.tok.operands);
                    self.tok.prev_hashes =
                        self.tok.prev_operands.iter().map(|v| v.iter().fold(7u32, |h, &x| hb(h, x))).collect();
                }
            }
            if b == b'/' {
                self.tok.in_tok = true;
                self.tok_push(b);
            } else if b == b'(' {
                self.tok.in_string = 1;
                self.tok.operands.push(b"(".to_vec());
            }
            // A new token may start next: expect the operand at the same place in the previous group.
            if is_ws {
                let k = self.tok.operands.len();
                if let Some(prev) = self.tok.prev_operands.get(k) {
                    self.exp = prev.clone();
                    self.exp_i = 0;
                }
            }
        } else {
            self.tok.in_tok = true;
            self.tok_push(b);
        }
    }

    /// The ten context hashes of the next byte, and the buckets of its first nibble.
    fn contexts(&mut self) {
        self.hash_contexts();
        for i in 0..NCTX {
            let h = h2(self.hashes[i], 1);
            self.reslot(i, h);
        }
    }

    /// The ten context hashes of the next byte (and in residual-coded images its prediction).
    fn hash_contexts(&mut self) {
        if self.resid && self.rh_end < self.hist.len() {
            self.fill_residuals();
        }
        self.ctx_dirty = false;
        self.pred = 0;
        let k = (self.kind as u32 + 1) << 28;
        let c4 = self.c4;
        let c1 = c4 & 0xff;
        let tokh = self.tok.tok_hash;
        let tl = self.tok.tok.len() as u32;
        let hs: [u32; NCTX] = match self.kind {
            Kind::Image if self.geom.stride > 0 && self.geom.exact => {
                let [ch, w, n, nw, ne, ww, nn] = self.exact_neighbours();
                let wn = ch | w << 8 | n << 16;
                [
                    h2(31, ch | w << 8),
                    h2(32, ch | n << 8),
                    h2(33, wn),
                    h2(34, h2(wn, nw)),
                    h2(35, ch | n << 8 | ne << 16),
                    h2(36, h2(wn, ne)),
                    h2(37, ch | w << 8 | ww << 16),
                    h2(38, ch | n << 8 | nn << 16),
                    h2(39, h2(wn, nw | ne << 8)),
                    h2(40, h2(c4 & 0xffff, n)),
                ]
            }
            Kind::Charstring if self.cs.left > 0 => {
                // A charstring byte: what it is (token start, end of a number, escaped command, random), the
                // numbers since the last command, the last commands, the size of the last numbers, the number
                // at the same place in the last command, and the glyph's name.
                let cs = &self.cs;
                let ph = cs.phase as u32;
                let depth = cs.depth.min(15);
                let lead = if cs.phase == CS_POS2 || cs.phase == CS_NEG2 { cs.lead as u32 } else { 0 };
                let st = ph | depth << 4 | cs.c1 << 8;
                let same = if cs.depth < cs.nlast.min(8) {
                    cs.last[cs.depth as usize].clamp(-4000, 4000) as u32 & 0xffff
                } else {
                    0x10000
                };
                [
                    h2(41, st),
                    h2(42, h2(st, cs.c2)),
                    h2(43, ph),
                    h2(44, h2(st | lead << 24, num_class(cs.n1))),
                    h2(45, ph | lead << 8 | depth << 16),
                    h2(46, h2(cs.name, st | lead << 24)),
                    h2(47, ph | c1 << 8 | depth << 16),
                    h2(48, h2(st | lead << 24, num_class(cs.n1) | num_class(cs.n2) << 8)),
                    h2(49, h2(st | lead << 24, same)),
                    h2(50, h2(c4, ph)),
                ]
            }
            Kind::Image if self.geom.stride > 0 => {
                let pos = self.hist.len();
                let s = self.geom.stride;
                let bpp = self.geom.bpp;
                let lo = self.seg_start;
                let col = self.col;
                debug_assert_eq!(col, (pos - lo) % s);
                let first_row = pos - lo < s;
                let rat = |back: usize| -> u32 {
                    if pos >= back && pos - back >= lo {
                        self.resid_at(pos - back) as u32
                    } else {
                        0
                    }
                };
                let (w, n, nw, ne, rw, rn, rnw, rne);
                if !first_row && col >= bpp && col + bpp < s {
                    // Inside the image: every neighbour is in the segment, so these are the bytes the general
                    // code reads.
                    let h = &self.hist;
                    (w, n, nw, ne) =
                        (h[pos - bpp] as i32, h[pos - s] as i32, h[pos - s - bpp] as i32, h[pos - s + bpp] as i32);
                    let r = |p: usize| self.resid_at(p) as u32;
                    (rw, rn, rnw, rne) = (r(pos - bpp), r(pos - s), r(pos - s - bpp), r(pos - s + bpp));
                } else {
                    (w, n, nw, ne) = self.neighbours_at(pos, col);
                    rw = if col >= bpp { rat(bpp) } else { 0 };
                    rn = if !first_row { rat(s) } else { 0 };
                    rnw = if !first_row && col >= bpp { rat(s + bpp) } else { 0 };
                    rne = if !first_row && col + bpp < s { rat(s - bpp) } else { 0 };
                }
                let ch = if bpp == 1 { 0 } else { (col % bpp) as u32 };
                let rp = if ch > 0 { rat(1) } else { 0 };
                let pred = loco(w, n, nw);
                self.pred = pred as u8;
                // The differences are of bytes, so the table holds every one.
                let q = |d: i32| Q[(d + 255) as usize & 511] as u32;
                let (dh, dv, dd) = (q(w - nw), q(n - nw), q(ne - n));
                [
                    h2(1, ch | dh << 4 | dv << 8),
                    h2(2, ch | dh << 4 | dv << 8 | dd << 12),
                    h2(3, ch | rw << 4),
                    h2(4, ch | rn << 4),
                    h2(5, ch | rw << 4 | rn << 12),
                    h2(6, ch | rp << 4 | (q(w - pred) << 12)),
                    h2(7, ch | ((pred as u32) >> 2) << 4),
                    h2(8, ch | q(w - pred) << 4 | q(n - pred) << 8 | q(ne - pred) << 12),
                    h2(9, ch | rnw << 4 | rne << 12),
                    h2(10, ch | rp << 4 | rw << 12),
                ]
            }
            _ => {
                let [h6, h7, h8, h9] = match self.kind {
                    Kind::Syntax => [
                        h2(17, h2(self.tok.name_hash, self.tok.since_name << 8 | c1)),
                        h2(18, h2(self.word, c1)),
                        h2(19, h2(tokh, tl)),
                        h2(20, h2(self.tok.name_hash, tokh)),
                    ],
                    Kind::Content => {
                        let idx = self.tok.operands.len() as u32;
                        let col = self.tok.prev_hashes.get(idx as usize).copied().unwrap_or(0);
                        [
                            h2(21, h2(self.tok.prev_op, idx << 16 | tokh & 0xffff) ^ tl),
                            h2(22, h2(col, tl << 8 | c1)),
                            h2(23, h2(self.tok.prev_op ^ self.tok.prev_op2.rotate_left(7), idx << 8 | c1)),
                            h2(24, h2(self.word, c1)),
                        ]
                    }
                    _ => [
                        h2(25, c4 & 0xff00_ff00),
                        h2(26, h2(self.word, c1)),
                        h2(27, (c4 & 0xffff_0000) | (self.hist.len() as u32 & 3)),
                        h2(28, c4 & 0x00ff_00ff),
                    ],
                };
                [
                    h2(11, c1),
                    h2(12, c4 & 0xffff),
                    h2(13, c4 & 0xff_ffff),
                    h2(14, c4),
                    h2(15, h2(c4, self.c8 & 0xffff)),
                    h2(16, h2(c4, self.c8)),
                    h6,
                    h7,
                    h8,
                    h9,
                ]
            }
        };
        for i in 0..NCTX {
            self.hashes[i] = k ^ hs[i];
        }
    }

    /// The number of bytes coded so far: the history's length.
    pub fn hist_len(&self) -> usize {
        self.hist.len()
    }

    /// `n` bytes of the history from `at`, or None when it doesn't hold them. Every byte coded is added to the
    /// history once, in order, and nothing else changes it, so this reads back exactly what was coded.
    pub fn history(&self, at: usize, n: usize) -> Option<&[u8]> {
        self.hist.get(at..at.checked_add(n)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Null;
    impl Coder for Null {
        fn code(&mut self, bit: Option<u32>, _p: i32) -> u32 {
            bit.unwrap_or(0)
        }
    }

    #[test]
    fn the_image_column_follows_the_bytes() {
        // Rows of noise and long repeats (coded by the one-decision shortcut), with switches of kind and row
        // length in between.
        let mut s = 77u64;
        let mut m = Model::new(12);
        let mut n = Null;
        for (stride, bpp, exact) in
            [(7usize, 1usize, false), (300, 3, false), (64, 1, true), (1, 1, false), (5, 2, false)]
        {
            m.set_kind(Kind::Syntax, Geom::default(), 0);
            for &b in b"1 0 obj\n" {
                m.code_byte(&mut n, Some(b));
            }
            m.set_kind(Kind::Image, Geom { stride, bpp, exact }, 0);
            for k in 0..6000usize {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let b = if k % 2000 < 1200 { (k % 3) as u8 } else { s as u8 };
                m.code_byte(&mut n, Some(b));
                assert_eq!(m.col, (m.hist.len() - m.seg_start) % stride, "row length {stride}, byte {k}");
            }
        }
        assert!(m.bypassed > 0);
    }

    #[test]
    fn token_hashes_follow_the_tokens() {
        // Syntax and content with long numbers, names, strings and operands.
        let mut text = Vec::new();
        text.extend_from_slice(b"1 0 obj << /Pi 3.");
        text.extend(std::iter::repeat_n(b'1', 5000));
        text.extend_from_slice(b" /Name#20x (a (nested) string) >> endobj\n");
        let mut content = Vec::new();
        for i in 0..40 {
            content.extend_from_slice(format!("{i} 0 {} Td (text) Tj ", "9".repeat(i * 50)).as_bytes());
            content.extend_from_slice(b"/F1 12 Tf [<00ff> 3 <0102>] TJ q 1 0 0 1 0 0 cm Q\n");
        }
        let mut m = Model::new(12);
        let mut n = Null;
        let fold0 = |t: &[u8]| t.iter().fold(0u32, |h, &x| hb(h, x));
        let fold7 = |t: &[u8]| t.iter().fold(7u32, |h, &x| hb(h, x));
        for (kind, data) in [(Kind::Syntax, &text), (Kind::Content, &content), (Kind::Syntax, &text)] {
            m.set_kind(kind, Geom::default(), 0);
            for &b in data.iter() {
                m.code_byte(&mut n, Some(b));
                assert_eq!(m.tok.tok_hash, fold0(&m.tok.tok));
                assert_eq!(m.tok.prev_hashes, m.tok.prev_operands.iter().map(|v| fold7(v)).collect::<Vec<u32>>());
            }
        }
    }

    /// A small deterministic generator (xorshift).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn byte(&mut self) -> u8 {
            (self.next() >> 24) as u8
        }
    }

    /// One step of a script run through the model, the same on both sides.
    enum Step {
        Kind(Kind, Geom, u64),
        Flat(bool),
        Bypass(bool),
        Forget,
        Bytes(Vec<u8>),
    }

    /// Page drawing instructions: operators with numbers, names, strings (some nested) and arrays.
    fn content(r: &mut Rng, n: usize) -> Vec<u8> {
        let mut t = String::new();
        while t.len() < n {
            let (a, b, c, d) = (r.below(612), r.below(792), r.below(100), r.below(10));
            t += &match r.below(7) {
                0 => format!("BT /F{} {} Tf {a} {b} Td (word {c}) Tj ET\n", d % 3 + 1, 8 + d),
                1 => format!("q {c} 0 0 {d} {a} {b} cm /Im{d} Do Q\n"),
                2 => format!("{a}.{d} {b} m {c} {a} l S\n"),
                3 => format!("0.{c} 0.{d} 0 rg {a} {b} {c} {d} re f\n"),
                4 => format!("[(Hel) -{c} (lo \\(x\\)) 3.5 <00ff>] TJ\n(a (nested {d}) string) Tj\n"),
                5 => format!("/GS{d} gs {d} w 1 J [{c} {d}] 0 d\n"),
                _ => format!("{a} {b} {c} {d} {a} {b} c\n"),
            };
        }
        t.into_bytes()
    }

    /// A Type 1 charstring of `groups` commands: `len_iv` random bytes, then numbers of every encoding
    /// (one byte, two bytes either sign, 32-bit) before each command, some of them escaped.
    fn charstring(r: &mut Rng, len_iv: i64, groups: u64) -> Vec<u8> {
        let mut c: Vec<u8> = (0..len_iv.max(0)).map(|_| r.byte()).collect();
        for _ in 0..groups {
            for _ in 0..r.below(5) {
                match r.below(8) {
                    0..=3 => c.push(32 + r.below(215) as u8),
                    4 => {
                        let v = r.below(1024);
                        c.extend([247 + (v >> 8) as u8, v as u8]);
                    }
                    5 => {
                        let v = r.below(1024);
                        c.extend([251 + (v >> 8) as u8, v as u8]);
                    }
                    _ => {
                        c.push(255);
                        c.extend((((r.next() as i32) >> 12) as u32).to_be_bytes());
                    }
                }
            }
            if r.below(6) == 0 {
                c.extend([12, [0, 1, 2, 6, 7, 16, 17, 33][r.below(8) as usize]]);
            } else {
                c.push([1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 14, 21, 22, 30, 31][r.below(16) as usize]);
            }
        }
        c
    }

    /// An opened Type 1 private part: text with /lenIV, then subroutines and charstrings ("n RD " or "n -| "
    /// and n bytes), one of them twice (a long repeat inside charstrings).
    fn private(r: &mut Rng, len_iv: i64) -> Vec<u8> {
        let mut t =
            b"dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n".to_vec();
        t.extend(format!("/lenIV {len_iv} def\n/BlueValues [-22 0 683 705 431 448] def\n/Subrs 4 array\n").bytes());
        for i in 0..4 {
            let cs = charstring(r, len_iv, 3 + i * 2);
            t.extend(format!("dup {i} {} RD ", cs.len()).bytes());
            t.extend(cs);
            t.extend(b" NP\n");
        }
        t.extend(b"2 index /CharStrings 12 dict dup begin\n");
        let twice = charstring(r, len_iv, 60);
        for (k, name) in ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"].iter().enumerate() {
            let groups = 8 + r.below(16);
            let cs = if k == 3 || k == 7 { twice.clone() } else { charstring(r, len_iv, groups) };
            let (rd, nd) = if k % 4 == 1 { ("-|", "|-") } else { ("RD", "ND") };
            t.extend(format!("/{name} {} {rd} ", cs.len()).bytes());
            t.extend(cs);
            t.extend(format!(" {nd}\n").bytes());
        }
        t.extend(b"end\nend\nreadonly put\nnoaccess put\ndup/FontName get exch definefont pop\n");
        t
    }

    /// Image rows of `bpp`-byte pixels: a smooth picture with a little noise, a run of rows equal to the one
    /// before them and a blank band (long repeats).
    fn picture(r: &mut Rng, stride: usize, rows: usize, bpp: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..rows {
            if y % 20 >= 8 && y % 20 < 14 {
                let at = v.len() - stride;
                v.extend_from_within(at..at + stride);
            } else if y % 20 >= 16 {
                v.extend(std::iter::repeat_n(255, stride));
            } else {
                v.extend(
                    (0..stride).map(|x| ((x / bpp.max(1)) * 3 + y * 2 + x % bpp.max(1) * 60) as u8 ^ r.below(4) as u8),
                );
            }
        }
        v
    }

    /// Symbol rows: palette indices (or packed pixels) of `bpp` bytes, with repeated rows.
    fn symbols(r: &mut Rng, stride: usize, rows: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..rows {
            if y % 12 >= 4 {
                let at = v.len() - stride;
                v.extend_from_within(at..at + stride);
            } else {
                v.extend((0..stride).map(|x| if r.below(9) == 0 { r.byte() } else { ((x / 4 + y / 3) % 4) as u8 }));
            }
        }
        v
    }

    /// A small PDF cut as the archive cuts it: syntax segments (with their offsets in the file) around content
    /// streams, numbered objects ("N 0 obj ... endobj"), a long repeat, and cross-reference tables whose entries
    /// the model expects.
    fn document(r: &mut Rng, steps: &mut Vec<Step>) {
        let mut file = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut seg = 0;
        let mut offsets = Vec::new();
        let widths: String = (0..70).map(|i| format!("{} ", 250 + (i * 37) % 500)).collect();
        for i in 1..=24 {
            offsets.push(file.len());
            if i % 3 == 0 {
                let c = content(r, 300 + 40 * i);
                file.extend(format!("{i} 0 obj\n<< /Length {} >>\nstream\n", c.len()).bytes());
                steps.push(Step::Kind(Kind::Syntax, Geom::default(), seg as u64));
                steps.push(Step::Bytes(file[seg..].to_vec()));
                steps.push(Step::Kind(Kind::Content, Geom::default(), file.len() as u64));
                steps.push(Step::Bytes(c.clone()));
                file.extend(c);
                seg = file.len();
                file.extend(b"\nendstream\nendobj\n");
            } else if i % 7 == 2 {
                file.extend(format!("{i} 0 obj\n<< /Type /Font /FirstChar 32 /Widths [{widths}] >>\nendobj\n").bytes());
            } else {
                let p = r.below(9);
                file.extend(
                    format!("{i} 0 obj\n<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Rotate {p} >>\nendobj\n", i + 1)
                        .bytes(),
                );
            }
        }
        let xref = file.len();
        file.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).bytes());
        for off in &offsets {
            file.extend(format!("{off:010} 00000 n \n").bytes());
        }
        file.extend(format!("trailer\n<< /Size 25 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").bytes());
        // An update: one more object and its own table.
        let last = file.len();
        file.extend(b"25 0 obj\n<< /Producer (test) >>\nendobj\n");
        let xref2 = file.len();
        file.extend(format!("xref\n0 1\n0000000000 65535 f \n25 1\n{last:010} 00000 n \n").bytes());
        file.extend(format!("trailer\n<< /Size 26 /Prev {xref} >>\nstartxref\n{xref2}\n%%EOF\n").bytes());
        steps.push(Step::Kind(Kind::Syntax, Geom::default(), seg as u64));
        steps.push(Step::Bytes(file[seg..].to_vec()));
    }

    /// Every kind and every path of the model: priming with the shortcut off, numbered objects and
    /// cross-reference tables (expected text), content, Type 1 private parts, residual images (1 to 3 bytes per
    /// pixel, odd row lengths, plain bytes in the middle), symbol images, rows of unknown length, other kinds,
    /// plain coding, long repeats everywhere, and many switches of kind.
    fn script() -> Vec<Step> {
        let mut r = Rng(0x9E37_79B9_7F4A_7C15);
        let mut s = Vec::new();
        let text =
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Kids [3 0 R] /Count 1 >>\nendobj\n";
        s.push(Step::Bypass(false));
        for _ in 0..2 {
            s.push(Step::Kind(Kind::Syntax, Geom::default(), 0));
            s.push(Step::Bytes(text.to_vec()));
            s.push(Step::Kind(Kind::Content, Geom::default(), 0));
            s.push(Step::Bytes(b"BT /F1 12 Tf 72 712 Td (Hello) Tj ET\nq 1 0 0 1 0 0 cm Q\n".to_vec()));
        }
        s.push(Step::Forget);
        s.push(Step::Bypass(true));
        document(&mut r, &mut s);

        s.push(Step::Kind(Kind::Font, Geom::default(), 0));
        s.push(Step::Bytes(
            b"%!PS-AdobeFont-1.0: CMR10 003.002\n/FontName /CMR10 def\n/PaintType 0 def\neexec\n".to_vec(),
        ));
        s.push(Step::Kind(Kind::Charstring, Geom::default(), 0));
        s.push(Step::Bytes(private(&mut r, 4)));
        s.push(Step::Kind(Kind::Font, Geom::default(), 0));
        s.push(Step::Bytes(b"cleartomark\n".to_vec()));

        for (stride, rows, bpp) in [(37, 50, 1), (87, 30, 3), (5, 60, 2), (1, 300, 1), (7, 40, 0)] {
            s.push(Step::Kind(Kind::Image, Geom { stride, bpp, exact: false }, 0));
            s.push(Step::Bytes(picture(&mut r, stride, rows, bpp)));
        }
        // Plain bytes inside a residual image, then the full model again.
        s.push(Step::Kind(Kind::Image, Geom { stride: 37, bpp: 1, exact: false }, 0));
        s.push(Step::Bytes(picture(&mut r, 37, 12, 1)));
        s.push(Step::Flat(true));
        let noise: Vec<u8> = (0..150).map(|_| r.byte()).collect();
        s.push(Step::Bytes([noise.clone(), noise].concat()));
        s.push(Step::Flat(false));
        s.push(Step::Bytes(picture(&mut r, 37, 20, 1)));
        for (stride, rows, bpp) in [(33, 30, 1), (22, 40, 2), (9, 20, 0)] {
            s.push(Step::Kind(Kind::Image, Geom { stride, bpp, exact: true }, 0));
            s.push(Step::Bytes(symbols(&mut r, stride, rows)));
        }
        s.push(Step::Kind(Kind::Image, Geom { stride: 0, bpp: 1, exact: false }, 0));
        s.push(Step::Bytes(picture(&mut r, 50, 12, 1)));

        s.push(Step::Kind(Kind::Meta, Geom::default(), 0));
        s.push(Step::Bytes((0..120).map(|i| if i % 5 == 0 { 0x80 | (i as u8) } else { r.below(8) as u8 }).collect()));
        s.push(Step::Kind(Kind::Data, Geom::default(), 0));
        s.push(Step::Bytes((0..400).map(|i| if i % 41 == 0 { r.below(5) as u8 } else { (i * 7) as u8 }).collect()));
        // Raw bytes, then a copy of the content of the document: a long repeat of earlier data.
        let raw: Vec<u8> = (0..600).map(|i| (i as u8).wrapping_mul(13) ^ (r.below(3) as u8)).collect();
        s.push(Step::Kind(Kind::Raw, Geom::default(), 9000));
        s.push(Step::Bytes([raw, content(&mut Rng(5), 600), content(&mut Rng(5), 600)].concat()));
        // Plain coding, with a repeat inside it (the shortcut works there too).
        s.push(Step::Kind(Kind::Raw, Geom::default(), 0));
        s.push(Step::Flat(true));
        let noise: Vec<u8> = (0..700).map(|_| r.byte()).collect();
        s.push(Step::Bytes([&noise[..], &noise[..300], &noise[200..]].concat()));
        s.push(Step::Flat(false));
        // The shortcut off for a while: long repeats go through the full model.
        s.push(Step::Bypass(false));
        s.push(Step::Kind(Kind::Data, Geom::default(), 0));
        let block = content(&mut r, 250);
        s.push(Step::Bytes([&block[..], &block[..]].concat()));
        s.push(Step::Bypass(true));
        s.push(Step::Forget);
        s.push(Step::Kind(Kind::Charstring, Geom::default(), 0));
        s.push(Step::Bytes(private(&mut r, -1)));

        // Many switches of kind, with a few bytes of each.
        let kinds = [
            (Kind::Meta, Geom::default()),
            (Kind::Syntax, Geom::default()),
            (Kind::Content, Geom::default()),
            (Kind::Image, Geom { stride: 9, bpp: 1, exact: false }),
            (Kind::Font, Geom::default()),
            (Kind::Data, Geom::default()),
            (Kind::Raw, Geom::default()),
            (Kind::Charstring, Geom::default()),
            (Kind::Image, Geom { stride: 6, bpp: 2, exact: true }),
            (Kind::Image, Geom { stride: 0, bpp: 3, exact: false }),
        ];
        for i in 0..60 {
            let (kind, geom) = kinds[r.below(kinds.len() as u64) as usize];
            s.push(Step::Kind(kind, geom, i * 100));
            let n = 5 + r.below(50) as usize;
            let bytes = if i % 2 == 0 { content(&mut r, n) } else { (0..n).map(|_| r.byte()).collect() };
            s.push(Step::Bytes(bytes));
        }
        s.push(Step::Kind(Kind::Syntax, Geom::default(), 0));
        s.push(Step::Bytes(text.to_vec()));
        s
    }

    /// Runs the script through the model with coder `c` (the bytes given when encoding) and returns the bytes
    /// the model gave back.
    fn run_script<C: Coder>(steps: &[Step], c: &mut C, encode: bool) -> (Vec<u8>, u64) {
        let mut m = Model::new(12);
        let mut got = Vec::new();
        for st in steps {
            match st {
                Step::Kind(kind, geom, orig) => m.set_kind(*kind, *geom, *orig),
                Step::Flat(f) => m.set_flat(*f),
                Step::Bypass(b) => m.bypass_on = *b,
                Step::Forget => m.forget_document(),
                Step::Bytes(v) => {
                    for &b in v {
                        got.push(m.code_byte(c, encode.then_some(b)));
                    }
                }
            }
        }
        (got, m.bypassed)
    }

    #[test]
    fn coded_bytes_stay_the_same_for_every_kind() {
        // The model is part of the archive format: this pins its output. A change that alters these numbers
        // changes the format.
        let steps = script();
        let input: Vec<u8> =
            steps.iter().flat_map(|s| if let Step::Bytes(v) = s { v.clone() } else { Vec::new() }).collect();
        let mut e = Encoder::new();
        let (back, bypassed) = run_script(&steps, &mut e, true);
        assert_eq!(back, input);
        assert!(bypassed > 1000, "long repeats: {bypassed}");
        let out = e.finish();
        let fnv = out.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
        assert_eq!((input.len(), out.len(), fnv), (30_806, 10_764, 0x3720_ab6b_a31b_18ca));
        let mut d = Decoder::new(&out);
        let (decoded, _) = run_script(&steps, &mut d, false);
        assert!(decoded == input);
    }

    /// `run_script` with the bytes coded by `code_bytes`, in runs of 1 to 5000 bytes.
    fn run_script_in_runs<C: Coder>(steps: &[Step], c: &mut C, encode: bool) -> Vec<u8> {
        let mut m = Model::new(12);
        let mut got = Vec::new();
        let mut run = 0;
        for st in steps {
            match st {
                Step::Kind(kind, geom, orig) => m.set_kind(*kind, *geom, *orig),
                Step::Flat(f) => m.set_flat(*f),
                Step::Bypass(b) => m.bypass_on = *b,
                Step::Forget => m.forget_document(),
                Step::Bytes(v) => {
                    let mut i = 0;
                    while i < v.len() {
                        run = (run + 1) % 5;
                        let n = (v.len() - i).min([1, 3, 17, 200, 5000][run]);
                        m.code_bytes(c, encode.then(|| &v[i..i + n]), n, |b| got.push(b));
                        i += n;
                    }
                }
            }
        }
        got
    }

    #[test]
    fn code_bytes_codes_as_code_byte_does() {
        let steps = script();
        let input: Vec<u8> =
            steps.iter().flat_map(|s| if let Step::Bytes(v) = s { v.clone() } else { Vec::new() }).collect();
        let mut e = Encoder::new();
        assert!(run_script_in_runs(&steps, &mut e, true) == input);
        let out = e.finish();
        let mut one = Encoder::new();
        run_script(&steps, &mut one, true);
        assert!(out == one.finish());
        let mut d = Decoder::new(&out);
        assert!(run_script_in_runs(&steps, &mut d, false) == input);
    }

    #[test]
    fn match_hash_is_the_hash_of_the_last_five_bytes() {
        // Text, image rows, long repeats and plain bytes.
        let mut r = Rng(3);
        let mut m = Model::new(12);
        let mut n = Null;
        for k in 0..6000usize {
            if k % 1000 == 0 {
                let kind = [Kind::Syntax, Kind::Image, Kind::Raw, Kind::Data, Kind::Content, Kind::Image][k / 1000];
                m.set_kind(kind, Geom { stride: 13, bpp: 1 + k / 5000, exact: false }, 0);
            }
            m.set_flat(k % 1000 >= 900);
            let b = if k % 300 < 150 { b"0 0 obj\n"[k % 8] } else { r.byte() };
            m.code_byte(&mut n, Some(b));
            let h = &m.hist;
            if h.len() >= MATCH_MIN {
                assert_eq!(m.match_hash(), h[h.len() - MATCH_MIN..].iter().fold(0, |h, &x| hb(h, x)), "byte {k}");
            }
        }
        assert!(m.bypassed > 0);
    }

    #[test]
    fn difference_classes_are_the_ones_worked_out_each_time() {
        for d in -255i32..=255 {
            let m = match d.unsigned_abs() {
                0 => 0,
                1 => 1,
                2 => 2,
                3..=4 => 3,
                5..=8 => 4,
                9..=16 => 5,
                17..=32 => 6,
                _ => 7,
            };
            assert_eq!(Q[(d + 255) as usize], if d < 0 { m + 8 } else { m }, "{d}");
        }
    }

    /// The prediction of the image byte at `p` as the first version of the model made it, byte by byte.
    fn eager_pred(h: &[u8], lo: usize, s: usize, bpp: usize, p: usize) -> u8 {
        let at = |back: usize| -> i32 {
            if p >= back && p - back >= lo {
                h[p - back] as i32
            } else {
                0
            }
        };
        let col = (p - lo) % s;
        let first_row = p - lo < s;
        let w = if col >= bpp {
            at(bpp)
        } else if !first_row {
            at(s)
        } else {
            0
        };
        let n = if !first_row { at(s) } else { w };
        let nw = if !first_row && col >= bpp { at(s + bpp) } else { n };
        let pred = if nw >= w.max(n) {
            w.min(n)
        } else if nw <= w.min(n) {
            w.max(n)
        } else {
            w + n - nw
        };
        pred as u8
    }

    #[test]
    fn stored_residuals_are_the_ones_made_byte_by_byte() {
        // Rows of noise, long repeats and plain bytes, at several row lengths and pixel sizes: whenever the
        // contexts are up to date, every residual they may read is the byte less its prediction.
        let mut r = Rng(11);
        let mut m = Model::new(12);
        let mut n = Null;
        for (stride, bpp) in [(7usize, 1usize), (300, 3), (1, 1), (5, 2), (31, 3), (2, 3), (64, 4)] {
            m.set_kind(Kind::Image, Geom { stride, bpp, exact: false }, 0);
            for k in 0..5000usize {
                m.set_flat(k % 1500 >= 1400);
                let b = if k % 1000 < 600 { (k % 3) as u8 } else { r.byte() };
                m.code_byte(&mut n, Some(b));
                if !m.ctx_dirty {
                    let (pos, lo) = (m.hist.len(), m.seg_start);
                    for p in pos.saturating_sub(stride + bpp).max(lo)..pos {
                        let want = m.hist[p].wrapping_sub(eager_pred(&m.hist, lo, stride, bpp, p));
                        assert_eq!(m.resid_at(p), want, "row length {stride}, {bpp} bytes per pixel, byte {k}");
                    }
                }
            }
        }
        assert!(m.bypassed > 0);
    }

    #[test]
    fn residuals_read_through_the_ring_are_the_eager_ones() {
        // Row lengths just below, at and just above a power of two, alone and with the pixel added, at 1 to 4
        // bytes per pixel. Each segment is many rings long, with noise, long repeats and plain bytes.
        let mut r = Rng(29);
        let mut m = Model::new(12);
        let mut n = Null;
        for bpp in 1..=4usize {
            for stride in [31, 32, 33, 32 - bpp, 33 - bpp, 255, 256, 257, 256 - bpp, 257 - bpp] {
                m.set_kind(Kind::Image, Geom { stride, bpp, exact: false }, 0);
                let ring = m.rmask + 1;
                assert!(ring.is_power_of_two() && ring >= stride + bpp && ring / 2 < stride + bpp);
                for k in 0..3000usize {
                    m.set_flat(k % 1100 >= 1000);
                    let b = if k % 700 < 400 { (k % 5) as u8 } else { r.byte() };
                    m.code_byte(&mut n, Some(b));
                    let (pos, lo) = (m.hist.len(), m.seg_start);
                    assert!(m.rhist.len() <= ring.min(pos - lo));
                    if !m.ctx_dirty {
                        for p in pos.saturating_sub(stride + bpp).max(lo)..pos {
                            let want = m.hist[p].wrapping_sub(eager_pred(&m.hist, lo, stride, bpp, p));
                            assert_eq!(m.resid_at(p), want, "row length {stride}, {bpp} bytes per pixel, byte {k}");
                        }
                    }
                }
            }
        }
        assert!(m.bypassed > 0);
    }

    #[test]
    fn a_row_length_at_the_top_of_usize_reads_the_eager_residuals() {
        // `pack` caps the row length at 2^30, but the model takes any: the gap a fill skips is a row and a pixel
        // back, and that sum must not wrap, or the residuals of long repeats and plain bytes are never stored.
        let mut r = Rng(5);
        let mut m = Model::new(12);
        let mut n = Null;
        for (stride, bpp) in [(usize::MAX - 15, 64usize), (usize::MAX, 1)] {
            m.set_kind(Kind::Image, Geom { stride, bpp, exact: false }, 0);
            for k in 0..3000usize {
                m.set_flat(k % 1100 >= 1000);
                let b = if k % 700 < 400 { (k % 5) as u8 } else { r.byte() };
                m.code_byte(&mut n, Some(b));
                if k % 50 == 49 && !m.ctx_dirty {
                    // Every byte is in the first row, so the contexts may read back to the segment's start.
                    let (pos, lo) = (m.hist.len(), m.seg_start);
                    for p in lo..pos {
                        let want = m.hist[p].wrapping_sub(eager_pred(&m.hist, lo, stride, bpp, p));
                        assert_eq!(m.resid_at(p), want, "row length {stride}, {bpp} bytes per pixel, byte {k}");
                    }
                }
            }
        }
        assert!(m.bypassed > 0);
    }

    #[test]
    fn a_huge_row_length_costs_only_the_bytes_that_come() {
        let mut m = Model::new(12);
        let mut n = Null;
        let mut r = Rng(29);
        m.set_kind(Kind::Image, Geom { stride: 1 << 30, bpp: 64, exact: false }, 0);
        assert_eq!(m.rmask, (1 << 31) - 1);
        // Long repeats, whose residuals are filled in when the full model comes back, and noise.
        for k in 0..3000usize {
            m.code_byte(&mut n, Some(if k % 700 < 400 { (k % 5) as u8 } else { r.byte() }));
        }
        assert!(m.bypassed > 0 && m.rhist.len() > 2000 && m.rhist.capacity() <= 2 * 3000);
        // The ring's room goes with its segment; other kinds store no residuals.
        m.set_kind(Kind::Data, Geom::default(), 0);
        assert_eq!(m.rhist.capacity(), 0);
        for k in 0..3000usize {
            m.code_byte(&mut n, Some((k % 5) as u8));
        }
        assert_eq!(m.rhist.capacity(), 0);
        // A segment that can use all of a ring's room keeps it.
        let geom = Geom { stride: 100, bpp: 3, exact: false };
        m.set_kind(Kind::Image, geom, 0);
        for _ in 0..1000 {
            m.code_byte(&mut n, Some(r.byte()));
        }
        let room = m.rhist.capacity();
        m.set_kind(Kind::Image, geom, 0);
        assert!(room == 128 && m.rhist.is_empty() && m.rhist.capacity() == room);
    }
}
