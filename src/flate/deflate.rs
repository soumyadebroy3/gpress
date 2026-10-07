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
use super::parse::{greedy, optimal, Costs, ParseScratch};
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
    /// Also offer farther matches whose distance codes may be cheaper.
    pub alternatives: bool,
    /// Segments up to this size are also optimised from a second starting point.
    pub multi_start_limit: usize,
}

impl Options {
    /// Our strongest setting.
    pub fn max() -> Options {
        Options { iterations: 8, exact_rounds: 4, max_chain: 8192, segment: 1 << 20, split: true, alternatives: true, multi_start_limit: 1 << 18 }
    }
    /// Fast but still optimal-parsed.
    pub fn quick() -> Options {
        Options { iterations: 2, exact_rounds: 2, max_chain: 128, segment: 1 << 20, split: true, alternatives: false, multi_start_limit: 0 }
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
        let t0 = std::time::Instant::now();
        let m = find_matches(data, a, b, opt.max_chain, opt.alternatives);
        let t1 = t0.elapsed();
        let plans = optimize_segment(data, a, b, &m, opt);
        if std::env::var_os("PDFE_DEBUG").is_some() {
            eprintln!(
                "segment {}..{}: matches {:?} ({} steps), optimise {:?}, {} blocks",
                a,
                b,
                t1,
                m.steps.len(),
                t0.elapsed() - t1,
                plans.len()
            );
        }
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

/// Block size for comparing parses: a close search for small blocks (where the header matters most), a
/// quick estimate for large ones.
fn price(h: &Hist, nbytes: usize) -> u64 {
    if nbytes < 65536 {
        3 + good_dynamic(h).bits.min(fixed_bits(h))
    } else {
        3 + estimate_dynamic_bits(h).min(fixed_bits(h))
    }
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
    let dbg = std::env::var_os("PDFE_DEBUG").is_some();
    let t = std::time::Instant::now();
    let mut sc = ParseScratch::default();
    // 1. The whole segment, from neutral prices and (for smaller segments) also from a greedy parse's
    //    statistics; the better result goes on.
    let mut first = Vec::new();
    optimal(data, a, b, m, &Costs::fixed(), false, &mut sc, &mut first);
    let mut syms = refine(data, a, b, m, opt, first, &mut sc);
    if b - a <= opt.multi_start_limit {
        let mut g = Vec::new();
        greedy(data, a, b, m, &mut g);
        let mut second = Vec::new();
        optimal(data, a, b, m, &Costs::entropy(&Hist::of(&g)), false, &mut sc, &mut second);
        let other = refine(data, a, b, m, opt, second, &mut sc);
        if price(&Hist::of(&other), b - a) < price(&Hist::of(&syms), b - a) {
            syms = other;
        }
    }
    let t1 = t.elapsed();
    // 2. Split into blocks.
    let mut plans = if opt.split { split(data, a, b, syms) } else { vec![Plan { a, b, syms }] };
    let t2 = t.elapsed();
    // 3. Each block under its own codes.
    for p in plans.iter_mut() {
        let s = std::mem::take(&mut p.syms);
        let s = refine(data, p.a, p.b, m, opt, s, &mut sc);
        p.syms = exact(data, p.a, p.b, m, opt, s, &mut sc);
    }
    let t3 = t.elapsed();
    // 4. Polish: merge neighbours that are cheaper together, split blocks that are cheaper apart.
    if opt.split {
        polish(data, m, opt, &mut plans, &mut sc);
    }
    if dbg {
        eprintln!("  global {:?}, split {:?}, blocks {:?}, polish {:?}", t1, t2 - t1, t3 - t2, t.elapsed() - t3);
    }
    plans
}

fn plan_price(p: &Plan) -> u64 {
    price(&Hist::of(&p.syms), p.b - p.a)
}

fn polish(data: &[u8], m: &Matches, opt: &Options, plans: &mut Vec<Plan>, sc: &mut ParseScratch) {
    for _pass in 0..2 {
        let mut changed = false;
        // Merges.
        let mut k = 0;
        while k + 1 < plans.len() {
            let (pa, pb) = (&plans[k], &plans[k + 1]);
            let apart = plan_price(pa) + plan_price(pb);
            let mut joined: Vec<Sym> = Vec::with_capacity(pa.syms.len() + pb.syms.len());
            joined.extend_from_slice(&pa.syms);
            joined.extend_from_slice(&pb.syms);
            let (a, b) = (pa.a, pb.b);
            let together = price(&Hist::of(&joined), b - a);
            // Merging lets one parse span both, so re-parse before judging when it is close.
            if together <= apart + (apart / 50).min(400) {
                let better = exact(data, a, b, m, opt, joined, sc);
                if price(&Hist::of(&better), b - a) < apart {
                    plans[k] = Plan { a, b, syms: better };
                    plans.remove(k + 1);
                    changed = true;
                    continue;
                }
            }
            k += 1;
        }
        // Splits: try the best single cut of each block.
        let mut k = 0;
        while k < plans.len() {
            let p = &plans[k];
            let n = p.syms.len();
            if n < 64 {
                k += 1;
                continue;
            }
            let whole = plan_price(p);
            let total = Hist::of(&p.syms);
            let step = (n / 64).max(4);
            let mut left = Hist::of(&[]);
            let mut best = (u64::MAX, 0usize);
            let mut c = 0;
            while c + step < n {
                for s in &p.syms[c..c + step] {
                    left.add(s);
                }
                c += step;
                let right = Hist::diff(&total, &left);
                let v = estimate_dynamic_bits(&left).min(fixed_bits(&left)) + estimate_dynamic_bits(&right).min(fixed_bits(&right));
                if v < best.0 {
                    best = (v, c);
                }
            }
            if best.1 == 0 || best.0 + 6 >= whole {
                k += 1;
                continue;
            }
            let cut = best.1;
            let mid = p.a + p.syms[..cut].iter().map(|x| x.span()).sum::<usize>();
            let (a, b) = (p.a, p.b);
            let l = exact(data, a, mid, m, opt, p.syms[..cut].to_vec(), sc);
            let r = exact(data, mid, b, m, opt, p.syms[cut..].to_vec(), sc);
            let split_price = price(&Hist::of(&l), mid - a) + price(&Hist::of(&r), b - mid);
            if split_price < whole {
                plans[k] = Plan { a, b: mid, syms: l };
                plans.insert(k + 1, Plan { a: mid, b, syms: r });
                changed = true;
                k += 2;
            } else {
                k += 1;
            }
        }
        if !changed {
            break;
        }
    }
}

/// Entropy-priced rounds: price symbols by how often the current parse uses them, re-parse, repeat.
fn refine(data: &[u8], a: usize, b: usize, m: &Matches, opt: &Options, start: Vec<Sym>, sc: &mut ParseScratch) -> Vec<Sym> {
    let mut best_bits = price(&Hist::of(&start), b - a);
    let mut best = start;
    let mut stats = Hist::of(&best);
    let mut last_bits = best_bits;
    let mut cur = Vec::with_capacity(best.len());
    let mut stuck = 0;
    for it in 0..opt.iterations {
        cur.clear();
        optimal(data, a, b, m, &Costs::entropy(&stats), false, sc, &mut cur);
        let h = Hist::of(&cur);
        let bits = price(&h, b - a);
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
    let mut best_bits = price(&Hist::of(&start), b - a);
    let mut best = start;
    let mut cur = Vec::with_capacity(best.len());
    for _ in 0..opt.exact_rounds {
        let h = Hist::of(&best);
        let code = best_dynamic(&h);
        let maxlen = *code.ll.iter().max().unwrap_or(&15) as f32;
        let mut improved = false;
        for (missing, alt) in [(maxlen + 1.0, false), (maxlen + 4.0, false), (maxlen + 1.0, opt.alternatives)] {
            if alt == false && missing > maxlen + 1.0 && opt.alternatives {
                // keep the variety small when alternatives are tried
            }
            cur.clear();
            optimal(data, a, b, m, &Costs::lengths(&code.ll, &code.d, missing), alt, sc, &mut cur);
            let bits = price(&Hist::of(&cur), b - a);
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
    let g = (n / 160).max(32);
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
    let span_limit = 96usize;
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
    // Fine-tune each inner boundary in symbol steps around its candidate (histograms kept incrementally).
    let mut idx: Vec<usize> = cuts.iter().map(|&c| cand[c]).collect();
    for t in 1..idx.len() - 1 {
        let (l, r) = (idx[t - 1], idx[t + 1]);
        let total = Hist::of(&syms[l..r]);
        let lo = idx[t].saturating_sub(g).max(l + 1);
        let hi = (idx[t] + g).min(r - 1);
        let step = (g / 8).max(1);
        let mut left = Hist::of(&syms[l..lo]);
        let mut c = lo;
        let mut bestc = idx[t];
        let mut bestv = u64::MAX;
        while c <= hi {
            let right = Hist::diff(&total, &left);
            let v = estimate_dynamic_bits(&left).min(fixed_bits(&left)) + estimate_dynamic_bits(&right).min(fixed_bits(&right));
            if v < bestv {
                bestv = v;
                bestc = c;
            }
            let next = (c + step).min(hi + 1);
            for s in &syms[c..next.min(r)] {
                left.add(s);
            }
            c = next;
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
