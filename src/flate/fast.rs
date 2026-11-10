//! A fast Deflate encoder, from very fast greedy matching up to a near-optimal parse.
//!
//! Levels 1 to 7 find matches in hash chains: 4-byte hashes lead to chains of earlier positions, and (from
//! level 3) a one-entry table of 3-byte hashes catches short matches. Level 1 takes the longest match found
//! at once (greedy), 2 to 6 first look one position ahead for a better one (lazy), 7 looks two positions
//! ahead. Levels 8 to 12 keep the earlier positions in binary trees sorted by the text that follows them,
//! which give for every position the closest match of each longer length; each block is then parsed as the
//! cheapest path over its positions, with symbol costs from a greedy pass (and the previous block's path),
//! then from the path found.
//!
//! Blocks end where the symbols change character. As symbols are made, a few coarse kinds of literal and
//! match are counted; every `BATCH` of them the latest batch is compared with the block so far, and a large
//! enough difference ends the block where the batch began (no search over split points). Each block gets
//! its codes from the shared Huffman builder and goes out dynamic, fixed or stored, whichever is smallest,
//! through the shared block writer.
//!
//! The input is cut into segments at multiples of `SEGMENT`. Each is encoded on its own, its match finder
//! primed with up to `DICT` bytes of input before it, so threads can share the work, the size barely
//! changes, and the output never depends on how many threads there are. A segment's coded blocks are
//! written from bit 0 of its own buffer and spliced into the stream at the right bit; stored blocks depend
//! on byte alignment, so they are written in that final pass.
//!
//! Memory per segment in flight: the match finder's tables (at most 640 KiB), the symbols of one block (at
//! most 1.3 MB), for levels 8 to 12 the candidate matches and the parse arrays of one block (at most 8 MB),
//! and the segment's output.
//!
//! Several parts are adapted from libdeflate's compressor (lib/deflate_compress.c, hc_matchfinder.h,
//! bt_matchfinder.h and matchfinder_common.h), rewritten in Rust with their constants tuned again: the
//! statistics that end a block, the hash-chain search, the lazy-match gain, the symbol costs taken from a
//! block's codes, the cheapest-path parse, the pass that gathers statistics and skips past a long match, and
//! the binary trees of levels 8 to 12 (`Tree::walk`, which like matcher.rs's search also follows LzFind.c's
//! GetMatchesSpec1 from Igor Pavlov's LZMA SDK, public domain). The segments, the threads and the bit
//! splicing are ours. libdeflate's licence:
//!
//! Copyright 2016 Eric Biggers
//! Copyright 2024 Google LLC
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
//! associated documentation files (the "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the
//! following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all copies or substantial
//! portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
//! LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO
//! EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
//! IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR
//! THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use super::bits::BitWriter;
use super::block::{
    data_bits, fixed_bits, patch_distances, stored_bits, write_dynamic, write_fixed, write_stored, DynCode, Header,
};
use super::huffman::lengths_into;
use super::lz::{Hist, Sym};
use super::tables::*;
use crate::clock::Deadline;
use crate::par::{self, Pool};

/// Segments start at multiples of this.
pub const SEGMENT: usize = 1 << 20;
/// Each segment's match finder is primed with up to this much input before it.
pub const DICT: usize = 32 << 10;
/// The strongest level.
pub const LEVELS: u8 = 12;

/// Matches reach back at most this far: one less than Deflate allows, so that the table slot of the
/// position being added never belongs to one still in reach.
const MAX_DIST: usize = WINDOW - 1;
/// Segments given to the threads at once (bounds the finished output held before it is written).
const GROUP: usize = 16;
/// An empty table entry: as a position it is always out of reach.
const NIL: u32 = u32::MAX;

/// How far back position `c`, read from a table, lies from `p`. An empty entry (`NIL`) gives `usize::MAX`, so
/// the window test rejects it whatever the width of usize: on a 32-bit target (WebAssembly) `p - NIL` wraps
/// round to `p + 1` and would pass for a match.
#[inline(always)]
fn back(p: usize, c: usize) -> usize {
    if c == NIL as usize {
        usize::MAX
    } else {
        p.wrapping_sub(c)
    }
}

/// How a level chooses between literals and matches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// The longest match at once.
    Greedy,
    /// A match is dropped for a literal when the next position has a better one.
    Lazy,
    /// As `Lazy`, also looking two positions ahead.
    Lazy2,
    /// The cheapest path through the block's candidate matches.
    Optimal,
}

#[derive(Clone, Copy, Debug)]
struct Level {
    mode: Mode,
    /// Most earlier positions compared at each position.
    depth: u32,
    /// A match at least this long is taken at once (and, for `Optimal`, the positions it covers are not
    /// searched).
    nice: usize,
    /// `Optimal`: parses per block, each under the costs of the one before.
    passes: u32,
    /// Hash chains: also look for 3-byte matches.
    short: bool,
}

const fn lv(mode: Mode, depth: u32, nice: usize, passes: u32, short: bool) -> Level {
    Level { mode, depth, nice, passes, short }
}

/// The levels, chosen on 3373 Flate streams of PDF files (152.5 MB) for the smallest output at each speed.
const TABLE: [Level; LEVELS as usize] = [
    lv(Mode::Greedy, 2, 16, 0, false),
    lv(Mode::Lazy, 3, 16, 0, false),
    lv(Mode::Lazy, 6, 16, 0, true),
    lv(Mode::Lazy, 12, 32, 0, true),
    lv(Mode::Lazy, 24, 48, 0, true),
    lv(Mode::Lazy, 64, 96, 0, true),
    lv(Mode::Lazy2, 100, 258, 0, true),
    lv(Mode::Optimal, 6, 24, 1, true),
    lv(Mode::Optimal, 12, 32, 1, true),
    lv(Mode::Optimal, 12, 32, 2, true),
    lv(Mode::Optimal, 24, 64, 2, true),
    lv(Mode::Optimal, 35, 100, 2, true),
];

/// Raw Deflate data for `data` at fast level 1 to `LEVELS` (others are clamped to that range). Segments run
/// on `pool`; once `deadline` has passed, the segments still to do are written stored (the caller drops
/// such an output). The output never depends on the number of threads.
pub fn deflate(data: &[u8], level: u8, pool: Option<&Pool>, deadline: Deadline) -> Vec<u8> {
    let lv = TABLE[level.clamp(1, LEVELS) as usize - 1];
    if data.is_empty() {
        let mut w = BitWriter::new();
        write_fixed(&mut w, &[], true);
        return w.finish();
    }
    let n = data.len().div_ceil(SEGMENT);
    if n == 1 {
        let mut pieces = segment(data, 0, &lv, deadline);
        // Usual case: the segment's bits are the whole stream.
        if let [Piece::Bits(..)] = pieces[..] {
            if let Some(Piece::Bits(bytes, _)) = pieces.pop() {
                return bytes;
            }
        }
        let mut s = Stream { w: BitWriter::with_capacity(data.len() / 3 + 64), pending: None };
        for p in pieces {
            s.add(data, p);
        }
        return s.finish(data);
    }
    let mut s = Stream { w: BitWriter::with_capacity(data.len() / 3 + 64), pending: None };
    let mut k = 0;
    while k < n {
        let m = GROUP.min(n - k);
        let outs = par::map(pool, m, None, |i| segment(data, k + i, &lv, deadline));
        for pieces in outs {
            for p in pieces {
                s.add(data, p);
            }
        }
        k += m;
    }
    s.finish(data)
}

