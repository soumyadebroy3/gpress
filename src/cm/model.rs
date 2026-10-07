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
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Geom {
    /// Bytes per row (0 when unknown).
    pub stride: usize,
    /// Bytes per pixel (at least 1).
    pub bpp: usize,
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

struct Table {
    t: Vec<[u32; 16]>,
    mask: usize,
}

impl Table {
    fn new(bits: u32) -> Table {
        Table { t: vec![[0u32; 16]; 1 << bits], mask: (1 << bits) - 1 }
    }
    #[inline]
    fn find(&mut self, h: u32) -> usize {
        let i = (h as usize) & self.mask;
        let chk = (h >> 16) | 1;
        let b = &mut self.t[i];
        if b[0] != chk {
            *b = [Counter::new().0; 16];
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

pub struct Model {
    tb: Tables,
    pub hist: Vec<u8>,
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
    /// Original offsets of objects seen ("N G obj"), for predicting cross-reference tables.
    obj_offsets: std::collections::HashMap<i64, (u64, i64)>,
}

const MM_MIN: usize = 7;

impl Model {
    pub fn new(table_bits: u32) -> Model {
        let mut tables = Vec::with_capacity(NCTX);
        for _ in 0..NCTX {
            tables.push(Table::new(table_bits));
        }
        let mut m = Model {
            tb: Tables::new(),
            hist: Vec::new(),
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
            mx: Mixer::new(NIN, 7 * 8 * 3, 6),
            a1: Apm::new(256 * 7, 7),
            a2: Apm::new(65536, 7),
            pr: 2048,
            tok: Tok::default(),
            obj_offsets: std::collections::HashMap::new(),
        };
        m.contexts();
        m
    }

    /// Switches the kind of data that follows (at a byte boundary).
    pub fn set_kind(&mut self, kind: Kind, geom: Geom, orig_offset: u64) {
        self.kind = kind;
        self.geom = geom;
        self.seg_start = self.hist.len();
        self.seg_orig = orig_offset;
        self.exp.clear();
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

    pub fn code_byte<C: Coder>(&mut self, c: &mut C, byte: Option<u8>) -> u8 {
        for k in (0..8).rev() {
            let bit = byte.map(|b| ((b >> k) & 1) as u32);
            let p = self.predict();
            let got = c.code(bit, p);
            self.update(got);
        }
        *self.hist.last().unwrap()
    }

    fn predict(&mut self) -> i32 {
        let node = self.nib as usize;
        for i in 0..NCTX {
            let v = self.tables[i].t[self.slot[i]][node];
            let st = self.tb.stretch(Counter(v).p12());
            self.mx.add(st);
        }
        // Long match.
        let mut mm_state = 0;
        if self.mm_len > 0 && !self.mm_miss {
            let pb = self.hist[self.mm_ptr];
            let expect = ((pb as u32 | 256) >> (7 - self.bitpos)) & 1;
            let ctx = self.mm_len.min(63) * 2 + expect as usize;
            let p = self.mm_sm.p(ctx);
            self.mx.add(self.tb.stretch(p));
            mm_state = 1 + (self.mm_len >= 24) as usize;
        } else {
            self.mx.add(0);
        }
        // Expected text.
        if self.exp_i < self.exp.len() && !self.exp_miss {
            let eb = self.exp[self.exp_i];
            let expect = ((eb as u32 | 256) >> (7 - self.bitpos)) & 1;
            let ctx = ((self.exp_hits.min(15) as usize * 2 + expect as usize) * 4) + (self.kind as usize % 4);
            let p = self.exp_sm.p(ctx);
            self.mx.add(self.tb.stretch(p));
        } else {
            self.mx.add(0);
        }
        self.mx.add(256);
        let sel = (self.kind as usize * 8 + self.bitpos as usize) * 3 + mm_state;
        let pm = self.mx.mix(sel);
        let c1 = (self.c4 & 0xff) as usize;
        let p1 = self.a1.p(pm, self.c0 as usize + 256 * self.kind as usize, &self.tb);
        let p2 = self.a2.p(pm, (self.c0 as usize | c1 << 8) & 0xffff, &self.tb);
        self.pr = ((pm + p1 + 2 * p2 + 2) >> 2).clamp(1, 4095);
        self.pr
    }

    fn update(&mut self, bit: u32) {
        let node = self.nib as usize;
        for i in 0..NCTX {
            let v = &mut self.tables[i].t[self.slot[i]][node];
            let mut c = Counter(*v);
            c.update(bit, 255);
            *v = c.0;
        }
        if self.mm_len > 0 && !self.mm_miss {
            self.mm_sm.update(bit);
            let pb = self.hist[self.mm_ptr];
            let expect = ((pb as u32 | 256) >> (7 - self.bitpos)) & 1;
            if expect != bit {
                self.mm_miss = true;
            }
        }
        if self.exp_i < self.exp.len() && !self.exp_miss {
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
            let b = (self.c0 & 0xff) as u8;
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
        if n >= MM_MIN {
            let mut h = 0u32;
            for &x in &self.hist[n - MM_MIN..] {
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
                    if l >= MM_MIN {
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
            _ => {}
        }
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

    fn contexts(&mut self) {
        let k = (self.kind as u32 + 1) << 28;
        let c4 = self.c4;
        let c1 = c4 & 0xff;
        let tokh = self.tok.tok.iter().fold(0u32, |h, &x| hb(h, x));
        let tl = self.tok.tok.len() as u32;
        let hs = &mut self.hashes;
        match self.kind {
            Kind::Image if self.geom.stride > 0 => {
                let pos = self.hist.len();
                let s = self.geom.stride;
                let bpp = self.geom.bpp.max(1);
                let at = |back: usize| -> u32 {
                    if pos >= back && pos - back >= self.seg_start {
                        self.hist[pos - back] as u32
                    } else {
                        0
                    }
                };
                let w = at(bpp);
                let n = at(s);
                let nw = at(s + bpp);
                let ne = if s > bpp { at(s - bpp) } else { 0 };
                let ww = at(2 * bpp);
                let nn = at(2 * s);
                let ch = (((pos - self.seg_start) % s) % bpp) as u32;
                let med = {
                    let (a, b, c) = (w as i32, n as i32, nw as i32);
                    let p = a + b - c;
                    p.clamp(a.min(b), a.max(b)) as u32
                };
                let grad = ((w as i32 + n as i32 - nw as i32).clamp(0, 255)) as u32;
                hs[0] = k ^ h2(1, w | ch << 8);
                hs[1] = k ^ h2(2, n | ch << 8);
                hs[2] = k ^ h2(3, w | n << 8 | ch << 16);
                hs[3] = k ^ h2(4, n | ne << 8 | ch << 16);
                hs[4] = k ^ h2(5, w | nw << 8 | n << 16);
                hs[5] = k ^ h2(6, med | ch << 8);
                hs[6] = k ^ h2(7, ((w + n) >> 1) | ch << 8 | (ne >> 4) << 16);
                hs[7] = k ^ h2(8, grad | (w >> 3) << 8 | (n >> 3) << 13 | ch << 18);
                hs[8] = k ^ h2(9, w | ww << 8 | ch << 16);
                hs[9] = k ^ h2(10, n | nn << 8 | ch << 16);
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
