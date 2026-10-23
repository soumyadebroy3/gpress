//! A bit-exact re-creation of zlib's deflate algorithm (levels 1-9, any window size, memory level and
//! strategy except RLE/HUFFMAN_ONLY). It exists to rebuild the original compressed bytes of PDF streams that
//! zlib wrote, so the gpress archive can store such streams decoded (where they compress far better) and still
//! give back the original file byte for byte.
//!
//! To give the same bytes it follows zlib's deflate.c and trees.c step for step: their configuration table,
//! match search, lazy matching, Huffman tree construction and choice of block type, under zlib's names. It is
//! an altered version of that code, not the original, and keeps zlib's licence notice:
//!
//! Copyright (C) 1995-2024 Jean-loup Gailly and Mark Adler
//!
//! This software is provided 'as-is', without any express or implied warranty. In no event will the authors
//! be held liable for any damages arising from the use of this software.
//!
//! Permission is granted to anyone to use this software for any purpose, including commercial applications,
//! and to alter it and redistribute it freely, subject to the following restrictions:
//!
//! 1. The origin of this software must not be misrepresented; you must not claim that you wrote the original
//!    software. If you use this software in a product, an acknowledgment in the product documentation would
//!    be appreciated but is not required.
//! 2. Altered source versions must be plainly marked as such, and must not be misrepresented as being the
//!    original software.
//! 3. This notice may not be removed or altered from any source distribution.

use super::bits::BitWriter;
use super::inflate::first_block_symbols;
use super::tables::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub level: u8,
    pub window_bits: u8,
    pub mem_level: u8,
    /// zlib's Z_FILTERED strategy.
    pub filtered: bool,
}

struct Config {
    good: usize,
    lazy: usize,
    nice: usize,
    chain: usize,
    fast: bool,
}

fn config(level: u8) -> Config {
    let (good, lazy, nice, chain, fast) = match level {
        1 => (4, 4, 8, 4, true),
        2 => (4, 5, 16, 8, true),
        3 => (4, 6, 32, 32, true),
        4 => (4, 4, 16, 16, false),
        5 => (8, 16, 32, 32, false),
        6 => (8, 16, 128, 128, false),
        7 => (8, 32, 128, 256, false),
        8 => (32, 128, 258, 1024, false),
        _ => (32, 258, 258, 4096, false),
    };
    Config { good, lazy, nice, chain, fast }
}

const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const TOO_FAR: usize = 4096;
const NIL: usize = usize::MAX;

/// Symbols of one block: (dist, len_or_lit). dist 0 = literal.
struct Tally {
    syms: Vec<(u16, u16)>,
    lit_freq: [u32; 286],
    dist_freq: [u32; 30],
    limit: usize,
}

impl Tally {
    fn new(limit: usize) -> Tally {
        let mut t = Tally { syms: Vec::with_capacity(limit), lit_freq: [0; 286], dist_freq: [0; 30], limit };
        t.lit_freq[256] = 1;
        t
    }
    fn reset(&mut self) {
        self.syms.clear();
        self.lit_freq = [0; 286];
        self.dist_freq = [0; 30];
        self.lit_freq[256] = 1;
    }
    #[inline]
    fn lit(&mut self, b: u8) -> bool {
        self.syms.push((0, b as u16));
        self.lit_freq[b as usize] += 1;
        self.syms.len() == self.limit
    }
    #[inline]
    fn dist(&mut self, dist: usize, len: usize) -> bool {
        self.syms.push((dist as u16, len as u16));
        self.lit_freq[257 + LEN_CODE[len] as usize] += 1;
        self.dist_freq[dist_code(dist)] += 1;
        self.syms.len() == self.limit
    }
}

struct State<'a> {
    d: &'a [u8],
    p: Params,
    cfg: Config,
    w_size: usize,
    max_dist: usize,
    hash_bits: u32,
    hash_mask: usize,
    hash_shift: u32,
    head: Vec<usize>,
    prev: Vec<usize>,
    ins_h: usize,
    strstart: usize,
    lookahead: usize,
    base: usize, // absolute position of window index 0
    match_start: usize,
    prev_length: usize,
    out: BitWriter,
    block_start: usize,
    /// End (absolute) of the input read into the window so far.
    read_upto: usize,
    /// When rebuilding known bytes: they, how many were compared so far, and whether a difference was seen.
    target: Option<&'a [u8]>,
    checked: usize,
    differs: bool,
}