// ---------------------------------------------------------------- output: pieces and the final stream

/// Part of a segment's output: Huffman-coded blocks (bytes and their exact number of bits, from bit 0), or
/// a stored block for an input range (written in the final pass, where its alignment is known).
enum Piece {
    Bits(Vec<u8>, u64),
    Stored(usize, usize),
}

/// A segment's output so far.
struct Out {
    pieces: Vec<Piece>,
    w: BitWriter,
    /// Bits of the pieces before `w`, as the segment counts them (stored blocks priced from bit 0).
    before: u64,
}

impl Out {
    fn new(cap: usize) -> Out {
        Out { pieces: Vec::new(), w: BitWriter::with_capacity(cap), before: 0 }
    }

    fn close(&mut self) {
        let n = self.w.bit_len();
        if n > 0 {
            let w = std::mem::take(&mut self.w);
            self.before += n;
            self.pieces.push(Piece::Bits(w.finish(), n));
        }
    }

    /// Writes the block of `syms` (counted in `h`, end-of-block included) for input `a..b` as the smallest
    /// of dynamic, fixed and stored.
    fn block(&mut self, syms: &[Sym], h: &Hist, a: usize, b: usize, last: bool) {
        let code = dynamic_code(h);
        let dyn_bits = 3 + code.bits;
        let fix_bits = 3 + fixed_bits(h);
        let here = self.before + self.w.bit_len();
        let st_bits = stored_bits(b - a, here);
        if st_bits < dyn_bits.min(fix_bits) {
            // The final pass writes it, as the last block of the stream when nothing follows.
            self.close();
            self.before = here + st_bits;
            self.pieces.push(Piece::Stored(a, b));
        } else if fix_bits <= dyn_bits {
            write_fixed(&mut self.w, syms, last);
        } else {
            write_dynamic(&mut self.w, syms, &code, last);
        }
    }

    fn finish(mut self) -> Vec<Piece> {
        self.close();
        self.pieces
    }
}

/// The final stream. Neighbouring stored pieces are joined, so that incompressible input costs the
/// stored-block overhead (5 bytes per 65535) and at most 5 bytes more: the last segment, when short, prices a
/// stored block of its own at 5 bytes of overhead, so a few random bytes there can go into a fixed block that
/// costs up to 5 bytes over them, where joining them to the stored run before would have cost nothing.
struct Stream {
    w: BitWriter,
    pending: Option<(usize, usize)>,
}

impl Stream {
    fn add(&mut self, data: &[u8], p: Piece) {
        match p {
            Piece::Stored(a, b) => match self.pending {
                Some((pa, pb)) if pb == a => self.pending = Some((pa, b)),
                _ => {
                    self.flush(data, false);
                    self.pending = Some((a, b));
                }
            },
            Piece::Bits(bytes, n) => {
                self.flush(data, false);
                splice(&mut self.w, &bytes, n);
            }
        }
    }

    fn flush(&mut self, data: &[u8], last: bool) {
        if let Some((a, b)) = self.pending.take() {
            write_stored(&mut self.w, &data[a..b], last);
        }
    }

    fn finish(mut self, data: &[u8]) -> Vec<u8> {
        // A stored piece left at the end is the last block; otherwise the last segment's bits end with one.
        self.flush(data, true);
        self.w.finish()
    }
}

/// `k` bits (at most 8) of `src` from bit `pos`.
fn bits_at(src: &[u8], pos: u64, k: u32) -> u32 {
    let i = (pos / 8) as usize;
    let s = (pos % 8) as u32;
    let mut v = (src[i] as u32) >> s;
    if s + k > 8 {
        v |= (src[i + 1] as u32) << (8 - s);
    }
    v & ((1u32 << k) - 1)
}

/// Appends the first `nbits` bits of `src` to `w`.
fn splice(w: &mut BitWriter, src: &[u8], nbits: u64) {
    let mut done = 0u64;
    let r = (w.bit_len() % 8) as u32;
    if r != 0 {
        // Complete the writer's partial byte.
        let k = (8 - r as u64).min(nbits) as u32;
        w.put(bits_at(src, 0, k), k);
        done = k as u64;
    }
    if done == nbits {
        return;
    }
    // The writer is on a byte boundary (nothing pending), so whole bytes go straight to its buffer, shifted
    // by the bits already taken.
    let whole = ((nbits - done) / 8) as usize;
    let s = done as u32;
    if s == 0 {
        w.out.extend_from_slice(&src[..whole]);
    } else {
        w.out.reserve(whole);
        let mut j = 0;
        while j + 8 < src.len() && j + 8 <= whole {
            let v = u64::from_le_bytes(src[j..j + 8].try_into().unwrap());
            let o = (v >> s) | ((src[j + 8] as u64) << (64 - s));
            w.out.extend_from_slice(&o.to_le_bytes());
            j += 8;
        }
        while j < whole {
            let hi = src.get(j + 1).map_or(0, |&x| x << (8 - s));
            w.out.push((src[j] >> s) | hi);
            j += 1;
        }
    }
    let tail = ((nbits - done) % 8) as u32;
    if tail > 0 {
        w.put(bits_at(src, done + whole as u64 * 8, tail), tail);
    }
}

// ---------------------------------------------------------------- block codes

/// The dynamic code for a histogram: optimal lengths of at most 15 bits and a header in one cheap pass.
fn dynamic_code(h: &Hist) -> DynCode {
    let mut ll = vec![0u8; LITLEN_SYMS];
    let mut d = vec![0u8; DIST_SYMS];
    lengths_into(&h.ll, 15, &mut ll);
    lengths_into(&h.d, 15, &mut d);
    // A code with one symbol (end-of-block alone) is incomplete: give it a partner.
    if ll.iter().filter(|&&l| l != 0).count() == 1 {
        let other = if ll[0] == 0 { 0 } else { 1 };
        ll[other] = 1;
    }
    patch_distances(&mut d);
    let header = header(&ll, &d);
    let bits = header.bits + data_bits(h, &ll, &d);
    DynCode { ll, d, header, bits }
}

fn token_extra(sym: u8) -> u64 {
    match sym {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    }
}

