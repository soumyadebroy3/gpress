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
                ((a + 1).min(7), if b > 2 { (b + 1) / 2 } else { b })
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
    #[inline]
    fn find(&mut self, h: u32) -> usize {
        let i = (h as usize) & self.mask;
        let chk = ((h >> 24) as u8) | 1;
        let b = &mut self.t[i];
        if b[0] != chk {
            *b = [0u8; 16];
            b[0] = chk;
        }
        i
    }
}

/// Tokenizer state over PDF syntax and content streams.
#[derive(Default, Clone)]
struct Tok {
    in_tok: bool,
    tok: Vec<u8>,

    name_hash: u32,
    since_name: u32,
    in_string: u32,
    // content streams
    operands: Vec<Vec<u8>>,
    prev_operands: Vec<Vec<u8>>,
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
        5 | 6 | 7 => 2,
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
    tb: Tables,
    pub hist: Vec<u8>,
    /// For images: the coded residual of each byte (actual minus prediction); 0 elsewhere.
    rhist: Vec<u8>,
    /// Prediction subtracted from the current byte before coding (images only).
    pred: u8,
    kind: Kind,
    geom: Geom,
    seg_start: usize,
    /// Original-file offset of the current syntax segment's first byte.
    seg_orig: u64,
    c0: u32,
    nib: u32,
    bitpos: u32,
    c4: u32,
    c8: u32,
    word: u32,
    hashes: [u32; NCTX],
    tables: Vec<Table>,
    slot: [usize; NCTX],
    states: States,
    smaps: Vec<Vec<Counter>>,
    // long matches
    mm_table: Vec<u32>,
    mm_ptr: usize,
    mm_len: usize,
    mm_sm: StateMap,
    mm_miss: bool,
    // expected text
    exp: Vec<u8>,
    exp_i: usize,
    exp_hits: u32,
    exp_sm: StateMap,
    exp_miss: bool,
    mx: Mixer,
    a1: Apm,
    a2: Apm,
    pr: i32,
    tok: Tok,
    cs: Cs,
    /// Original offsets of objects seen ("N G obj"), for predicting cross-reference tables.
    obj_offsets: std::collections::HashMap<i64, (u64, i64)>,
    limit: u32,
    mm_min: usize,
    // Long repeats: one almost-certain decision ("same byte as the match") instead of eight.
    rep: Vec<Counter>,
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

// Fixed settings of the format. They are part of it: the decoder must use exactly the same values as the
// encoder, so they are constants (never read from the environment).
const STATE_LIMIT: u32 = 1023;
const MATCH_MIN: usize = 5;
const MIXER_SHIFT: i32 = 12;
/// A match at least this long codes the next byte with one decision (measured: 32, 64, 128 and 256; 128 is
/// the smallest on a random sample and on huge repetitive images).
const LONG_MIN: usize = 128;
const REP_LIMIT: u32 = 1023;
/// Mixer weight sets for charstring bytes: at a token start 7 operand counts x 6 kinds of last command, plus 3
/// other places (end of a number, escaped command, other), each x 8 bits x 3 match states.
const CS_SEL: usize = 45 * 8 * 3;

impl Model {
    pub fn new(table_bits: u32) -> Model {
        let mut tables = Vec::with_capacity(NCTX);
        for _ in 0..NCTX {
            tables.push(Table::new(table_bits));
        }
        let mut m = Model {
            tb: Tables::new(),
            hist: Vec::new(),
            rhist: Vec::new(),
            pred: 0,
            kind: Kind::Meta,
            geom: Geom::default(),
            seg_start: 0,
            seg_orig: 0,
            c0: 1,
            nib: 1,
            bitpos: 0,
            c4: 0,
            c8: 0,
            word: 0,
            hashes: [0; NCTX],
            tables,
            slot: [0; NCTX],
            states: States::new(),
            smaps: Vec::new(),
            mm_table: vec![0; 1 << 20],
            mm_ptr: 0,
            mm_len: 0,
            mm_sm: StateMap::new(64 * 2, 1023),
            mm_miss: false,
            exp: Vec::new(),
            exp_i: 0,
            exp_hits: 0,
            exp_sm: StateMap::new(16 * 2 * 4, 1023),
            exp_miss: false,
            mx: Mixer::new(NIN, KINDS * 8 * 3 + CS_SEL, 6),
            a1: Apm::new(256 * KINDS, 7),
            a2: Apm::new(65536, 7),
            pr: 2048,
            tok: Tok::default(),
            cs: Cs::new(),
            obj_offsets: std::collections::HashMap::new(),
            limit: STATE_LIMIT,
            mm_min: MATCH_MIN,
            rep: vec![Counter::new(); KINDS * 16 * 2 * 2],
            rep_last: 1,
            ctx_dirty: false,
            bypass_on: true,
            flat: false,
            bypassed: 0,
        };
        m.mx.shift = MIXER_SHIFT;
        m.smaps = (0..NCTX * KINDS)
            .map(|_| (0..128).map(|st| Counter(((m.states.init_p[st] as u32) << 20) | 0)).collect())
            .collect();
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
        self.geom = geom;
        self.seg_start = self.hist.len();
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

    pub fn code_byte<C: Coder>(&mut self, c: &mut C, byte: Option<u8>) -> u8 {
        // A long repeat: one almost-certain decision for the whole byte.
        if self.bypass_on && self.mm_len >= LONG_MIN && !self.mm_miss {
            let pb = self.hist[self.mm_ptr];
            let n = self.hist.len();
            let run = (n > 0 && self.hist[n - 1] == pb) as usize;
            let lb = ((usize::BITS - self.mm_len.leading_zeros()) as usize).min(15);
            let cx = ((self.kind as usize * 16 + lb) * 2 + self.rep_last as usize) * 2 + run;
            let p = self.rep[cx].p16();
            let hit = c.code16(byte.map(|b| (b == pb) as u32), p);
            self.rep[cx].update(hit, REP_LIMIT);
            self.rep_last = hit;
            if hit == 1 {
                self.bypassed += 1;
                self.skip_byte(pb);
                return pb;
            }
            // Not the matched byte: the match model has nothing to say about this one.
            self.mm_miss = true;
        }
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
        let sym = byte.map(|b| b.wrapping_sub(self.pred));
        for k in (0..8).rev() {
            let bit = sym.map(|b| ((b >> k) & 1) as u32);
            let p = self.predict();
            let got = c.code(bit, p);
            self.update(got);
        }
        *self.hist.last().unwrap()
    }

    fn predict(&mut self) -> i32 {
        let node = self.nib as usize;
        let kb = self.kind as usize * NCTX;
        for i in 0..NCTX {
            let state = self.tables[i].t[self.slot[i]][node] as usize;
            let p = self.smaps[kb + i][state].p12();
            self.mx.add(if state == 0 { 0 } else { self.tb.stretch(p) });
        }
        // Long match.
        let mut mm_state = 0;
        let residual = self.pred_on();
        if self.mm_len > 0 && !self.mm_miss {
            // For residual-coded images the match predicts the residual the matched byte would give.
            let pb = self.hist[self.mm_ptr].wrapping_sub(self.pred);
            let expect = ((pb as u32 | 256) >> (7 - self.bitpos)) & 1;
            let ctx = self.mm_len.min(63) * 2 + expect as usize;
            let p = self.mm_sm.p(ctx);
            self.mx.add(self.tb.stretch(p));
            mm_state = 1 + (self.mm_len >= 24) as usize;
        } else {
            self.mx.add(0);
        }
        // Expected text.
        if self.exp_i < self.exp.len() && !self.exp_miss && !residual {
            let eb = self.exp[self.exp_i];
            let expect = ((eb as u32 | 256) >> (7 - self.bitpos)) & 1;
            let ctx = ((self.exp_hits.min(15) as usize * 2 + expect as usize) * 4) + (self.kind as usize % 4);
            let p = self.exp_sm.p(ctx);
            self.mx.add(self.tb.stretch(p));
        } else {
            self.mx.add(0);
        }
        self.mx.add(256);
        let sel = if self.kind == Kind::Charstring && self.cs.left > 0 {
            let at = match self.cs.phase {
                CS_TOKEN => (self.cs.depth as usize).min(6) * 6 + cmd_class(self.cs.c1),
                CS_POS2 | CS_NEG2 => 42,
                CS_ESC => 43,
                _ => 44,
            };
            KINDS * 8 * 3 + (at * 8 + self.bitpos as usize) * 3 + mm_state
        } else {
            (self.kind as usize * 8 + self.bitpos as usize) * 3 + mm_state
        };
        let pm = self.mx.mix(sel);
        let c1 = (self.c4 & 0xff) as usize;
        let p1 = self.a1.p(pm, self.c0 as usize + 256 * self.kind as usize, &self.tb);
        let p2 = self.a2.p(pm, (self.c0 as usize | c1 << 8) & 0xffff, &self.tb);
        self.pr = ((pm + p1 + 2 * p2 + 2) >> 2).clamp(1, 4095);
        self.pr
    }

    fn update(&mut self, bit: u32) {
        let node = self.nib as usize;
        let kb = self.kind as usize * NCTX;
        for i in 0..NCTX {
            let v = &mut self.tables[i].t[self.slot[i]][node];
            let state = *v as usize;
            self.smaps[kb + i][state].update(bit, self.limit);
            *v = self.states.next[state][bit as usize];
        }
        let residual = self.pred_on();
        if self.mm_len > 0 && !self.mm_miss {
            self.mm_sm.update(bit);
            let pb = self.hist[self.mm_ptr].wrapping_sub(self.pred);
            let expect = ((pb as u32 | 256) >> (7 - self.bitpos)) & 1;
            if expect != bit {
                self.mm_miss = true;
            }
        }
        if self.exp_i < self.exp.len() && !self.exp_miss && !residual {
            self.exp_sm.update(bit);
            let eb = self.exp[self.exp_i];
            let expect = ((eb as u32 | 256) >> (7 - self.bitpos)) & 1;
            if expect != bit {
                self.exp_miss = true;
            }
        }
        self.mx.update(bit);
        self.a1.update(bit);
        self.a2.update(bit);
        self.c0 = (self.c0 << 1) | bit;
        self.nib = (self.nib << 1) | bit;
        self.bitpos += 1;
        if self.bitpos == 4 {
            self.nib = 1;
            for i in 0..NCTX {
                let h = h2(self.hashes[i], self.c0);
                self.slot[i] = self.tables[i].find(h);
            }
        } else if self.bitpos == 8 {
            let r = (self.c0 & 0xff) as u8;
            let b = r.wrapping_add(self.pred);
            self.rhist.push(if self.pred_on() { r } else { 0 });
            self.byte_done(b);
            self.c0 = 1;
            self.nib = 1;
            self.bitpos = 0;
            self.contexts();
        }
    }

    fn byte_done(&mut self, b: u8) {
        self.hist.push(b);
        self.c8 = (self.c8 << 8) | (self.c4 >> 24);
        self.c4 = (self.c4 << 8) | b as u32;
        if b.is_ascii_alphanumeric() {
            self.word = hb(self.word, b.to_ascii_lowercase());
        } else {
            self.word = 0;
        }
        // Long match bookkeeping.
        let n = self.hist.len();
        if self.mm_len > 0 {
            if !self.mm_miss && self.hist[self.mm_ptr] == b {
                self.mm_len += 1;
                self.mm_ptr += 1;
            } else {
                self.mm_len = 0;
            }
        }
        self.mm_miss = false;
        if n >= self.mm_min {
            let mut h = 0u32;
            for &x in &self.hist[n - self.mm_min..] {
                h = hb(h, x);
            }
            let idx = (h >> 12) as usize;
            if self.mm_len == 0 {
                let cand = self.mm_table[idx] as usize;
                if cand > 0 && cand < n {
                    let mut l = 0;
                    while l < 64 && cand > l && self.hist[cand - 1 - l] == self.hist[n - 1 - l] {
                        l += 1;
                    }
                    if l >= self.mm_min {
                        self.mm_len = l;
                        self.mm_ptr = cand;
                    }
                }
            }
            self.mm_table[idx] = n as u32;
        }
        // Expected text bookkeeping.
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
        match self.kind {
            Kind::Syntax => self.syntax_byte(b),
            Kind::Content => self.content_byte(b),
            Kind::Charstring => self.charstring_byte(b),
            _ => {}
        }
    }

    /// Follows the text of a Type 1 private part to its charstrings, and the tokens inside them.
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

    /// A byte known without the full model (a long repeat, or plain coding): history, matches and the
    /// syntax trackers follow it; the hash contexts are refreshed only when the full model is needed again.
    fn skip_byte(&mut self, b: u8) {
        let image = self.pred_on();
        self.rhist.push(if image { b.wrapping_sub(self.pred) } else { 0 });
        self.byte_done(b);
        self.pred = if image { self.image_neighbours().4 as u8 } else { 0 };
        self.ctx_dirty = true;
    }

    /// For images with known rows: the left, upper, upper-left and upper-right samples of the next byte,
    /// and LOCO-I's median edge prediction from them.
    fn image_neighbours(&self) -> (i32, i32, i32, i32, i32) {
        let pos = self.hist.len();
        let s = self.geom.stride;
        let bpp = self.geom.bpp.max(1);
        let lo = self.seg_start;
        let at = |back: usize| -> i32 {
            if pos >= back && pos - back >= lo {
                self.hist[pos - back] as i32
            } else {
                0
            }
        };
        let col = (pos - lo) % s;
        let first_row = pos - lo < s;
        let w = if col >= bpp { at(bpp) } else if !first_row { at(s) } else { 0 };
        let n = if !first_row { at(s) } else { w };
        let nw = if !first_row && col >= bpp { at(s + bpp) } else { n };
        let ne = if !first_row && col + bpp < s { at(s - bpp) } else { n };
        let pred = if nw >= w.max(n) {
            w.min(n)
        } else if nw <= w.min(n) {
            w.max(n)
        } else {
            w + n - nw
        };
        (w, n, nw, ne, pred)
    }

    /// For symbol images: the channel and the exact left, upper, upper-left, upper-right, second-left and
    /// second-upper bytes of the next byte (0 outside the image).
    fn exact_neighbours(&self) -> [u32; 7] {
        let pos = self.hist.len();
        let s = self.geom.stride;
        let bpp = self.geom.bpp.max(1);
        let lo = self.seg_start;
        let at = |back: usize| -> u32 {
            if pos >= back && pos - back >= lo {
                self.hist[pos - back] as u32
            } else {
                0
            }
        };
        let col = (pos - lo) % s;
        let up = pos - lo >= s;
        let w = if col >= bpp { at(bpp) } else { 0 };
        let ww = if col >= 2 * bpp { at(2 * bpp) } else { 0 };
        let n = if up { at(s) } else { 0 };
        let nw = if up && col >= bpp { at(s + bpp) } else { 0 };
        let ne = if up && col + bpp < s { at(s - bpp) } else { 0 };
        let nn = if pos - lo >= 2 * s { at(2 * s) } else { 0 };
        [(col % bpp) as u32, w, n, nw, ne, ww, nn]
    }

    fn token_end(&mut self) -> Option<Vec<u8>> {
        if self.tok.in_tok {
            self.tok.in_tok = false;
            let t = std::mem::take(&mut self.tok.tok);
            Some(t)
        } else {
            None
        }
    }

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
                self.tok.tok.push(b);
            } else if b == b'(' {
                self.tok.in_string = 1;
            }
            if b == b'\n' {
                self.syntax_newline();
            }
        } else {
            self.tok.in_tok = true;
            self.tok.tok.push(b);
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
                }
            }
            if b == b'/' {
                self.tok.in_tok = true;
                self.tok.tok.push(b);
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
            self.tok.tok.push(b);
        }
    }

    /// Whether the current byte is coded as a residual (images with known rows).
    #[inline]
    fn pred_on(&self) -> bool {
        self.kind == Kind::Image && self.geom.stride > 0 && !self.geom.exact
    }

    fn contexts(&mut self) {
        self.ctx_dirty = false;
        self.pred = 0;
        let k = (self.kind as u32 + 1) << 28;
        let c4 = self.c4;
        let c1 = c4 & 0xff;
        let tokh = self.tok.tok.iter().fold(0u32, |h, &x| hb(h, x));
        let tl = self.tok.tok.len() as u32;
        let nb = if self.kind == Kind::Image && self.geom.stride > 0 { self.image_neighbours() } else { (0, 0, 0, 0, 0) };
        let ex = if self.kind == Kind::Image && self.geom.stride > 0 && self.geom.exact { self.exact_neighbours() } else { [0; 7] };
        let hs = &mut self.hashes;
        match self.kind {
            Kind::Image if self.geom.stride > 0 && self.geom.exact => {
                let [ch, w, n, nw, ne, ww, nn] = ex;
                let wn = ch | w << 8 | n << 16;
                hs[0] = k ^ h2(31, ch | w << 8);
                hs[1] = k ^ h2(32, ch | n << 8);
                hs[2] = k ^ h2(33, wn);
                hs[3] = k ^ h2(34, h2(wn, nw));
                hs[4] = k ^ h2(35, ch | n << 8 | ne << 16);
                hs[5] = k ^ h2(36, h2(wn, ne));
                hs[6] = k ^ h2(37, ch | w << 8 | ww << 16);
                hs[7] = k ^ h2(38, ch | n << 8 | nn << 16);
                hs[8] = k ^ h2(39, h2(wn, nw | ne << 8));
                hs[9] = k ^ h2(40, h2(c4 & 0xffff, n));
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
                let same = if cs.depth < cs.nlast.min(8) { cs.last[cs.depth as usize].clamp(-4000, 4000) as u32 & 0xffff } else { 0x10000 };
                hs[0] = k ^ h2(41, st);
                hs[1] = k ^ h2(42, h2(st, cs.c2));
                hs[2] = k ^ h2(43, ph);
                hs[3] = k ^ h2(44, h2(st | lead << 24, num_class(cs.n1)));
                hs[4] = k ^ h2(45, ph | lead << 8 | depth << 16);
                hs[5] = k ^ h2(46, h2(cs.name, st | lead << 24));
                hs[6] = k ^ h2(47, ph | c1 << 8 | depth << 16);
                hs[7] = k ^ h2(48, h2(st | lead << 24, num_class(cs.n1) | num_class(cs.n2) << 8));
                hs[8] = k ^ h2(49, h2(st | lead << 24, same));
                hs[9] = k ^ h2(50, h2(c4, ph));
            }
            Kind::Image if self.geom.stride > 0 => {
                let pos = self.hist.len();
                let s = self.geom.stride;
                let bpp = self.geom.bpp.max(1);
                let lo = self.seg_start;
                let rat = |back: usize| -> u32 {
                    if pos >= back && pos - back >= lo {
                        self.rhist[pos - back] as u32
                    } else {
                        0
                    }
                };
                let col = (pos - lo) % s;
                let first_row = pos - lo < s;
                let (w, n, nw, ne, pred) = nb;
                self.pred = pred as u8;
                let q = |d: i32| -> u32 {
                    let a = d.unsigned_abs();
                    let m = match a {
                        0 => 0,
                        1 => 1,
                        2 => 2,
                        3..=4 => 3,
                        5..=8 => 4,
                        9..=16 => 5,
                        17..=32 => 6,
                        _ => 7,
                    };
                    if d < 0 { m + 8 } else { m }
                };
                let ch = (col % bpp) as u32;
                let rw = if col >= bpp { rat(bpp) } else { 0 };
                let rn = if !first_row { rat(s) } else { 0 };
                let rnw = if !first_row && col >= bpp { rat(s + bpp) } else { 0 };
                let rne = if !first_row && col + bpp < s { rat(s - bpp) } else { 0 };
                let rp = if bpp > 1 && ch > 0 { rat(1) } else { 0 };
                let (dh, dv, dd) = (q(w - nw), q(n - nw), q(ne - n));
                hs[0] = k ^ h2(1, ch | dh << 4 | dv << 8);
                hs[1] = k ^ h2(2, ch | dh << 4 | dv << 8 | dd << 12);
                hs[2] = k ^ h2(3, ch | rw << 4);
                hs[3] = k ^ h2(4, ch | rn << 4);
                hs[4] = k ^ h2(5, ch | rw << 4 | rn << 12);
                hs[5] = k ^ h2(6, ch | rp << 4 | (q(w - pred) << 12));
                hs[6] = k ^ h2(7, ch | ((pred as u32) >> 2) << 4);
                hs[7] = k ^ h2(8, ch | q(w - pred) << 4 | q(n - pred) << 8 | q(ne - pred) << 12);
                hs[8] = k ^ h2(9, ch | rnw << 4 | rne << 12);
                hs[9] = k ^ h2(10, ch | rp << 4 | rw << 12);
            }
            _ => {
                hs[0] = k ^ h2(11, c1);
                hs[1] = k ^ h2(12, c4 & 0xffff);
                hs[2] = k ^ h2(13, c4 & 0xff_ffff);
                hs[3] = k ^ h2(14, c4);
                hs[4] = k ^ h2(15, h2(c4, self.c8 & 0xffff));
                hs[5] = k ^ h2(16, h2(c4, self.c8));
                match self.kind {
                    Kind::Syntax => {
                        hs[6] = k ^ h2(17, h2(self.tok.name_hash, self.tok.since_name << 8 | c1));
                        hs[7] = k ^ h2(18, h2(self.word, c1));
                        hs[8] = k ^ h2(19, h2(tokh, tl));
                        hs[9] = k ^ h2(20, h2(self.tok.name_hash, tokh));
                    }
                    Kind::Content => {
                        let idx = self.tok.operands.len() as u32;
                        let col = self.tok.prev_operands.get(idx as usize).map_or(0, |v| v.iter().fold(7u32, |h, &x| hb(h, x)));
                        hs[6] = k ^ h2(21, h2(self.tok.prev_op, idx << 16 | tokh & 0xffff) ^ tl);
                        hs[7] = k ^ h2(22, h2(col, tl << 8 | c1));
                        hs[8] = k ^ h2(23, h2(self.tok.prev_op ^ self.tok.prev_op2.rotate_left(7), idx << 8 | c1));
                        hs[9] = k ^ h2(24, h2(self.word, c1));
                    }
                    _ => {
                        hs[6] = k ^ h2(25, c4 & 0xff00_ff00);
                        hs[7] = k ^ h2(26, h2(self.word, c1));
                        hs[8] = k ^ h2(27, (c4 & 0xffff_0000) | (self.hist.len() as u32 & 3));
                        hs[9] = k ^ h2(28, c4 & 0x00ff_00ff);
                    }
                }
            }
        }
        for i in 0..NCTX {
            let h = h2(self.hashes[i], 1);
            self.slot[i] = self.tables[i].find(h);
        }
    }
}
