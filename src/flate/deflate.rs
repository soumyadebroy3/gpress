//! Our Deflate encoder.
//!
//! For each segment of the input it finds every usable match, then searches for the cheapest parse by
//! alternating "price the symbols" and "find the cheapest path under those prices", first with smooth
//! (entropy) prices and then with the exact code lengths the block will be written with. Block boundaries
//! come from a dynamic-programming search over candidate split points, after which each block is
//! re-parsed under its own codes. Every block is finally written in whichever form is smallest:
//! dynamic Huffman, fixed Huffman or stored.

use super::bits::BitWriter;
use super::block::*;
use super::lz::{Hist, Sym};
use super::matcher::{find_matches, Matches};
use super::parse::{optimal, Costs, ParseScratch};
use super::tables::*;

#[derive(Clone, Debug)]
pub struct Options {
    /// Rounds of entropy-priced parsing (per segment, then per block).
    pub iterations: usize,
    /// Rounds of exact-length parsing per block (stops early when nothing improves).
    pub exact_rounds: usize,
    /// How many earlier positions the match finder compares at most.
    pub max_chain: usize,
    /// Bytes optimised together; blocks never cross segments.
    pub segment: usize,
    pub split: bool,
}

impl Options {
    /// Our strongest setting.
    pub fn max() -> Options {
        Options { iterations: 15, exact_rounds: 8, max_chain: 8192, segment: 1 << 20, split: true }
    }
    /// Fast but still optimal-parsed.
    pub fn quick() -> Options {
        Options { iterations: 2, exact_rounds: 2, max_chain: 128, segment: 1 << 20, split: true }
    }
}

/// Raw Deflate data for `data`.
pub fn deflate(data: &[u8], opt: &Options) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(data.len() / 2 + 64);
    if data.is_empty() {
        write_fixed(&mut w, &[], true);
        return w.finish();
    }
    let mut a = 0;
    while a < data.len() {
        let b = (a + opt.segment).min(data.len());
        let m = find_matches(data, a, b, opt.max_chain);
        let plans = optimize_segment(data, a, b, &m, opt);
        let n = plans.len();
        for (k, p) in plans.iter().enumerate() {
            let last = b == data.len() && k + 1 == n;
            write_best(&mut w, data, p, last);
        }
        a = b;
    }
    w.finish()
}

/// Zlib-wrapped data, as PDF's FlateDecode expects.
pub fn deflate_zlib(data: &[u8], opt: &Options) -> Vec<u8> {
    super::zlib::wrap(&deflate(data, opt), data)
}

struct Plan {
    a: usize,
    b: usize,
    syms: Vec<Sym>,
}

/// Exact size of a block in its best form (dynamic or fixed), excluding stored, plus the 3-bit block header.
fn coded_bits(h: &Hist) -> u64 {
    3 + best_dynamic(h).bits.min(fixed_bits(h))
}

fn write_best(w: &mut BitWriter, data: &[u8], p: &Plan, last: bool) {
    let h = Hist::of(&p.syms);
    let dyncode = best_dynamic(&h);
    let dyn_bits = 3 + dyncode.bits;
    let fix_bits = 3 + fixed_bits(&h);
    let st_bits = stored_bits(p.b - p.a, w.bit_len());
    if st_bits < dyn_bits.min(fix_bits) {
        write_stored(w, &data[p.a..p.b], last);
    } else if fix_bits <= dyn_bits {
        write_fixed(w, &p.syms, last);
    } else {
        write_dynamic(w, &p.syms, &dyncode, last);
    }
}

