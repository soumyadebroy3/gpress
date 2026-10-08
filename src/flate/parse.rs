//! Optimal parsing: the cheapest sequence of literals and matches under a cost model (a shortest path
//! over byte positions).

use super::lz::{d_extra, ll_extra, Hist, Sym};
use super::matcher::Matches;
use super::tables::*;

/// Bits charged for each choice. `len[l]` includes the length symbol and its extra bits; `dist[c]` the
/// distance symbol and its extra bits.
#[derive(Clone)]
pub struct Costs {
    pub lit: [f32; 256],
    pub len: [f32; MAX_MATCH + 1],
    pub dist: [f32; DIST_SYMS],
}

impl Costs {
    fn from_symbol_costs(ll: &[f32], d: &[f32]) -> Costs {
        let mut c = Costs { lit: [0.0; 256], len: [0.0; MAX_MATCH + 1], dist: [0.0; DIST_SYMS] };
        c.lit.copy_from_slice(&ll[..256]);
        for l in MIN_MATCH..=MAX_MATCH {
            let s = 257 + LEN_CODE[l] as usize;
            c.len[l] = ll[s] + ll_extra(s) as f32;
        }
        for k in 0..DIST_SYMS {
            c.dist[k] = d[k] + d_extra(k) as f32;
        }
        c
    }

    /// Costs of the fixed Huffman codes: a neutral starting point.
    pub fn fixed() -> Costs {
        let ll = fixed_litlen_lengths();
        let llf: Vec<f32> = ll.iter().map(|&x| x as f32).collect();
        let d = [5.0f32; DIST_SYMS];
        Costs::from_symbol_costs(&llf, &d)
    }

    /// Costs from symbol counts: -log2(p). Unused symbols cost as if seen once.
    pub fn entropy(h: &Hist) -> Costs {
        let ll = entropy_bits(&h.ll);
        let d = entropy_bits(&h.d);
        Costs::from_symbol_costs(&ll, &d)
    }

    /// Costs from actual code lengths. Symbols without a code get `missing` bits.
    pub fn lengths(ll: &[u8], d: &[u8], missing: f32) -> Costs {
        let llf: Vec<f32> = ll.iter().map(|&x| if x == 0 { missing } else { x as f32 }).collect();
        let df: Vec<f32> = d.iter().map(|&x| if x == 0 { missing } else { x as f32 }).collect();
        Costs::from_symbol_costs(&llf, &df)
    }
}

fn entropy_bits(counts: &[u32]) -> Vec<f32> {
    let total: u64 = counts.iter().map(|&c| c as u64).sum::<u64>().max(1);
    let lt = (total as f64).log2();
    counts.iter().map(|&c| if c == 0 { lt as f32 + 1.0 } else { (lt - (c as f64).log2()).max(0.0) as f32 }).collect()
}

/// Scratch space reused between parses.
#[derive(Default)]
pub struct ParseScratch {
    cost: Vec<f32>,
    choice: Vec<u32>,
}

/// Cheapest parse of `data[a..b]` (positions inside the range covered by `m`). Matches are cut at `b`.
/// With `alt`, every length may use any candidate distance long enough (the cheapest); without it, each
/// length uses the smallest distance that reaches it.
// The parse takes its inputs apart on purpose: the same signature is built twice, once for AVX2.
#[allow(clippy::too_many_arguments)]
pub fn optimal(data: &[u8], a: usize, b: usize, m: &Matches, c: &Costs, alt: bool, sc: &mut ParseScratch, out: &mut Vec<Sym>) {
    optimal_nice(data, a, b, m, c, alt, MAX_MATCH + 1, sc, out)
}