/// The header of a dynamic block: the code lengths as runs (16 repeats the previous length 3 to 6 times,
/// 17 and 18 give 3 to 10 and 11 to 138 zeros), and the code-length code for those tokens.
fn header(ll: &[u8], d: &[u8]) -> Header {
    let hlit = ll.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1).max(257);
    let hdist = d.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1).max(1);
    let mut all = [0u8; LITLEN_SYMS + DIST_SYMS];
    all[..hlit].copy_from_slice(&ll[..hlit]);
    all[hlit..hlit + hdist].copy_from_slice(&d[..hdist]);
    let seq = &all[..hlit + hdist];
    let mut tokens: Vec<(u8, u8)> = Vec::with_capacity(seq.len());
    let mut i = 0;
    while i < seq.len() {
        let v = seq[i];
        let mut run = 1;
        while i + run < seq.len() && seq[i + run] == v {
            run += 1;
        }
        i += run;
        if v == 0 {
            while run >= 11 {
                let k = run.min(138);
                tokens.push((18, (k - 11) as u8));
                run -= k;
            }
            if run >= 3 {
                tokens.push((17, (run - 3) as u8));
                run = 0;
            }
        } else {
            tokens.push((v, 0));
            run -= 1;
            while run >= 3 {
                let k = run.min(6);
                tokens.push((16, (k - 3) as u8));
                run -= k;
            }
        }
        for _ in 0..run {
            tokens.push((v, 0));
        }
    }
    let mut f = [0u32; 19];
    for &(s, _) in &tokens {
        f[s as usize] += 1;
    }
    let mut cl = [0u8; 19];
    lengths_into(&f, 7, &mut cl);
    // The code-length code must be complete too.
    if cl.iter().filter(|&&x| x != 0).count() == 1 {
        if let Some(&other) = CL_ORDER.iter().find(|&&s| cl[s] == 0) {
            cl[other] = 1;
        }
    }
    let hclen = CL_ORDER.iter().rposition(|&s| cl[s] != 0).map_or(0, |p| p + 1).max(4);
    let mut bits = 14 + 3 * hclen as u64;
    for &(s, _) in &tokens {
        bits += cl[s as usize] as u64 + token_extra(s);
    }
    Header { hlit, hdist, hclen, cl, tokens, bits }
}

fn zero_hist() -> Hist {
    Hist { ll: [0; LITLEN_SYMS], d: [0; DIST_SYMS] }
}

/// `h += x`; then `x` is cleared.
fn take_into(h: &mut Hist, x: &mut Hist) {
    for (a, b) in h.ll.iter_mut().zip(x.ll.iter_mut()) {
        *a += *b;
        *b = 0;
    }
    for (a, b) in h.d.iter_mut().zip(x.d.iter_mut()) {
        *a += *b;
        *b = 0;
    }
}

#[inline(always)]
fn count_match(h: &mut Hist, len: usize, dist: usize) {
    h.ll[257 + LEN_CODE[len] as usize] += 1;
    h.d[dist_code(dist)] += 1;
}

// ---------------------------------------------------------------- where blocks end

/// Observations every block-end check compares.
const BATCH: u32 = 512;
/// Blocks are at least this long (bytes), unless the segment is shorter...
const MIN_BLOCK: usize = 2000;
/// ...and at most about this long.
const MAX_BLOCK: usize = 300_000;
/// Kinds of observation: eight for literals, two for matches.
const KINDS: usize = 10;

/// The kind of each literal: its top two bits and its lowest one (a coarse fingerprint of the bytes in use).
static LIT_KIND: [u8; 256] = lit_kinds();

const fn lit_kinds() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut b = 0;
    while b < 256 {
        t[b] = ((b >> 6) * 2 + (b & 1)) as u8;
        b += 1;
    }
    t
}

/// Counts of the kinds of symbol in the block so far and in the latest batch.
struct Split {
    seen: [u32; KINDS],
    now: [u32; KINDS],
    n_seen: u32,
    n_now: u32,
}

impl Split {
    fn new() -> Split {
        Split { seen: [0; KINDS], now: [0; KINDS], n_seen: 0, n_now: 0 }
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        self.now[LIT_KIND[b as usize] as usize] += 1;
        self.n_now += 1;
    }

    #[inline(always)]
    fn mat(&mut self, len: usize) {
        self.now[8 + (len >= 6) as usize] += 1;
        self.n_now += 1;
    }

    /// Whether the latest batch differs enough from the block so far (`len` bytes) to start a new block.
    fn changed(&self, len: usize) -> bool {
        let (a, b) = (self.n_now as u64, self.n_seen as u64);
        if a == 0 || b == 0 {
            return false;
        }
        // The sum of |now/a - seen/b| (from 0 to 2), times a * b.
        let mut diff = 0u64;
        for i in 0..KINDS {
            diff += (self.now[i] as u64 * b).abs_diff(self.seen[i] as u64 * a);
        }
        // A difference over about 0.3 ends the block. Short blocks need more (a new header costs more than
        // their small gain), long ones less.
        let mut limit = a * b * 10 / 32;
        let total = a + b;
        if total < 8192 {
            limit += limit * (8192 - total) / 8192;
        }
        diff + (len as u64 / 2048) * b >= limit
    }

    /// The batch joins the block.
    fn merge(&mut self) {
        for i in 0..KINDS {
            self.seen[i] += self.now[i];
            self.now[i] = 0;
        }
        self.n_seen += self.n_now;
        self.n_now = 0;
    }

    /// A new block starts with the batch.
    fn restart(&mut self) {
        self.seen = self.now;
        self.now = [0; KINDS];
        self.n_seen = self.n_now;
        self.n_now = 0;
    }
}

// ---------------------------------------------------------------- segments

/// One segment's input and output.
struct Seg<'a> {
    /// The segment with the dictionary before it; positions are indices into it.
    buf: &'a [u8],
    /// Where `buf` starts in the input.
    base: usize,
    /// Where the segment starts in `buf`.
    start: usize,
    /// Whether the segment ends the input (its last block is final).
    last: bool,
    out: Out,
}

impl Seg<'_> {
    fn block(&mut self, syms: &[Sym], h: &Hist, a: usize, b: usize, last: bool) {
        self.out.block(syms, h, self.base + a, self.base + b, last);
    }
}

/// The pieces of segment `k`.
fn segment(data: &[u8], k: usize, lv: &Level, deadline: Deadline) -> Vec<Piece> {
    let a = k * SEGMENT;
    let b = (a + SEGMENT).min(data.len());
    // Out of time: stored (the caller drops the output).
    if deadline.passed() {
        return vec![Piece::Stored(a, b)];
    }
    let base = a.saturating_sub(DICT);
    let mut seg = Seg { buf: &data[base..b], base, start: a - base, last: b == data.len(), out: Out::new((b - a) / 3) };
    match lv.mode {
        Mode::Optimal => optimal(&mut seg, lv),
        _ => hashed(&mut seg, lv),
    }
    seg.out.finish()
}

// ---------------------------------------------------------------- matching helpers