impl<'a> State<'a> {
    #[inline]
    fn byte(&self, q: usize) -> u8 {
        if q < self.d.len() {
            self.d[q]
        } else if self.base > 0 {
            // Past the input, zlib's window still holds the bytes it held one window earlier.
            self.d[q - self.w_size]
        } else {
            0
        }
    }
    #[inline]
    fn update_hash(&mut self, c: u8) {
        self.ins_h = ((self.ins_h << self.hash_shift) ^ c as usize) & self.hash_mask;
    }
    #[inline]
    fn valid(&self, pos: usize) -> bool {
        // Window index 0 doubles as "no position" in zlib.
        pos != NIL && pos > self.base
    }
    /// INSERT_STRING: returns the previous head.
    #[inline]
    fn insert(&mut self, s: usize) -> usize {
        let c = self.byte(s + MIN_MATCH - 1);
        self.update_hash(c);
        let h = self.head[self.ins_h];
        let h = if self.valid(h) { h } else { NIL };
        self.prev[s & (self.w_size - 1)] = h;
        self.head[self.ins_h] = s;
        h
    }
    fn fill(&mut self) {
        // Slide when the current position is far enough into the window (zlib does this even when no
        // input is left).
        if self.strstart - self.base >= self.w_size + self.max_dist {
            self.base += self.w_size;
        }
        let end = self.d.len().min(self.base + 2 * self.w_size);
        if end > self.read_upto {
            // zlib re-primes the hash only after reading new input.
            self.read_upto = end;
            self.lookahead = end - self.strstart;
            if self.lookahead >= MIN_MATCH {
                self.ins_h = self.d[self.strstart] as usize;
                let c = self.d[self.strstart + 1];
                self.update_hash(c);
            }
        } else {
            self.lookahead = end - self.strstart;
        }
    }

    fn longest_match(&mut self, mut cur: usize) -> usize {
        let mut chain = self.cfg.chain;
        let scan = self.strstart;
        let mut best = self.prev_length;
        let mut nice = self.cfg.nice;
        let limit = if scan - self.base > self.max_dist { scan - self.max_dist } else { self.base };
        if self.prev_length >= self.cfg.good {
            chain >>= 2;
        }
        if nice > self.lookahead {
            nice = self.lookahead;
        }
        let mut scan_end1 = self.byte(scan + best - 1);
        let mut scan_end = self.byte(scan + best);
        loop {
            let m = cur;
            if !(self.byte(m + best) != scan_end
                || self.byte(m + best - 1) != scan_end1
                || self.byte(m) != self.byte(scan)
                || self.byte(m + 1) != self.byte(scan + 1))
            {
                // Bytes 0, 1 match; byte 2 matches by the hash; compare from 3 up to 258.
                let mut len = 3;
                while len < MAX_MATCH && self.byte(scan + len) == self.byte(m + len) {
                    len += 1;
                }
                if len > best {
                    self.match_start = cur;
                    best = len;
                    if len >= nice {
                        break;
                    }
                    scan_end1 = self.byte(scan + best - 1);
                    scan_end = self.byte(scan + best);
                }
            }
            cur = self.prev[cur & (self.w_size - 1)];
            if cur == NIL || !(cur > limit) || !self.valid(cur) {
                break;
            }
            chain -= 1;
            if chain == 0 {
                break;
            }
        }
        best.min(self.lookahead)
    }

    fn flush(&mut self, t: &mut Tally, last: bool) {
        let stored_len = self.strstart - self.block_start;
        let block = &self.d[self.block_start..self.strstart];
        // zlib can only store a block whose start is still in its window.
        let storable = self.block_start >= self.base;
        flush_block(&mut self.out, t, block, stored_len, last, storable);
        self.block_start = self.strstart;
        t.reset();
        // Compare the whole bytes written so far with the target: the first difference ends the attempt.
        if let Some(target) = self.target {
            let done = &self.out.out;
            if done.len() > target.len() || done[self.checked..] != target[self.checked..done.len()] {
                self.differs = true;
            }
            self.checked = done.len();
        }
    }
}

/// Compresses `d` exactly as zlib would with these parameters (raw Deflate data, no zlib wrapper).
pub fn deflate_like_zlib(d: &[u8], p: Params) -> Vec<u8> {
    run_zlib(d, p, None).expect("no target to differ from")
}

/// Whether `d` compressed with these parameters gives exactly `target` (raw Deflate data). Stops at the
/// first block that differs, so a wrong guess costs little.
pub fn rebuilds(d: &[u8], p: Params, target: &[u8]) -> bool {
    run_zlib(d, p, Some(target)).is_some_and(|z| z == target)
}

/// The compressed data, or None as soon as it is known to differ from `target`.
fn run_zlib(d: &[u8], p: Params, target: Option<&[u8]>) -> Option<Vec<u8>> {
    let w_size = 1usize << p.window_bits;
    let hash_bits = p.mem_level as u32 + 7;
    let mut s = State {
        d,
        p,
        cfg: config(p.level),
        w_size,
        max_dist: w_size - MIN_LOOKAHEAD,
        hash_bits,
        hash_mask: (1 << hash_bits) - 1,
        hash_shift: hash_bits.div_ceil(MIN_MATCH as u32),
        head: vec![NIL; 1 << hash_bits],
        prev: vec![NIL; w_size],
        ins_h: 0,
        strstart: 0,
        lookahead: 0,
        base: 0,
        match_start: 0,
        prev_length: MIN_MATCH - 1,
        // Reserved up front: half the input, but at most 4 MiB (the vector grows with what is written, so
        // a large part costs no more than its output).
        out: BitWriter::with_capacity((d.len() / 2 + 64).min(1 << 22)),
        block_start: 0,
        read_upto: 0,
        target,
        checked: 0,
        differs: false,
    };
    let _ = s.hash_bits;
    let mut t = Tally::new((1usize << (p.mem_level + 6)) - 1);
    if s.cfg.fast {
        run_fast(&mut s, &mut t);
    } else {
        run_slow(&mut s, &mut t);
    }
    if s.differs {
        return None;
    }
    s.flush(&mut t, true);
    Some(s.out.finish())
}