fn optimize_segment(data: &[u8], a: usize, b: usize, m: &Matches, opt: &Options) -> Vec<Plan> {
    let mut sc = ParseScratch::default();
    // 1. The whole segment, from neutral prices.
    let mut first = Vec::new();
    optimal(data, a, b, m, &Costs::fixed(), &mut sc, &mut first);
    let syms = refine(data, a, b, m, opt, first, &mut sc);
    // 2. Split into blocks.
    let mut plans = if opt.split { split(data, a, b, syms) } else { vec![Plan { a, b, syms }] };
    // 3. Each block under its own codes.
    for p in plans.iter_mut() {
        let s = std::mem::take(&mut p.syms);
        let s = refine(data, p.a, p.b, m, opt, s, &mut sc);
        p.syms = exact(data, p.a, p.b, m, opt, s, &mut sc);
    }
    // 4. Once more: the re-parsed blocks may prefer other boundaries.
    if opt.split && plans.len() > 1 {
        let all: Vec<Sym> = plans.iter().flat_map(|p| p.syms.iter().copied()).collect();
        let before: u64 = plans.iter().map(|p| coded_bits(&Hist::of(&p.syms))).sum();
        let mut again = split(data, a, b, all);
        for p in again.iter_mut() {
            let s = std::mem::take(&mut p.syms);
            p.syms = exact(data, p.a, p.b, m, opt, s, &mut sc);
        }
        let after: u64 = again.iter().map(|p| coded_bits(&Hist::of(&p.syms))).sum();
        if after < before {
            plans = again;
        }
    }
    plans
}

/// Entropy-priced rounds: price symbols by how often the current parse uses them, re-parse, repeat.
fn refine(data: &[u8], a: usize, b: usize, m: &Matches, opt: &Options, start: Vec<Sym>, sc: &mut ParseScratch) -> Vec<Sym> {
    let mut best_bits = coded_bits(&Hist::of(&start));
    let mut best = start;
    let mut stats = Hist::of(&best);
    let mut last_bits = best_bits;
    let mut cur = Vec::with_capacity(best.len());
    let mut stuck = 0;
    for it in 0..opt.iterations {
        cur.clear();
        optimal(data, a, b, m, &Costs::entropy(&stats), sc, &mut cur);
        let h = Hist::of(&cur);
        let bits = coded_bits(&h);
        if bits < best_bits {
            best_bits = bits;
            best.clone_from(&cur);
        }
        if bits >= last_bits {
            stuck += 1;
            // Blend in the previous statistics to step out of a cycle.
            stats = blend(&h, &stats, it as u64 + stuck);
            if stuck > 3 {
                break;
            }
        } else {
            stuck = 0;
            stats = h;
        }
        last_bits = bits;
    }
    best
}

fn blend(new: &Hist, old: &Hist, salt: u64) -> Hist {
    let mut h = new.clone();
    let mut r = 0x2545_F491_4F6C_DD1Du64 ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for i in 0..LITLEN_SYMS {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        h.ll[i] = new.ll[i] + old.ll[i] / 2 + (r % 2) as u32 * (new.ll[i] / 8);
    }
    for i in 0..DIST_SYMS {
        h.d[i] = new.d[i] + old.d[i] / 2;
    }
    h
}

/// Exact-priced rounds: price symbols by the code lengths the block would really use.
fn exact(data: &[u8], a: usize, b: usize, m: &Matches, opt: &Options, start: Vec<Sym>, sc: &mut ParseScratch) -> Vec<Sym> {
    let mut best_bits = coded_bits(&Hist::of(&start));
    let mut best = start;
    let mut cur = Vec::with_capacity(best.len());
    for _ in 0..opt.exact_rounds {
        let h = Hist::of(&best);
        let code = best_dynamic(&h);
        let maxlen = *code.ll.iter().max().unwrap_or(&15) as f32;
        let mut improved = false;
        for missing in [maxlen + 1.0, maxlen + 4.0] {
            cur.clear();
            optimal(data, a, b, m, &Costs::lengths(&code.ll, &code.d, missing), sc, &mut cur);
            let bits = coded_bits(&Hist::of(&cur));
            if bits < best_bits {
                best_bits = bits;
                best.clone_from(&cur);
                improved = true;
            }
        }
        if !improved {
            break;
        }
    }
    best
}