#[inline(always)]
fn load4(buf: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(buf[p..p + 4].try_into().unwrap())
}

#[inline(always)]
fn hash4(x: u32, shift: u32) -> usize {
    (x.wrapping_mul(0x1E35_A7BD) >> shift) as usize
}

/// Hash of the low three bytes.
#[inline(always)]
fn hash3(x: u32, shift: u32) -> usize {
    ((x << 8).wrapping_mul(0x1E35_A7BD) >> shift) as usize
}

/// Length of the common prefix of `buf[a..]` and `buf[b..]`, at most `max` (`a < b`, `b + max <= buf.len()`).
#[inline(always)]
fn common(buf: &[u8], a: usize, b: usize, max: usize) -> usize {
    let x = &buf[a..a + max];
    let y = &buf[b..b + max];
    let mut n = 0;
    let (xs, xr) = x.as_chunks::<8>();
    let (ys, yr) = y.as_chunks::<8>();
    for (u, v) in xs.iter().zip(ys) {
        let z = u64::from_le_bytes(*u) ^ u64::from_le_bytes(*v);
        if z != 0 {
            return n + (z.trailing_zeros() / 8) as usize;
        }
        n += 8;
    }
    for (u, v) in xr.iter().zip(yr) {
        if u != v {
            break;
        }
        n += 1;
    }
    n
}

/// Table bits for an input of `len` bytes: enough for it, at most `max`.
fn table_bits(len: usize, max: u32) -> u32 {
    (usize::BITS - len.max(2).saturating_sub(1).leading_zeros()).clamp(8, max)
}

/// Slots of the per-position tables: one per position of the window (fewer for short inputs).
fn window_slots(len: usize) -> usize {
    len.max(1).next_power_of_two().min(WINDOW)
}

#[inline(always)]
fn floor_log2(x: usize) -> i32 {
    (usize::BITS - 1) as i32 - x.max(1).leading_zeros() as i32
}

// ---------------------------------------------------------------- hash chains (levels 1 to 7)

struct Chains {
    head4: Vec<u32>,
    shift4: u32,
    head3: Vec<u32>,
    shift3: u32,
    /// The previous position with the same 4-byte hash, per window slot.
    prev: Vec<u32>,
    wmask: usize,
    short: bool,
}

impl Chains {
    fn new(len: usize, short: bool) -> Chains {
        let b4 = table_bits(len, 16);
        let b3 = if short { table_bits(len, 15) } else { 1 };
        let w = window_slots(len);
        Chains {
            head4: vec![NIL; 1 << b4],
            shift4: 32 - b4,
            head3: vec![NIL; 1 << b3],
            shift3: 32 - b3,
            prev: vec![NIL; w],
            wmask: w - 1,
            short,
        }
    }

    /// Adds position `p` (`p + 4 <= buf.len()`).
    #[inline(always)]
    fn insert(&mut self, buf: &[u8], p: usize) {
        let x = load4(buf, p);
        if self.short {
            self.head3[hash3(x, self.shift3)] = p as u32;
        }
        let h = hash4(x, self.shift4);
        self.prev[p & self.wmask] = self.head4[h];
        self.head4[h] = p as u32;
    }

    /// Adds position `p` and returns the longest match there longer than `best`, with its distance (or
    /// `(best, 0)`), comparing at most `depth` earlier positions. Needs `p + 4 <= buf.len()` and
    /// `4 <= max <= buf.len() - p`.
    #[inline(always)]
    fn find(&mut self, buf: &[u8], p: usize, max: usize, nice: usize, depth: u32, best: usize) -> (usize, usize) {
        let x = load4(buf, p);
        let mut best = best;
        let mut dist = 0;
        if self.short {
            let h = hash3(x, self.shift3);
            let c = self.head3[h] as usize;
            self.head3[h] = p as u32;
            let d = back(p, c);
            if best < 3 && d.wrapping_sub(1) < MAX_DIST && (load4(buf, c) ^ x) << 8 == 0 {
                best = 3;
                dist = d;
            }
        }
        let h = hash4(x, self.shift4);
        let mut c = self.head4[h] as usize;
        self.head4[h] = p as u32;
        self.prev[p & self.wmask] = c as u32;
        let nice = nice.min(max);
        if best >= nice {
            return (best, dist);
        }
        let mut left = depth;
        loop {
            let d = back(p, c);
            if d.wrapping_sub(1) >= MAX_DIST {
                break;
            }
            // A longer match must agree on the four bytes ending at `best` and on the first four.
            let maybe = if best >= 4 {
                load4(buf, c + best - 3) == load4(buf, p + best - 3) && load4(buf, c) == x
            } else {
                load4(buf, c) == x
            };
            if maybe {
                let len = 4 + common(buf, c + 4, p + 4, max - 4);
                if len > best {
                    best = len;
                    dist = d;
                    if len >= nice {
                        break;
                    }
                }
            }
            left -= 1;
            if left == 0 {
                break;
            }
            c = self.prev[c & self.wmask] as usize;
        }
        (best, dist)
    }
}

/// How much better a match of `nl` at `nd` is than one of `cl` at `cd`: four units per byte longer, one
/// per halving of the distance.
#[inline(always)]
fn gain(nl: usize, nd: usize, cl: usize, cd: usize) -> i32 {
    if nl < cl {
        return i32::MIN;
    }
    4 * (nl - cl) as i32 + floor_log2(cd) - floor_log2(nd)
}

/// The symbols of the block being made, their counts and the split statistics.
struct Blocks {
    syms: Vec<Sym>,
    /// Counts of the symbols before the mark (end-of-block included) and since the mark.
    seen: Hist,
    now: Hist,
    split: Split,
    /// Where the block starts and where the latest batch of observations began (positions in the segment's
    /// buffer, and the symbol where the batch began).
    start: usize,
    mark: usize,
    mark_sym: usize,
}

impl Blocks {
    fn new(start: usize, cap: usize) -> Blocks {
        Blocks {
            syms: Vec::with_capacity(cap),
            seen: Hist::empty(),
            now: zero_hist(),
            split: Split::new(),
            start,
            mark: start,
            mark_sym: 0,
        }
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        self.syms.push(Sym::lit(b));
        self.now.ll[b as usize] += 1;
        self.split.lit(b);
    }

    #[inline(always)]
    fn mat(&mut self, len: usize, dist: usize) {
        self.syms.push(Sym::mat(len, dist));
        count_match(&mut self.now, len, dist);
        self.split.mat(len);
    }

    /// After each step, with the position reached: ends the block when it is long enough, or when the
    /// latest batch of symbols differs from the block so far.
    #[inline(always)]
    fn step(&mut self, p: usize, seg: &mut Seg) {
        if self.split.n_now >= BATCH || p - self.start >= MAX_BLOCK {
            self.check(p, seg);
        }
    }