fn run_fast(s: &mut State, t: &mut Tally) {
    let mut match_length;
    loop {
        if s.lookahead < MIN_LOOKAHEAD {
            s.fill();
            if s.lookahead == 0 {
                break;
            }
        }
        let mut hash_head = NIL;
        if s.lookahead >= MIN_MATCH {
            hash_head = s.insert(s.strstart);
        }
        match_length = 0;
        if hash_head != NIL && s.strstart - hash_head <= s.max_dist {
            match_length = s.longest_match(hash_head);
        }
        let bflush;
        if match_length >= MIN_MATCH {
            bflush = t.dist(s.strstart - s.match_start, match_length);
            s.lookahead -= match_length;
            if match_length <= s.cfg.lazy && s.lookahead >= MIN_MATCH {
                match_length -= 1;
                loop {
                    s.strstart += 1;
                    s.insert(s.strstart);
                    match_length -= 1;
                    if match_length == 0 {
                        break;
                    }
                }
                s.strstart += 1;
            } else {
                s.strstart += match_length;
                s.ins_h = s.byte(s.strstart) as usize;
                let c = s.byte(s.strstart + 1);
                s.update_hash(c);
            }
        } else {
            bflush = t.lit(s.d[s.strstart]);
            s.lookahead -= 1;
            s.strstart += 1;
        }
        if bflush {
            s.flush(t, false);
            if s.differs {
                return;
            }
        }
    }
}

fn run_slow(s: &mut State, t: &mut Tally) {
    let mut match_available = false;
    let mut match_length = MIN_MATCH - 1;
    let mut prev_match;
    loop {
        if s.lookahead < MIN_LOOKAHEAD {
            s.fill();
            if s.lookahead == 0 {
                break;
            }
        }
        let mut hash_head = NIL;
        if s.lookahead >= MIN_MATCH {
            hash_head = s.insert(s.strstart);
        }
        s.prev_length = match_length;
        prev_match = s.match_start;
        match_length = MIN_MATCH - 1;
        if hash_head != NIL && s.prev_length < s.cfg.lazy && s.strstart - hash_head <= s.max_dist {
            match_length = s.longest_match(hash_head);
            if match_length <= 5
                && (s.p.filtered || (match_length == MIN_MATCH && s.strstart - s.match_start > TOO_FAR))
            {
                match_length = MIN_MATCH - 1;
            }
        }
        if s.prev_length >= MIN_MATCH && match_length <= s.prev_length {
            let max_insert = s.strstart + s.lookahead - MIN_MATCH;
            let bflush = t.dist(s.strstart - 1 - prev_match, s.prev_length);
            s.lookahead -= s.prev_length - 1;
            let mut n = s.prev_length - 2;
            loop {
                s.strstart += 1;
                if s.strstart <= max_insert {
                    s.insert(s.strstart);
                }
                n -= 1;
                if n == 0 {
                    break;
                }
            }
            match_available = false;
            match_length = MIN_MATCH - 1;
            s.strstart += 1;
            if bflush {
                s.flush(t, false);
                if s.differs {
                    return;
                }
            }
        } else if match_available {
            let bflush = t.lit(s.d[s.strstart - 1]);
            if bflush {
                s.flush(t, false);
                if s.differs {
                    return;
                }
            }
            s.strstart += 1;
            s.lookahead -= 1;
        } else {
            match_available = true;
            s.strstart += 1;
            s.lookahead -= 1;
        }
    }
    if match_available {
        t.lit(s.d[s.strstart - 1]);
    }
}

// ---------------------------------------------------------------- zlib's trees

struct Tree {
    freq: Vec<u32>,
    len: Vec<u16>,
    dad: Vec<u16>,
    code: Vec<u16>,
    max_code: isize,
}

