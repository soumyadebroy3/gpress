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
    choice: Vec<(u16, u16)>,
}

/// Cheapest parse of `data[a..b]` (positions inside the range covered by `m`). Matches are cut at `b`.
pub fn optimal(data: &[u8], a: usize, b: usize, m: &Matches, c: &Costs, sc: &mut ParseScratch, out: &mut Vec<Sym>) {
    let n = b - a;
    sc.cost.clear();
    sc.cost.resize(n + 1, f32::INFINITY);
    sc.choice.clear();
    sc.choice.resize(n + 1, (0, 0));
    let cost = &mut sc.cost;
    let choice = &mut sc.choice;
    cost[0] = 0.0;
    for k in 0..n {
        let c0 = cost[k];
        let i = a + k;
        let lc = c0 + c.lit[data[i] as usize];
        if lc < cost[k + 1] {
            cost[k + 1] = lc;
            choice[k + 1] = (1, 0);
        }
        let maxl = (b - i).min(MAX_MATCH);
        if maxl < MIN_MATCH {
            continue;
        }
        let mut lo = MIN_MATCH;
        for &(ml, d) in m.at(i) {
            let hi = (ml as usize).min(maxl);
            if hi < lo {
                break;
            }
            let dc = c0 + c.dist[dist_code(d as usize)];
            for l in lo..=hi {
                let v = dc + c.len[l];
                if v < cost[k + l] {
                    cost[k + l] = v;
                    choice[k + l] = (l as u16, d);
                }
            }
            lo = hi + 1;
            if lo > maxl {
                break;
            }
        }
    }
    // Walk back from the end.
    let first = out.len();
    let mut k = n;
    while k > 0 {
        let (l, d) = choice[k];
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