    fn check(&mut self, p: usize, seg: &mut Seg) {
        // (Never at the end of the segment, which would leave an empty last block.)
        if p - self.start >= MAX_BLOCK && p < seg.buf.len() {
            take_into(&mut self.seen, &mut self.now);
            seg.block(&self.syms, &self.seen, self.start, p, false);
            self.syms.clear();
            self.seen = Hist::empty();
            self.split = Split::new();
            self.start = p;
        } else if self.mark - self.start >= MIN_BLOCK
            && seg.buf.len() - self.mark >= MIN_BLOCK
            && self.split.changed(self.mark - self.start)
        {
            // The block ends where the batch began; the batch starts the next one.
            seg.block(&self.syms[..self.mark_sym], &self.seen, self.start, self.mark, false);
            self.syms.drain(..self.mark_sym);
            self.seen = std::mem::replace(&mut self.now, zero_hist());
            self.seen.ll[END_OF_BLOCK] = 1;
            self.split.restart();
            self.start = self.mark;
        } else {
            take_into(&mut self.seen, &mut self.now);
            self.split.merge();
        }
        self.mark = p;
        self.mark_sym = self.syms.len();
    }

    fn finish(mut self, seg: &mut Seg) {
        let end = seg.buf.len();
        if self.start < end || seg.last {
            take_into(&mut self.seen, &mut self.now);
            seg.block(&self.syms, &self.seen, self.start, end, seg.last);
        }
    }
}

/// Levels 1 to 7: greedy or lazy matching over hash chains.
fn hashed(seg: &mut Seg, lv: &Level) {
    let buf = seg.buf;
    let end = buf.len();
    // Positions below `stop` have four bytes to hash.
    let stop = end.saturating_sub(3);
    let mut mf = Chains::new(end, lv.short);
    for q in 0..seg.start.min(stop) {
        mf.insert(buf, q);
    }
    let mut bk = Blocks::new(seg.start, (end - seg.start).min(MAX_BLOCK) + 2 * MAX_MATCH);
    let lazy = lv.mode != Mode::Greedy;
    let two = lv.mode == Mode::Lazy2;
    let mut p = seg.start;
    while p < end {
        if p >= stop {
            bk.lit(buf[p]);
            p += 1;
            bk.step(p, seg);
            continue;
        }
        let (mut len, mut dist) = mf.find(buf, p, (end - p).min(MAX_MATCH), lv.nice, lv.depth, 2);
        if len < MIN_MATCH {
            bk.lit(buf[p]);
            p += 1;
            bk.step(p, seg);
            continue;
        }
        // Positions below `ins` are in the chains.
        let mut ins = p + 1;
        if lazy {
            while len < lv.nice && p + 1 < stop {
                let (l1, d1) = mf.find(buf, p + 1, (end - p - 1).min(MAX_MATCH), lv.nice, lv.depth, len - 1);
                ins = p + 2;
                if gain(l1, d1, len, dist) > 1 {
                    bk.lit(buf[p]);
                    p += 1;
                    bk.step(p, seg);
                    (len, dist) = (l1, d1);
                    continue;
                }
                if two && p + 2 < stop {
                    let (l2, d2) = mf.find(buf, p + 2, (end - p - 2).min(MAX_MATCH), lv.nice, lv.depth, len - 1);
                    ins = p + 3;
                    if gain(l2, d2, len, dist) > 4 {
                        bk.lit(buf[p]);
                        bk.lit(buf[p + 1]);
                        p += 2;
                        bk.step(p, seg);
                        (len, dist) = (l2, d2);
                        continue;
                    }
                }
                break;
            }
        }
        bk.mat(len, dist);
        for q in ins..(p + len).min(stop) {
            mf.insert(buf, q);
        }
        p += len;
        bk.step(p, seg);
    }
    bk.finish(seg);
}

// ---------------------------------------------------------------- binary trees (levels 8 to 12)

/// The greedy pass behind the first costs takes a 3-byte match only this close (farther ones are rarely
/// worth it in the end).
const STATS_FAR3: usize = 1024;
/// Candidate matches kept per position at most (the longest replaces the last one beyond this).
const MAX_CANDS: usize = 64;
/// Positions of one block for the optimal levels at most.
const MAX_OPT_BLOCK: usize = 1 << 18;
/// Candidate matches of one block at most (a block ends early when they would not fit).
const CAND_CAP: usize = 3 * MAX_OPT_BLOCK;

struct Tree {
    head4: Vec<u32>,
    shift4: u32,
    head3: Vec<u32>,
    shift3: u32,
    /// Per window slot: the roots of the subtrees with smaller and with larger text.
    son: Vec<u32>,
    wmask: usize,
}

impl Tree {
    fn new(len: usize) -> Tree {
        let b4 = table_bits(len, 16);
        let b3 = table_bits(len, 15);
        let w = window_slots(len);
        Tree {
            head4: vec![NIL; 1 << b4],
            shift4: 32 - b4,
            head3: vec![NIL; 1 << b3],
            shift3: 32 - b3,
            son: vec![NIL; 2 * w],
            wmask: w - 1,
        }
    }

    /// Adds position `p` to its tree. With `RECORD`, appends to `out` the matches found, longer and
    /// longer, each the closest of its length that the search met, and returns the longest length (2 when
    /// none). Needs `p + 4 <= buf.len()` and `4 <= max <= buf.len() - p`.
    #[inline(always)]
    fn walk<const RECORD: bool>(
        &mut self,
        buf: &[u8],
        p: usize,
        max: usize,
        nice: usize,
        depth: u32,
        out: &mut Vec<(u16, u16)>,
    ) -> usize {
        let x = load4(buf, p);
        let first = out.len();
        let mut best = 2;
        let h3 = hash3(x, self.shift3);
        let c3 = self.head3[h3] as usize;
        self.head3[h3] = p as u32;
        if RECORD {
            let d = back(p, c3);
            if d.wrapping_sub(1) < MAX_DIST && (load4(buf, c3) ^ x) << 8 == 0 {
                out.push((3, d as u16));
                best = 3;
            }
        }
        let h4 = hash4(x, self.shift4);
        let mut c = self.head4[h4] as usize;
        self.head4[h4] = p as u32;
        let nice = nice.min(max);
        // Comparisons go no further than needed: the tree only asks whether a match reaches `nice`, the
        // recorded matches want their full length.
        let reach = if RECORD { max } else { nice };
        let m = self.wmask;
        // Where the next node with smaller (larger) text than `p` attaches, and how much of `p` it shares.
        let mut lt = 2 * (p & m);
        let mut gt = lt + 1;
        let (mut lt_len, mut gt_len) = (0usize, 0usize);
        let mut left = depth;
        loop {
            let d = back(p, c);
            if d.wrapping_sub(1) >= MAX_DIST || left == 0 {
                self.son[lt] = NIL;
                self.son[gt] = NIL;
                break;
            }
            left -= 1;
            let pair = 2 * (c & m);
            let mut len = lt_len.min(gt_len);
            if buf[c + len] == buf[p + len] {
                len += 1 + common(buf, c + len + 1, p + len + 1, reach - len - 1);
                if RECORD && len > best {
                    best = len;
                    if out.len() - first < MAX_CANDS {
                        out.push((len as u16, d as u16));
                    } else if let Some(e) = out.last_mut() {
                        *e = (len as u16, d as u16);
                    }
                }
                if len >= nice {
                    // Same text as far as we care: `p` takes this node's place.
                    self.son[lt] = self.son[pair];
                    self.son[gt] = self.son[pair + 1];
                    break;
                }
            }
            if buf[c + len] < buf[p + len] {
                self.son[lt] = c as u32;
                lt = pair + 1;
                c = self.son[lt] as usize;
                lt_len = len;
            } else {
                self.son[gt] = c as u32;
                gt = pair;
                c = self.son[gt] as usize;
                gt_len = len;
            }
        }
        // The 3-byte match is no use when a closer match is at least as long.
        if RECORD && out.len() > first + 1 && out[first].0 == 3 && out[first + 1].1 < out[first].1 {
            out.remove(first);
        }
        best
    }
}