/// zlib's Huffman construction (heap with depth tie-break, then the overflow fix). Returns (tree, opt bits,
/// static bits) where the bit counts include extra bits.
fn build_tree(
    freqs: &[u32],
    extra: &dyn Fn(usize) -> u32,
    static_len: Option<&dyn Fn(usize) -> u32>,
    max_length: u16,
    opt: &mut u64,
    stat: &mut u64,
) -> Tree {
    let elems = freqs.len();
    let nodes = 2 * elems + 1;
    let mut freq = vec![0u32; nodes];
    freq[..elems].copy_from_slice(freqs);
    let mut len = vec![0u16; nodes + 1];
    let mut dad = vec![0u16; nodes];
    let mut depth = vec![0u8; nodes];
    let heap_size = 2 * 286 + 1;
    let mut heap = vec![0usize; heap_size + 1];
    let mut heap_len = 0usize;
    let mut heap_max = heap_size;
    let mut max_code: isize = -1;
    for n in 0..elems {
        if freq[n] != 0 {
            heap_len += 1;
            heap[heap_len] = n;
            max_code = n as isize;
            depth[n] = 0;
        }
    }
    while heap_len < 2 {
        let node = if max_code < 2 {
            max_code += 1;
            max_code as usize
        } else {
            0
        };
        heap_len += 1;
        heap[heap_len] = node;
        freq[node] = 1;
        depth[node] = 0;
        *opt = opt.wrapping_sub(1);
        if let Some(sl) = static_len {
            *stat = stat.wrapping_sub(sl(node) as u64);
        }
    }
    let smaller = |freq: &Vec<u32>, depth: &Vec<u8>, n: usize, m: usize| {
        freq[n] < freq[m] || (freq[n] == freq[m] && depth[n] <= depth[m])
    };
    let down = |heap: &mut Vec<usize>, heap_len: usize, freq: &Vec<u32>, depth: &Vec<u8>, mut k: usize| {
        let v = heap[k];
        let mut j = k << 1;
        while j <= heap_len {
            if j < heap_len && smaller(freq, depth, heap[j + 1], heap[j]) {
                j += 1;
            }
            if smaller(freq, depth, v, heap[j]) {
                break;
            }
            heap[k] = heap[j];
            k = j;
            j <<= 1;
        }
        heap[k] = v;
    };
    for n in (1..=heap_len / 2).rev() {
        down(&mut heap, heap_len, &freq, &depth, n);
    }
    let mut node = elems;
    loop {
        let n = heap[1];
        heap[1] = heap[heap_len];
        heap_len -= 1;
        down(&mut heap, heap_len, &freq, &depth, 1);
        let m = heap[1];
        heap_max -= 1;
        heap[heap_max] = n;
        heap_max -= 1;
        heap[heap_max] = m;
        freq[node] = freq[n] + freq[m];
        depth[node] = (depth[n].max(depth[m])) + 1;
        dad[n] = node as u16;
        dad[m] = node as u16;
        heap[1] = node;
        node += 1;
        down(&mut heap, heap_len, &freq, &depth, 1);
        if heap_len < 2 {
            break;
        }
    }
    heap_max -= 1;
    heap[heap_max] = heap[1];

    // gen_bitlen
    let mut bl_count = [0u16; 16];
    let mut overflow = 0i32;
    len[heap[heap_max]] = 0;
    let mut h = heap_max + 1;
    while h < heap_size {
        let n = heap[h];
        let mut bits = len[dad[n] as usize] + 1;
        if bits > max_length {
            bits = max_length;
            overflow += 1;
        }
        len[n] = bits;
        h += 1;
        if n as isize > max_code {
            continue;
        }
        bl_count[bits as usize] += 1;
        let xb = extra(n);
        let f = freq[n] as u64;
        *opt = opt.wrapping_add(f * (bits as u64 + xb as u64));
        if let Some(sl) = static_len {
            *stat = stat.wrapping_add(f * (sl(n) as u64 + xb as u64));
        }
    }
    if overflow > 0 {
        loop {
            let mut bits = max_length as usize - 1;
            while bl_count[bits] == 0 {
                bits -= 1;
            }
            bl_count[bits] -= 1;
            bl_count[bits + 1] += 2;
            bl_count[max_length as usize] -= 1;
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }
        let mut h = heap_size;
        for bits in (1..=max_length as usize).rev() {
            let mut n = bl_count[bits];
            while n != 0 {
                h -= 1;
                let m = heap[h];
                if m as isize > max_code {
                    continue;
                }
                if len[m] as usize != bits {
                    *opt = opt.wrapping_add((bits as u64).wrapping_sub(len[m] as u64).wrapping_mul(freq[m] as u64));
                    len[m] = bits as u16;
                }
                n -= 1;
            }
        }
    }
    // gen_codes
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for bits in 1..16 {
        code = (code + bl_count[bits - 1]) << 1;
        next[bits] = code;
    }
    let mut codes = vec![0u16; elems];
    for n in 0..=max_code.max(-1) as usize {
        if max_code < 0 {
            break;
        }
        let l = len[n];
        if l == 0 {
            continue;
        }
        codes[n] = super::huffman::reverse_bits(next[l as usize], l as u32);
        next[l as usize] += 1;
    }
    let lens: Vec<u16> = len[..elems].to_vec();
    Tree { freq: freq[..elems].to_vec(), len: lens, dad: Vec::new(), code: codes, max_code }
}