/// Chooses block boundaries by dynamic programming over candidate split points.
fn split(data: &[u8], a: usize, b: usize, syms: Vec<Sym>) -> Vec<Plan> {
    let n = syms.len();
    if n < 64 {
        return vec![Plan { a, b, syms }];
    }
    let g = (n / 384).max(32);
    let mut cand: Vec<usize> = (0..n).step_by(g).collect();
    if *cand.last().unwrap() != n {
        cand.push(n);
    }
    let k = cand.len();
    // Byte position of every candidate and prefix histograms.
    let mut pos = Vec::with_capacity(k);
    let mut pref: Vec<Hist> = Vec::with_capacity(k);
    let mut h = Hist { ll: [0; LITLEN_SYMS], d: [0; DIST_SYMS] };
    let mut bytes = a;
    let mut si = 0;
    for &c in &cand {
        while si < c {
            h.add(&syms[si]);
            bytes += syms[si].span();
            si += 1;
        }
        pos.push(bytes);
        pref.push(h.clone());
    }
    let span_limit = 160usize;
    let est = |i: usize, j: usize| -> u64 {
        let hh = Hist::diff(&pref[j], &pref[i]);
        let nbytes = pos[j] - pos[i];
        3 + estimate_dynamic_bits(&hh).min(fixed_bits(&hh)).min(8 * nbytes as u64 + 40)
    };
    let mut best = vec![u64::MAX; k];
    let mut from = vec![0usize; k];
    best[0] = 0;
    for j in 1..k {
        let lo = j.saturating_sub(span_limit);
        for i in lo..j {
            if best[i] == u64::MAX {
                continue;
            }
            let c = best[i] + est(i, j);
            if c < best[j] {
                best[j] = c;
                from[j] = i;
            }
        }
    }
    let mut cuts = vec![k - 1];
    let mut j = k - 1;
    while j > 0 {
        j = from[j];
        cuts.push(j);
    }
    cuts.reverse();
    // Fine-tune each inner boundary in symbol steps around its candidate.
    let mut idx: Vec<usize> = cuts.iter().map(|&c| cand[c]).collect();
    let hist_of = |s: usize, e: usize| Hist::of(&syms[s..e]);
    for t in 1..idx.len() - 1 {
        let (l, r) = (idx[t - 1], idx[t + 1]);
        let mut bestc = idx[t];
        let mut bestv = coded_bits(&hist_of(l, bestc)) + coded_bits(&hist_of(bestc, r));
        let step = (g / 8).max(1);
        let mut c = idx[t].saturating_sub(g).max(l + 1);
        while c < (idx[t] + g).min(r) {
            let v = estimate_pair(&syms, l, c, r);
            if v < bestv {
                let exactv = coded_bits(&hist_of(l, c)) + coded_bits(&hist_of(c, r));
                if exactv < bestv {
                    bestv = exactv;
                    bestc = c;
                }
            }
            c += step;
        }
        idx[t] = bestc;
    }
    // Build the plans.
    let mut plans = Vec::with_capacity(idx.len() - 1);
    let mut byte = a;
    for w in idx.windows(2) {
        let s: Vec<Sym> = syms[w[0]..w[1]].to_vec();
        let len: usize = s.iter().map(|x| x.span()).sum();
        plans.push(Plan { a: byte, b: byte + len, syms: s });
        byte += len;
    }
    debug_assert_eq!(byte, b);
    let _ = data;
    plans
}

fn estimate_pair(syms: &[Sym], l: usize, c: usize, r: usize) -> u64 {
    let h1 = Hist::of(&syms[l..c]);
    let h2 = Hist::of(&syms[c..r]);
    estimate_dynamic_bits(&h1).min(fixed_bits(&h1)) + estimate_dynamic_bits(&h2).min(fixed_bits(&h2))
}