/// Bit costs (in sixteenths of a bit) of literals, of lengths (symbol and extra bits) and of distance
/// codes (symbol and extra bits).
struct Costs {
    lit: [u32; 256],
    len: [u32; MAX_MATCH + 1],
    dist: [u32; DIST_SYMS],
}

const BIT: u32 = 16;
/// Costs of symbols a histogram doesn't have: rare, but not out of the question.
const LIT_NOSTAT: u32 = 11;
const LEN_NOSTAT: u32 = 13;
const DIST_NOSTAT: u32 = 10;

impl Costs {
    /// Costs from the code lengths a histogram would get.
    fn of(h: &Hist) -> Costs {
        let mut ll = [0u8; LITLEN_SYMS];
        let mut d = [0u8; DIST_SYMS];
        lengths_into(&h.ll, 15, &mut ll);
        lengths_into(&h.d, 15, &mut d);
        let or = |l: u8, none: u32| if l == 0 { none } else { l as u32 };
        let mut c = Costs { lit: [0; 256], len: [0; MAX_MATCH + 1], dist: [0; DIST_SYMS] };
        for i in 0..256 {
            c.lit[i] = or(ll[i], LIT_NOSTAT) * BIT;
        }
        for l in MIN_MATCH..=MAX_MATCH {
            let s = LEN_CODE[l] as usize;
            c.len[l] = (or(ll[257 + s], LEN_NOSTAT) + LEN_EXTRA[s] as u32) * BIT;
        }
        for k in 0..DIST_SYMS {
            c.dist[k] = (or(d[k], DIST_NOSTAT) + DIST_EXTRA[k] as u32) * BIT;
        }
        c
    }
}

/// Working arrays of the parse.
#[derive(Default)]
struct Parse {
    cost: Vec<u32>,
    choice: Vec<(u16, u16)>,
    syms: Vec<Sym>,
    prev: Option<Hist>,
}

/// The cheapest path through `buf[a..b]` under `c`, into `o.syms`; returns its histogram. `counts` has the
/// number of candidate matches of each position, `cands` the candidates, position after position.
#[allow(clippy::too_many_arguments)]
fn parse(
    buf: &[u8],
    a: usize,
    b: usize,
    counts: &[u8],
    cands: &[(u16, u16)],
    c: &Costs,
    nice: usize,
    o: &mut Parse,
) -> Hist {
    let n = b - a;
    o.cost.clear();
    o.cost.resize(n + 1, 0);
    o.choice.clear();
    o.choice.resize(n, (1, 0));
    let mut k = cands.len();
    // The cost from the position after this one (kept at hand rather than read back).
    let mut next = 0;
    // From the end back: the cheapest way to finish from each position (among equals the shortest step).
    for i in (0..n).rev() {
        let cnt = counts[i] as usize;
        k -= cnt;
        let mut best = next + c.lit[buf[a + i] as usize];
        let mut pick = (1u16, 0u16);
        let room = (n - i).min(MAX_MATCH);
        if cnt > 0 && room >= MIN_MATCH {
            // Each length uses the closest candidate that reaches it.
            let mut l = MIN_MATCH;
            let ahead = &o.cost[i..=i + room];
            for &(ml, md) in &cands[k..k + cnt] {
                let top = (ml as usize).min(room);
                if top < l {
                    break;
                }
                let dc = c.dist[dist_code(md as usize)];
                // Past `nice` only the full length: the positions it covers were not searched, so stopping
                // short of its end could only continue with literals.
                let upto = top.min(nice.max(l));
                let mut bl = 0;
                for (j, (&lc, &rest)) in c.len[l..=upto].iter().zip(&ahead[l..=upto]).enumerate() {
                    let x = lc + dc + rest;
                    if x < best {
                        best = x;
                        bl = l + j;
                    }
                }
                if top > upto {
                    let x = c.len[top] + dc + ahead[top];
                    if x < best {
                        best = x;
                        bl = top;
                    }
                }
                if bl != 0 {
                    pick = (bl as u16, md);
                }
                l = top + 1;
            }
        }
        o.cost[i] = best;
        o.choice[i] = pick;
        next = best;
    }
    o.syms.clear();
    let mut h = Hist::empty();
    let mut i = 0;
    while i < n {
        let (l, d) = o.choice[i];
        if d == 0 {
            let x = buf[a + i];
            o.syms.push(Sym::lit(x));
            h.ll[x as usize] += 1;
            i += 1;
        } else {
            o.syms.push(Sym::mat(l as usize, d as usize));
            count_match(&mut h, l as usize, d as usize);
            i += l as usize;
        }
    }
    h
}

/// Size in bits of a block with these counts (the cheaper of dynamic and fixed).
fn block_bits(h: &Hist) -> u64 {
    dynamic_code(h).bits.min(fixed_bits(h))
}

/// Parses `buf[a..b]` (passes as the level says) and writes it.
#[allow(clippy::too_many_arguments)]
fn optimal_block(
    seg: &mut Seg,
    o: &mut Parse,
    a: usize,
    b: usize,
    counts: &[u8],
    cands: &[(u16, u16)],
    greedy: &Hist,
    lv: &Level,
    last: bool,
) {
    let buf = seg.buf;
    let (nice, passes) = (lv.nice, lv.passes);
    // The first costs: the greedy pass's counts, with those of the previous block's path when there is one
    // (a much better start, since a path's costs settle only over several passes).
    let first = match &o.prev {
        Some(p) => Costs::of(&blend(greedy, p)),
        None => Costs::of(greedy),
    };
    let mut h = parse(buf, a, b, counts, cands, &first, nice, o);
    if passes > 1 {
        let mut bits = block_bits(&h);
        let mut syms = std::mem::take(&mut o.syms);
        for _ in 1..passes {
            let h2 = parse(buf, a, b, counts, cands, &Costs::of(&h), nice, o);
            let bits2 = block_bits(&h2);
            if bits2 >= bits {
                break;
            }
            bits = bits2;
            h = h2;
            std::mem::swap(&mut syms, &mut o.syms);
        }
        seg.block(&syms, &h, a, b, last);
        o.syms = syms;
    } else {
        seg.block(&o.syms, &h, a, b, last);
    }
    o.prev = Some(h);
}