const BL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// zlib's run-length scan of a tree's lengths (counts for the bit-length tree, or the codes to send).
fn scan_tree(len: &[u16], max_code: isize, mut emit: impl FnMut(u16, u16)) {
    // emit(symbol, repeat count for 16/17/18 or 0)
    let mut prevlen: i32 = -1;
    let mut nextlen = len[0] as i32;
    let mut count = 0;
    let (mut max_count, mut min_count) = if nextlen == 0 { (138, 3) } else { (7, 4) };
    for n in 0..=max_code as usize {
        let curlen = nextlen;
        nextlen = if n < max_code as usize { len[n + 1] as i32 } else { 0xffff };
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        } else if count < min_count {
            for _ in 0..count {
                emit(curlen as u16, 0);
            }
        } else if curlen != 0 {
            if curlen != prevlen {
                emit(curlen as u16, 0);
                count -= 1;
            }
            emit(16, count as u16);
        } else if count <= 10 {
            emit(17, count as u16);
        } else {
            emit(18, count as u16);
        }
        count = 0;
        prevlen = curlen;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        } else if curlen == nextlen {
            max_count = 6;
            min_count = 3;
        } else {
            max_count = 7;
            min_count = 4;
        }
    }
}

fn flush_block(w: &mut BitWriter, t: &Tally, block: &[u8], stored_len: usize, last: bool, storable: bool) {
    let mut opt: u64 = 0;
    let mut stat: u64 = 0;
    let fixed_ll = fixed_litlen_lengths();
    let ll_extra = |n: usize| if n >= 257 { LEN_EXTRA[n - 257] as u32 } else { 0 };
    let d_extra = |n: usize| DIST_EXTRA[n] as u32;
    let st_ll = |n: usize| fixed_ll[n] as u32;
    let st_d = |_n: usize| 5u32;
    let lt = build_tree(&t.lit_freq, &ll_extra, Some(&st_ll), 15, &mut opt, &mut stat);
    let dt = build_tree(&t.dist_freq, &d_extra, Some(&st_d), 15, &mut opt, &mut stat);
    // Bit-length tree.
    let mut bl_freq = [0u32; 19];
    scan_tree(&lt.len, lt.max_code, |s, _| bl_freq[s as usize] += 1);
    scan_tree(&dt.len, dt.max_code, |s, _| bl_freq[s as usize] += 1);
    let bl_extra = |n: usize| match n {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    };
    let mut dummy = 0u64;
    let blt = build_tree(&bl_freq, &bl_extra, None, 7, &mut opt, &mut dummy);
    let mut max_blindex = 18;
    while max_blindex >= 3 && blt.len[BL_ORDER[max_blindex]] == 0 {
        max_blindex -= 1;
    }
    opt = opt.wrapping_add(3 * (max_blindex as u64 + 1) + 5 + 5 + 4);
    let mut opt_lenb = (opt.wrapping_add(3 + 7)) >> 3;
    let static_lenb = (stat.wrapping_add(3 + 7)) >> 3;
    if static_lenb <= opt_lenb {
        opt_lenb = static_lenb;
    }
    if storable && stored_len as u64 + 4 <= opt_lenb {
        w.put(last as u32, 1);
        w.put(0, 2);
        w.align();
        w.put(stored_len as u32 & 0xffff, 16);
        w.put(!(stored_len as u32) & 0xffff, 16);
        for &b in block {
            w.put(b as u32, 8);
        }
    } else if static_lenb == opt_lenb {
        w.put((1 << 1) | last as u32, 3);
        let fl = fixed_litlen_lengths();
        let fcodes = super::huffman::canonical_codes(&fl);
        let fd = fixed_dist_lengths();
        let dcodes = super::huffman::canonical_codes(&fd);
        let fl16: Vec<u16> = fl.iter().map(|&x| x as u16).collect();
        let fd16: Vec<u16> = fd.iter().map(|&x| x as u16).collect();
        compress_block(w, t, &fcodes, &fl16, &dcodes, &fd16);
    } else {
        w.put((2 << 1) | last as u32, 3);
        let lcodes = lt.max_code as usize + 1;
        let dcodes = dt.max_code as usize + 1;
        w.put((lcodes - 257) as u32, 5);
        w.put((dcodes - 1) as u32, 5);
        w.put((max_blindex + 1 - 4) as u32, 4);
        for rank in 0..=max_blindex {
            w.put(blt.len[BL_ORDER[rank]] as u32, 3);
        }
        for tree in [&lt, &dt] {
            scan_tree(&tree.len, tree.max_code, |s, c| {
                w.put(blt.code[s as usize] as u32, blt.len[s as usize] as u32);
                match s {
                    16 => w.put((c - 3) as u32, 2),
                    17 => w.put((c - 3) as u32, 3),
                    18 => w.put((c - 11) as u32, 7),
                    _ => {}
                }
            });
        }
        compress_block(w, t, &lt.code, &lt.len, &dt.code, &dt.len);
    }
    let _ = (&lt.freq, &dt.freq, &lt.dad);
}