/// As `optimal`, but a match of at least `nice` bytes is taken as soon as it is found (the positions it
/// covers are not tried as starting points): much faster on repetitive data, slightly less thorough.
#[allow(clippy::too_many_arguments)]
pub fn optimal_nice(data: &[u8], a: usize, b: usize, m: &Matches, c: &Costs, alt: bool, nice: usize, sc: &mut ParseScratch, out: &mut Vec<Sym>) {
    // The same code built for AVX2 where the processor has it (wider vectors, the same arithmetic).
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // Safe: the processor supports AVX2.
            return unsafe { parse_avx2(data, a, b, m, c, alt, nice, sc, out) };
        }
    }
    parse(data, a, b, m, c, alt, nice, sc, out)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn parse_avx2(data: &[u8], a: usize, b: usize, m: &Matches, c: &Costs, alt: bool, nice: usize, sc: &mut ParseScratch, out: &mut Vec<Sym>) {
    parse(data, a, b, m, c, alt, nice, sc, out)
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn parse(data: &[u8], a: usize, b: usize, m: &Matches, c: &Costs, alt: bool, nice: usize, sc: &mut ParseScratch, out: &mut Vec<Sym>) {
    let n = b - a;
    sc.cost.clear();
    sc.cost.resize(n + 1, f32::INFINITY);
    sc.choice.clear();
    sc.choice.resize(n + 1, 0);
    let cost = &mut sc.cost;
    let choice = &mut sc.choice;
    cost[0] = 0.0;
    let run_cost = c.len[MAX_MATCH] + c.dist[0];
    let mut skip_to = 0usize;
    for k in 0..n {
        if k < skip_to {
            continue;
        }
        let c0 = cost[k];
        // An unreachable position offers nothing, and must not skip positions that are reachable (with `nice`).
        if c0 == f32::INFINITY {
            continue;
        }
        let i = a + k;
        // Deep inside a long run of one byte (more than 2 x 258 left, and 258 back still inside it), the only
        // sensible step is a 258-byte match at distance 1; skip the full pricing there.
        if k > MAX_MATCH && i + MAX_MATCH <= b && m.same_at(i) > 2 * MAX_MATCH && m.same_at(i - MAX_MATCH) > MAX_MATCH {
            let v = c0 + run_cost;
            if v < cost[k + MAX_MATCH] {
                cost[k + MAX_MATCH] = v;
                choice[k + MAX_MATCH] = pack(MAX_MATCH, 1);
            }
            continue;
        }
        let lc = c0 + c.lit[data[i] as usize];
        if lc < cost[k + 1] {
            cost[k + 1] = lc;
            choice[k + 1] = pack(1, 0);
        }
        let maxl = (b - i).min(MAX_MATCH);
        if maxl < MIN_MATCH {
            continue;
        }
        // Steps longer than every closer one (the rest only matter with `alt`).
        let chain = m.chain_at(i);
        if chain.is_empty() {
            continue;
        }
        if nice <= MAX_MATCH {
            let longest = (chain[chain.len() - 1].0 as usize).min(maxl);
            if longest >= nice {
                // The smallest distance reaching that length.
                let d = chain.iter().find(|s| s.0 as usize >= longest).map(|s| s.1).unwrap();
                let v = c0 + c.dist[dist_code(d as usize)] + c.len[longest];
                if v < cost[k + longest] {
                    cost[k + longest] = v;
                    choice[k + longest] = pack(longest, d);
                }
                skip_to = k + longest;
                continue;
            }
        }
        let (cost_k, choice_k) = (&mut cost[k..], &mut choice[k..]);
        if !alt {
            // The smallest-distance chain: steps whose length beats every earlier (closer) one.
            let mut lo = MIN_MATCH;
            for &(ml, d) in chain {
                debug_assert!(ml as usize >= lo);
                let hi = (ml as usize).min(maxl);
                debug_assert!(k + hi <= n && hi <= MAX_MATCH);
                relax_range(cost_k, choice_k, &c.len, lo, hi, c0 + c.dist[dist_code(d as usize)], d);
                lo = hi + 1;
                if lo > maxl {
                    break;
                }
            }
            continue;
        }
        // Candidates sorted by length, longest first, each with its distance price.
        let mut cand = [(0u16, 0f32, 0u16); 32];
        let mut cnt = 0;
        for &(ml, d) in m.at(i).iter().take(32) {
            let e = (ml, c.dist[dist_code(d as usize)], d);
            let mut j = cnt;
            while j > 0 && cand[j - 1].0 < ml {
                cand[j] = cand[j - 1];
                j -= 1;
            }
            cand[j] = e;
            cnt += 1;
        }
        let top = (cand[0].0 as usize).min(maxl);
        debug_assert!(k + top <= n && top <= MAX_MATCH);
        // Lengths from the longest down; each uses the cheapest distance among the candidates long enough,
        // which only changes where another candidate joins.
        let (mut j, mut bc, mut bd) = (0usize, f32::INFINITY, 0u16);
        let mut l = top;
        while l >= MIN_MATCH {
            while j < cnt && cand[j].0 as usize >= l {
                if cand[j].1 < bc {
                    bc = cand[j].1;
                    bd = cand[j].2;
                }
                j += 1;
            }
            let lo = if j < cnt { cand[j].0 as usize + 1 } else { MIN_MATCH };
            relax_range(cost_k, choice_k, &c.len, lo, l, c0 + bc, bd);
            l = lo - 1;
        }
    }
    // Walk back from the end.
    let first = out.len();
    let mut k = n;
    while k > 0 {
        let (l, d) = (choice[k] & 0xffff, choice[k] >> 16);
        if d == 0 {
            out.push(Sym::lit(data[a + k - 1]));
            k -= 1;
        } else {
            out.push(Sym::mat(l as usize, d as usize));
            k -= l as usize;
        }
    }
    out[first..].reverse();
}

/// A match choice: length in the low 16 bits, distance in the high 16 bits (0 for a literal).
#[inline]
fn pack(l: usize, d: u16) -> u32 {
    l as u32 | (d as u32) << 16
}

/// `cost[l] = base + len[l]` and `choice[l] = (l, d)` for every `l` in `lo..=hi` where that is lower than
/// `cost[l]`. Written without branches so the compiler can use vector instructions; the result is exactly
/// that of the plain loop (the same additions and comparisons, in any order since each `l` is separate).
#[inline(always)]
fn relax_range(cost: &mut [f32], choice: &mut [u32], len: &[f32], lo: usize, hi: usize, base: f32, d: u16) {
    let tag = (d as u32) << 16;
    let (cost, choice, len) = (&mut cost[lo..=hi], &mut choice[lo..=hi], &len[lo..=hi]);
    for (i, ((c, ch), &x)) in cost.iter_mut().zip(choice.iter_mut()).zip(len).enumerate() {
        let v = base + x;
        let better = v < *c;
        *c = if better { v } else { *c };
        *ch = if better { (lo + i) as u32 | tag } else { *ch };
    }
}

/// Greedy parse: the longest match wherever there is one (closest distance), otherwise a literal.
pub fn greedy(data: &[u8], a: usize, b: usize, m: &Matches, out: &mut Vec<Sym>) {
    let mut i = a;
    while i < b {
        let maxl = (b - i).min(MAX_MATCH);
        let mut best: Option<(usize, usize)> = None;
        if maxl >= MIN_MATCH {
            for &(ml, d) in m.chain_at(i) {
                let l = (ml as usize).min(maxl);
                if l >= MIN_MATCH && best.is_none_or(|(bl, _)| l > bl) {
                    best = Some((l, d as usize));
                }
            }
        }
        match best {
            Some((l, d)) => {
                out.push(Sym::mat(l, d));
                i += l;
            }
            None => {
                out.push(Sym::lit(data[i]));
                i += 1;
            }
        }
    }
}