/// `g` plus `p` scaled to the same number of literal/length symbols.
fn blend(g: &Hist, p: &Hist) -> Hist {
    let tg: u64 = g.ll.iter().map(|&x| x as u64).sum();
    let tp: u64 = p.ll.iter().map(|&x| x as u64).sum::<u64>().max(1);
    let mut h = g.clone();
    for (x, &y) in h.ll.iter_mut().zip(&p.ll) {
        *x += (y as u64 * tg / tp) as u32;
    }
    for (x, &y) in h.d.iter_mut().zip(&p.d) {
        *x += (y as u64 * tg / tp) as u32;
    }
    h
}

/// Levels 8 to 12: candidate matches from binary trees, then the cheapest path through each block.
fn optimal(seg: &mut Seg, lv: &Level) {
    let buf = seg.buf;
    let end = buf.len();
    let stop = end.saturating_sub(3);
    let nice = lv.nice;
    let mut mf = Tree::new(end);
    let mut cands: Vec<(u16, u16)> = Vec::with_capacity(CAND_CAP.min(4 * (end - seg.start)) + MAX_CANDS);
    for q in 0..seg.start.min(stop) {
        mf.walk::<false>(buf, q, (end - q).min(MAX_MATCH), nice, lv.depth, &mut cands);
    }
    let mut counts: Vec<u8> = Vec::with_capacity((end - seg.start).min(MAX_OPT_BLOCK) + MAX_MATCH);
    let mut o = Parse::default();
    let mut split = Split::new();
    // Counts of a greedy parse (before the mark, end-of-block included, and since): the first costs.
    let (mut seen, mut now) = (Hist::empty(), zero_hist());
    let mut start = seg.start;
    let (mut mark, mut mark_k) = (start, 0usize);
    // The greedy pass's next position and its last two distances.
    let mut next = start;
    let mut reps = [0usize; 2];
    let mut p = start;
    while p < end {
        let mut step = 1;
        if p < stop {
            let max = (end - p).min(MAX_MATCH);
            let k0 = cands.len();
            let mut best = mf.walk::<true>(buf, p, max, nice, lv.depth, &mut cands);
            // The greedy pass's last distances often come back (rows of an image, columns of a table): a
            // match there longer than the trees found is kept too.
            for r in reps {
                if r != 0 && r <= p && best < max {
                    let l = common(buf, p - r, p, max);
                    if l > best && l >= MIN_MATCH {
                        if cands.len() - k0 < MAX_CANDS {
                            cands.push((l as u16, r as u16));
                        } else if let Some(e) = cands.last_mut() {
                            *e = (l as u16, r as u16);
                        }
                        best = l;
                    }
                }
            }
            counts.push((cands.len() - k0) as u8);
            if p >= next {
                let d = cands.last().map_or(0, |e| e.1 as usize);
                if best > MIN_MATCH || (best == MIN_MATCH && d <= STATS_FAR3) {
                    count_match(&mut now, best, d);
                    split.mat(best);
                    next = p + best;
                    if d != reps[0] {
                        reps = [d, reps[0]];
                    }
                } else {
                    now.ll[buf[p] as usize] += 1;
                    split.lit(buf[p]);
                    next = p + 1;
                }
            }
            if best >= nice.min(max) {
                // Long enough: the positions it covers are added to the trees but not searched.
                for q in p + 1..p + best {
                    if q < stop {
                        mf.walk::<false>(buf, q, (end - q).min(MAX_MATCH), nice, lv.depth, &mut cands);
                    }
                    counts.push(0);
                }
                step = best;
            }
        } else {
            counts.push(0);
            if p >= next {
                now.ll[buf[p] as usize] += 1;
                split.lit(buf[p]);
                next = p + 1;
            }
        }
        p += step;
        let full = (p - start >= MAX_OPT_BLOCK || cands.len() + MAX_CANDS > CAND_CAP) && p < end;
        if split.n_now >= BATCH || full {
            if full {
                take_into(&mut seen, &mut now);
                optimal_block(seg, &mut o, start, p, &counts, &cands, &seen, lv, false);
                counts.clear();
                cands.clear();
                seen = Hist::empty();
                split = Split::new();
                start = p;
            } else if mark - start >= MIN_BLOCK && end - mark >= MIN_BLOCK && split.changed(mark - start) {
                optimal_block(seg, &mut o, start, mark, &counts[..mark - start], &cands[..mark_k], &seen, lv, false);
                counts.drain(..mark - start);
                cands.drain(..mark_k);
                seen = std::mem::replace(&mut now, zero_hist());
                seen.ll[END_OF_BLOCK] = 1;
                split.restart();
                start = mark;
            } else {
                take_into(&mut seen, &mut now);
                split.merge();
            }
            mark = p;
            mark_k = cands.len();
        }
    }
    if start < end || seg.last {
        take_into(&mut seen, &mut now);
        optimal_block(seg, &mut o, start, end, &counts, &cands, &seen, lv, seg.last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flate::inflate;
    use std::time::Duration;

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    fn random(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n).map(|_| (rng(&mut s) >> 11) as u8).collect()
    }

    /// Text, runs, small-alphabet noise and random bytes in turn, changing every few thousand bytes.
    fn mixed(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        let text = b"BT /F1 12 Tf 72 712 Td [(Hello)-250(World)]TJ ET q 0.5 0 0 0.5 0 0 cm /Im1 Do Q\n";
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let len = 500 + (rng(&mut s) % 6000) as usize;
            match rng(&mut s) % 5 {
                0 => {
                    for k in 0..len {
                        out.push(text[k % text.len()].wrapping_add(rng(&mut s).is_multiple_of(23) as u8));
                    }
                }
                1 => out.extend(std::iter::repeat_n((rng(&mut s) % 4) as u8 * 85, len)),
                2 => {
                    for _ in 0..len {
                        out.push(b'a' + (rng(&mut s) % 3) as u8);
                    }
                }
                3 => {
                    for _ in 0..len {
                        out.push((rng(&mut s) >> 17) as u8);
                    }
                }
                _ => {
                    // Copies from far back.
                    let from = out.len().saturating_sub(1 + (rng(&mut s) % 30_000) as usize);
                    for k in 0..len.min(out.len() - from) {
                        out.push(out[from + k]);
                    }
                    out.push(7);
                }
            }
        }
        out.truncate(n);
        out
    }

    fn samples() -> Vec<Vec<u8>> {
        let text = b"stream of text, more text, and text again; 0 0 612 792 re W n\n".repeat(900);
        vec![
            vec![],
            vec![42],
            vec![0, 0],
            b"abcabcabc".to_vec(),
            vec![0u8; 1000],
            vec![7u8; 200_000],
            [vec![1u8; 70_000], vec![2u8; 70_000], vec![1u8; 70_000]].concat(),
            text,
            random(100_000, 3),
            mixed(300_000, 5),
        ]
    }

    fn round_trip(d: &[u8], level: u8, pool: Option<&Pool>) -> Vec<u8> {
        let z = deflate(d, level, pool, Deadline::NONE);
        let back = inflate(&z, d.len() + 1).unwrap_or_else(|e| panic!("level {level}, {} bytes: {e:?}", d.len()));
        assert!(back.data == d, "level {level}, {} bytes: wrong data", d.len());
        assert_eq!(back.consumed, z.len(), "level {level}");
        z
    }

    #[test]
    fn round_trips_at_every_level() {
        for d in samples() {
            let mut sizes = Vec::new();
            for level in 1..=LEVELS {
                sizes.push(round_trip(&d, level, None).len());
            }
            if d.len() > 10_000 {
                // Stronger levels are never much larger.
                assert!(sizes[LEVELS as usize - 1] <= sizes[0] + 16, "{sizes:?}");
            }
        }
        // Out-of-range levels are clamped.
        let d = mixed(20_000, 9);
        assert_eq!(deflate(&d, 0, None, Deadline::NONE), deflate(&d, 1, None, Deadline::NONE));
        assert_eq!(deflate(&d, 200, None, Deadline::NONE), deflate(&d, LEVELS, None, Deadline::NONE));
    }

    #[test]
    fn round_trips_over_several_segments() {
        let n = SEGMENT * 7 / 2;
        let d = mixed(n, 11);
        for level in [1, 6, 10] {
            let z = round_trip(&d, level, None);
            assert!(z.len() < n * 3 / 4);
        }
        // Random data across segments: stored blocks joined over segment ends.
        let r = random(SEGMENT * 2 + 1000, 13);
        let z = round_trip(&r, 4, None);
        assert!(z.len() <= r.len() + 5 * r.len().div_ceil(65535));
        let zeros = vec![0u8; SEGMENT * 3 + 5];
        assert!(round_trip(&zeros, 12, None).len() < 10_000);
    }

    #[test]
    fn every_size_round_trips() {
        let d = mixed(70_001, 21);
        // Every size in an optimised build; a stride keeps debug builds quick.
        let stride = if cfg!(debug_assertions) { 997 } else { 1 };
        let pool = Pool::new(4);
        // A fast level and the first optimal one (the most involved block handling).
        for level in [1, 8] {
            par::map(Some(&pool), 16, None, |c| {
                for n in (c * stride..=70_000).step_by(16 * stride) {
                    round_trip(&d[..n], level, None);
                }
            });
        }
        // Every other level: every size up to 300, then one size in 97.
        par::map(Some(&pool), LEVELS as usize, None, |i| {
            let level = i as u8 + 1;
            if level != 1 && level != 8 {
                for n in (0..=300).chain((301..=70_000).step_by(97 * stride)) {
                    round_trip(&d[..n], level, None);
                }
            }
        });
    }

    #[test]
    fn sizes_around_segment_ends_round_trip() {
        let d = mixed(2 * SEGMENT + 9, 51);
        for n in [SEGMENT - 1, SEGMENT, SEGMENT + 1, SEGMENT + 3, SEGMENT + 4, 2 * SEGMENT, 2 * SEGMENT + 9] {
            for level in [2, 8] {
                round_trip(&d[..n], level, None);
            }
        }
    }

    #[test]
    fn same_output_for_any_thread_count() {
        let d = mixed(SEGMENT * 2 + SEGMENT / 3, 31);
        for level in 1..=LEVELS {
            let want = deflate(&d, level, None, Deadline::NONE);
            assert_eq!(inflate(&want, d.len() + 1).unwrap().data, d);
            for t in [1, 2, 3, 8] {
                let pool = Pool::new(t);
                assert!(deflate(&d, level, Some(&pool), Deadline::NONE) == want, "level {level}, {t} threads");
            }
        }
    }

    #[test]
    fn a_passed_deadline_gives_stored_segments_that_decode() {
        let d = mixed(SEGMENT + 5000, 41);
        let gone = Deadline::after(Duration::ZERO);
        std::thread::sleep(Duration::from_millis(2));
        for level in [1, LEVELS] {
            let z = deflate(&d, level, None, gone);
            assert_eq!(inflate(&z, d.len() + 1).unwrap().data, d);
            assert!(z.len() <= d.len() + 5 * d.len().div_ceil(65535));
        }
        // A deadline that may pass halfway: coded segments, then stored ones; the stream decodes either way.
        let d = mixed(4 * SEGMENT + 3, 43);
        for ms in [1, 5, 20] {
            let z = deflate(&d, LEVELS, None, Deadline::after(Duration::from_millis(ms)));
            assert_eq!(inflate(&z, d.len() + 1).unwrap().data, d);
        }
    }

    #[test]
    fn incompressible_data_costs_only_the_stored_overhead() {
        for n in [1usize, 100, 65535, 65536, 300_000, 1_000_000] {
            let d = random(n, n as u64);
            for level in 1..=LEVELS {
                let z = round_trip(&d, level, None);
                assert!(z.len() <= n + 5 * n.div_ceil(65535), "level {level}, {n} bytes: {}", z.len());
            }
        }
        // A short last segment: at most 5 bytes more (see `Stream`), whatever its length and bytes.
        let tails = (1..=80).chain([100, 150, 200, 1000, 65519, 65535, 70_000]);
        for (k, tail) in tails.enumerate() {
            let n = SEGMENT + tail;
            let d = random(n, 1 + k as u64 % 3);
            for level in [1, LEVELS] {
                let z = round_trip(&d, level, None);
                assert!(z.len() <= n + 5 * n.div_ceil(65535) + 5, "level {level}, {n} bytes: {}", z.len());
            }
        }
    }

    #[test]
    fn splicing_matches_writing_bit_by_bit() {
        let mut s = 77u64;
        for _ in 0..200 {
            let lead = (rng(&mut s) % 20) as u32;
            let nbits = rng(&mut s) % 300;
            let src: Vec<u8> = (0..nbits.div_ceil(8) + (rng(&mut s) % 2)).map(|_| rng(&mut s) as u8).collect();
            let mut a = BitWriter::new();
            let mut b = BitWriter::new();
            for i in 0..lead {
                a.put(i & 1, 1);
                b.put(i & 1, 1);
            }
            splice(&mut a, &src, nbits);
            for i in 0..nbits {
                b.put(bits_at(&src, i, 1), 1);
            }
            assert_eq!(a.bit_len(), b.bit_len());
            assert_eq!(a.finish(), b.finish());
        }
    }
}