fn compress_block(w: &mut BitWriter, t: &Tally, lc: &[u16], ll: &[u16], dc: &[u16], dl: &[u16]) {
    for &(dist, lv) in &t.syms {
        if dist == 0 {
            w.put(lc[lv as usize] as u32, ll[lv as usize] as u32);
        } else {
            let len = lv as usize;
            let c = LEN_CODE[len] as usize;
            w.put(lc[257 + c] as u32, ll[257 + c] as u32);
            if LEN_EXTRA[c] > 0 {
                w.put((len - LEN_BASE[c] as usize) as u32, LEN_EXTRA[c] as u32);
            }
            let k = dist_code(dist as usize);
            w.put(dc[k] as u32, dl[k] as u32);
            if DIST_EXTRA[k] > 0 {
                w.put((dist as usize - DIST_BASE[k] as usize) as u32, DIST_EXTRA[k] as u32);
            }
        }
    }
    w.put(lc[256] as u32, ll[256] as u32);
}

/// Finds zlib parameters that reproduce `raw` (a whole zlib stream) from `data`, if any. Tries the
/// likeliest settings first, guided by the zlib header.
pub fn find_params(data: &[u8], raw: &[u8]) -> Option<Params> {
    if raw.len() < 6 || raw[0] & 0x0f != 8 || raw[1] & 0x20 != 0 {
        return None;
    }
    let wb = (raw[0] >> 4) + 8;
    if !(9..=15).contains(&wb) {
        return None;
    }
    let flevel = raw[1] >> 6;
    let levels: &[u8] = match flevel {
        0 => &[1],
        1 => &[2, 3, 4, 5],
        2 => &[6],
        _ => &[9, 7, 8],
    };
    let body = &raw[2..raw.len() - 4];
    // zlib ends every block but the last after exactly 2^(mem_level + 6) - 1 symbols (the `Tally` limit, at
    // every level and strategy), so a first block that is neither stored nor the last leaves at most one
    // memory level to try: none when it holds any other count (counted up to 2^15 - 1, memory level 9's).
    // One that can't be read leaves them all.
    let first = first_block_symbols(body, (1 << 15) - 1).ok().flatten();
    for &mem in &[8u8, 9, 7, 6, 5, 4, 3, 2, 1] {
        if first.is_some_and(|n| n != (1 << (mem + 6)) - 1) {
            continue;
        }
        for &level in levels {
            for filtered in [false, true] {
                if filtered && level < 4 {
                    continue;
                }
                let p = Params { level, window_bits: wb, mem_level: mem, filtered };
                if rebuilds(data, p, body) {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// The full zlib stream for these parameters (header, data, Adler-32).
pub fn zlib_stream(data: &[u8], p: Params) -> Vec<u8> {
    let cmf = ((p.window_bits - 8) << 4) | 8;
    let flevel = if p.level < 2 {
        0
    } else if p.level < 6 {
        1
    } else if p.level == 6 {
        2
    } else {
        3
    };
    let mut hdr = ((cmf as u16) << 8) | ((flevel as u16) << 6);
    hdr += 31 - (hdr % 31);
    let mut out = vec![(hdr >> 8) as u8, hdr as u8];
    out.extend_from_slice(&deflate_like_zlib(data, p));
    out.extend_from_slice(&super::zlib::adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn reproduces_cpython_zlib_byte_for_byte() {
        // tests/data holds streams made by CPython's zlib at several levels (default and filtered strategy).
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let list = std::fs::read_to_string(dir.join("vectors.txt")).unwrap();
        let mut checked = 0;
        for line in list.lines() {
            let (z, raw) = line.split_once('\t').unwrap();
            // name-level-strategy-wbits
            let parts: Vec<&str> = z.trim_end_matches(".z").split('-').collect();
            let level: u8 = parts[1].parse().unwrap();
            let strat = parts[2];
            let wbits: u8 = parts[3].parse().unwrap();
            if level == 0 || !(strat == "def" || strat == "filt") {
                continue;
            }
            let zb = std::fs::read(dir.join(z)).unwrap();
            let data = std::fs::read(dir.join(raw)).unwrap();
            let p = Params {
                level,
                window_bits: wbits,
                mem_level: if level == 9 { 9 } else { 8 },
                filtered: strat == "filt",
            };
            let ours = zlib_stream(&data, p);
            assert!(ours == zb, "{z}: {} vs {} bytes", ours.len(), zb.len());
            checked += 1;
        }
        assert!(checked >= 40, "checked {checked}");
    }

    #[test]
    fn early_stop_agrees_with_full_encoding() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let mut data = std::fs::read(dir.join("mixed.raw")).unwrap();
        // Long enough for several blocks at the smaller memory levels.
        let text = std::fs::read(dir.join("text.raw")).unwrap();
        for k in 0..60 {
            data.extend_from_slice(&text);
            data.extend(std::iter::repeat_n((k * 7) as u8, k * 13));
        }
        let mut tried = 0;
        for level in [1u8, 4, 6, 9] {
            for mem in [1u8, 8] {
                let target =
                    deflate_like_zlib(&data, Params { level, window_bits: 15, mem_level: mem, filtered: false });
                for l2 in [1u8, 4, 6, 9] {
                    for m2 in [1u8, 8] {
                        let p = Params { level: l2, window_bits: 15, mem_level: m2, filtered: false };
                        assert_eq!(
                            rebuilds(&data, p, &target),
                            deflate_like_zlib(&data, p) == target,
                            "{level}/{mem} vs {p:?}"
                        );
                        tried += 1;
                    }
                }
                // A damaged target is never rebuilt, wherever the damage is.
                let p = Params { level, window_bits: 15, mem_level: mem, filtered: false };
                assert!(rebuilds(&data, p, &target));
                for pos in [0, target.len() / 3, target.len() - 1] {
                    let mut bad = target.clone();
                    bad[pos] ^= 0x10;
                    assert!(!rebuilds(&data, p, &bad));
                }
                assert!(!rebuilds(&data, p, &target[..target.len() - 1]));
                let mut longer = target.clone();
                longer.push(0);
                assert!(!rebuilds(&data, p, &longer));
            }
        }
        assert_eq!(tried, 64);
    }

    /// The search as it was before the first block's count cut it down: every memory level, in the same order.
    fn unpruned(data: &[u8], raw: &[u8]) -> Option<Params> {
        let wb = (raw[0] >> 4) + 8;
        if raw.len() < 6 || raw[0] & 0x0f != 8 || raw[1] & 0x20 != 0 || !(9..=15).contains(&wb) {
            return None;
        }
        let levels: &[u8] = match raw[1] >> 6 {
            0 => &[1],
            1 => &[2, 3, 4, 5],
            2 => &[6],
            _ => &[9, 7, 8],
        };
        for mem_level in [8u8, 9, 7, 6, 5, 4, 3, 2, 1] {
            for &level in levels {
                for filtered in [false, true] {
                    let p = Params { level, window_bits: wb, mem_level, filtered };
                    if !(filtered && level < 4) && rebuilds(data, p, &raw[2..raw.len() - 4]) {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    /// Every setting of ours: levels 1 to 9, the filtered strategy from level 4 (as find_params tries them).
    fn settings() -> impl Iterator<Item = (u8, bool)> {
        (1u8..=9).flat_map(|level| [(level, false), (level, true)]).filter(|&(level, f)| !f || level >= 4)
    }

    /// Letters from a 20-letter alphabet: mostly literals and short matches, so a block never ends up
    /// stored, and long enough for the first block not to be the last at `mem_level` and any level.
    fn letters(mem_level: u8) -> Vec<u8> {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let len = (1usize << (mem_level + 6)) * 3;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                b'a' + (s % 20) as u8
            })
            .collect()
    }

    #[test]
    fn a_first_block_that_is_not_the_last_holds_the_tally_limit() {
        // What lets find_params try one memory level: at every level, strategy, window size and memory level,
        // a first block that is neither stored nor the last holds exactly 2^(mem_level + 6) - 1 symbols.
        let mut seen = 0;
        for mem_level in 1u8..=9 {
            let data = letters(mem_level);
            for window_bits in 9u8..=15 {
                for (level, filtered) in settings() {
                    let p = Params { level, window_bits, mem_level, filtered };
                    let z = deflate_like_zlib(&data, p);
                    assert_eq!(first_block_symbols(&z, usize::MAX), Ok(Some((1 << (mem_level + 6)) - 1)), "{p:?}");
                    seen += 1;
                }
            }
        }
        assert_eq!(seen, 9 * 7 * 15);
    }

    #[test]
    fn the_cut_search_finds_what_the_full_search_finds() {
        // Streams of ours at every level, strategy and memory level: the one memory level left to try is the
        // right one, so the same parameters come out as from trying them all.
        for mem_level in 1u8..=9 {
            let data = letters(mem_level);
            for (level, filtered) in settings() {
                let p = Params { level, window_bits: 15, mem_level, filtered };
                let z = zlib_stream(&data, p);
                let found = find_params(&data, &z);
                assert_eq!(found, unpruned(&data, &z), "{p:?}");
                assert!(found.is_some_and(|q| q.mem_level == mem_level && zlib_stream(&data, q) == z), "{p:?}");
            }
        }
    }

    #[test]
    fn every_test_data_stream_gives_the_same_parameters() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let list = std::fs::read_to_string(dir.join("vectors.txt")).unwrap();
        let (mut streams, mut found, mut cut) = (0, 0, 0);
        for line in list.lines() {
            let (z, raw) = line.split_once('\t').unwrap();
            let zb = std::fs::read(dir.join(z)).unwrap();
            let data = std::fs::read(dir.join(raw)).unwrap();
            let u = super::super::zlib::unwrap(&zb, 1 << 24).unwrap();
            assert_eq!(u.data, data, "{z}");
            let whole = &zb[..zb.len() - u.trailing];
            let p = find_params(&data, whole);
            assert_eq!(p, unpruned(&data, whole), "{z}");
            streams += 1;
            found += p.is_some() as usize;
            cut += first_block_symbols(&whole[2..], usize::MAX).unwrap().is_some() as usize;
        }
        // The default and filtered streams (and a few others) are rebuilt; 59 streams have a first block that
        // is neither stored nor the last, so the cut is used.
        assert_eq!((streams, found, cut), (240, 109, 59));
    }

    /// A raw Deflate stream made by hand: a block with fixed codes holding `lits` (the last one when `last`),
    /// then, unless it is the last, an empty last block.
    fn fixed_block(lits: &[u8], last: bool) -> Vec<u8> {
        let lens = fixed_litlen_lengths();
        let codes = super::super::huffman::canonical_codes(&lens);
        let mut w = BitWriter::new();
        w.put(2 | last as u32, 3);
        for &b in lits {
            w.put(codes[b as usize] as u32, lens[b as usize] as u32);
        }
        w.put(codes[256] as u32, lens[256] as u32);
        if !last {
            w.put(3, 3);
            w.put(codes[256] as u32, lens[256] as u32);
        }
        w.finish()
    }

    /// The zlib stream (32K window, "default" level) around the raw Deflate stream `body` of `data`.
    fn wrapped(body: &[u8], data: &[u8]) -> Vec<u8> {
        let mut z = vec![0x78, 0x9c];
        z.extend_from_slice(body);
        z.extend_from_slice(&super::super::zlib::adler32(data).to_be_bytes());
        assert_eq!(super::super::zlib::unwrap(&z, 1 << 24).unwrap().data, data);
        z
    }

    #[test]
    fn first_blocks_made_by_hand() {
        use super::super::inflate::InflateError;
        let lits: Vec<u8> = (0..200u32).map(|i| (i * 7 % 251) as u8).collect();
        // Not the last, with fixed codes: counted, up to one past `max`.
        assert_eq!(first_block_symbols(&fixed_block(&lits, false), usize::MAX), Ok(Some(200)));
        assert_eq!(first_block_symbols(&fixed_block(&lits, false), 50), Ok(Some(51)));
        assert_eq!(first_block_symbols(&fixed_block(&[], false), usize::MAX), Ok(Some(0)));
        // The last block, or a stored one, is not counted.
        assert_eq!(first_block_symbols(&fixed_block(&lits, true), usize::MAX), Ok(None));
        let stored = [0x00, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c', 0x03, 0x00];
        assert_eq!(super::super::inflate(&stored, 100).unwrap().data, b"abc");
        assert_eq!(first_block_symbols(&stored, usize::MAX), Ok(None));
        // What can't be read is an error, as when inflating.
        assert_eq!(first_block_symbols(&[], 10), Err(InflateError::Truncated));
        assert_eq!(first_block_symbols(&[0x06], 10), Err(InflateError::BadBlockType));
        assert_eq!(first_block_symbols(&fixed_block(&lits, false)[..20], usize::MAX), Err(InflateError::Truncated));
        // A first match reaching before the start: length 3 (code 257, 7 bits 0000001), distance 1 (code 0).
        let mut w = BitWriter::new();
        w.put(2, 3);
        w.put(0b1000000, 7);
        w.put(0, 5);
        assert_eq!(first_block_symbols(&w.finish(), 10), Err(InflateError::TooFarBack));

        // A first block of 200 symbols: no memory level ends a block there, so nothing is tried.
        let z = wrapped(&fixed_block(&lits, false), &lits);
        assert_eq!(find_params(&lits, &z), None);
        assert_eq!(unpruned(&lits, &z), None);
        // 63 and 65535 symbols (2^6 - 1 and 2^16 - 1): out of the memory levels' range.
        for n in [63usize, 65535] {
            let lits: Vec<u8> = (0..n).map(|i| (i * 7 % 251) as u8).collect();
            let z = wrapped(&fixed_block(&lits, false), &lits);
            assert_eq!(find_params(&lits, &z), None);
        }

        // Ours, with a stored first block: random bytes in blocks of 127 symbols (memory level 1). The count
        // can't be read, so every memory level is still tried, and memory level 1 is found.
        let mut s = 7u64;
        let random: Vec<u8> = (0..1000)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect();
        let p = Params { level: 6, window_bits: 15, mem_level: 1, filtered: false };
        let z = zlib_stream(&random, p);
        assert_eq!(z[2] & 7, 0, "a stored first block, not the last");
        assert_eq!(first_block_symbols(&z[2..], usize::MAX), Ok(None));
        assert_eq!(find_params(&random, &z), Some(p));
        // Ours, all in one last block.
        let p = Params { level: 9, window_bits: 15, mem_level: 1, filtered: false };
        let z = zlib_stream(b"one block, one block", p);
        assert_eq!(first_block_symbols(&z[2..], usize::MAX), Ok(None));
        assert_eq!(find_params(b"one block, one block", &z), unpruned(b"one block, one block", &z));
        assert!(find_params(b"one block, one block", &z).is_some());
    }
}
