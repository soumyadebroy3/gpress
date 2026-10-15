//! Optimal parsing: the cheapest sequence of literals and matches under a cost model (a shortest path
//! over byte positions).

use super::lz::{d_extra, ll_extra, Hist, Sym};
use super::matcher::{Matches, Step};
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
    cells: Vec<Cell>,
    done: Finished,
}

/// What the finished groups of a parse leave behind.
#[derive(Default)]
struct Finished {
    choices: Vec<Lanes<u32>>,
    /// Their costs too, for the tests.
    #[cfg(test)]
    costs: Vec<Lanes<f32>>,
}

impl Finished {
    fn clear(&mut self, groups: usize) {
        self.choices.clear();
        self.choices.reserve(groups);
        #[cfg(test)]
        self.costs.clear();
    }

    #[inline(always)]
    fn take(&mut self, cells: &[Cell]) {
        self.choices.extend(cells.iter().map(|c| c.choice));
        #[cfg(test)]
        self.costs.extend(cells.iter().map(|c| c.cost));
    }
}

/// Eight neighbouring entries (positions `8g..8g + 8`), aligned so that each group is one vector load or store.
/// The parser always reads and writes whole groups, so a group just written is read back whole (and never
/// half from a narrower write).
#[derive(Clone, Copy)]
#[repr(C, align(32))]
struct Lanes<T>([T; 8]);

/// The costs and choices of one group of positions, side by side (one window holds both).
#[derive(Clone, Copy)]
#[repr(C)]
struct Cell {
    cost: Lanes<f32>,
    choice: Lanes<u32>,
}

/// One group of a window as a position at lane `o` sees it: the price of each lane's length (infinity where it
/// is not a match length) and the length itself, `8g + j - o`. The lengths come from a table rather than
/// arithmetic so that the compiler treats all lanes alike (and makes one vector operation of them).
#[derive(Clone, Copy)]
#[repr(C)]
struct Shift {
    len: Lanes<f32>,
    lane: Lanes<i32>,
}

/// Groups in the window of one position: the position's own group and every length up to `MAX_MATCH` from it.
const WG: usize = MAX_MATCH.div_ceil(8) + 1;

/// Groups a parse keeps at a time (the window of the current position, and room to slide it).
const RING: usize = 8 * WG;

/// Cheapest parse of `data[a..b]` (positions inside the range covered by `m`). Matches are cut at `b`.
/// With `alt`, every length may use any candidate distance long enough (the cheapest); without it, each
/// length uses the smallest distance that reaches it.
// The parse takes its inputs apart on purpose: the same signature is built twice, once for AVX2.
#[allow(clippy::too_many_arguments)]
pub fn optimal(
    data: &[u8],
    a: usize,
    b: usize,
    m: &Matches,
    c: &Costs,
    alt: bool,
    sc: &mut ParseScratch,
    out: &mut Vec<Sym>,
) {
    optimal_nice(data, a, b, m, c, alt, MAX_MATCH + 1, sc, out)
}

/// As `optimal`, but a match of at least `nice` bytes is taken as soon as it is found (the positions it
/// covers are not tried as starting points): much faster on repetitive data, slightly less thorough.
#[allow(clippy::too_many_arguments)]
pub fn optimal_nice(
    data: &[u8],
    a: usize,
    b: usize,
    m: &Matches,
    c: &Costs,
    alt: bool,
    nice: usize,
    sc: &mut ParseScratch,
    out: &mut Vec<Sym>,
) {
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
unsafe fn parse_avx2(
    data: &[u8],
    a: usize,
    b: usize,
    m: &Matches,
    c: &Costs,
    alt: bool,
    nice: usize,
    sc: &mut ParseScratch,
    out: &mut Vec<Sym>,
) {
    parse(data, a, b, m, c, alt, nice, sc, out)
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn parse(
    data: &[u8],
    a: usize,
    b: usize,
    m: &Matches,
    c: &Costs,
    alt: bool,
    nice: usize,
    sc: &mut ParseScratch,
    out: &mut Vec<Sym>,
) {
    let n = b - a;
    // Position `k` is lane `k % 8` of group `k / 8`. At most `RING` groups are kept, from group `base` on
    // (group `g` is `cells[g - base]`): a position writes at most `MAX_MATCH` ahead and never behind, so the
    // groups before its own are finished, and leave their choices in `done`. Matches are relaxed whole, even
    // where they run past `n`: nothing reads those entries.
    let fresh = Cell { cost: Lanes([f32::INFINITY; 8]), choice: Lanes([0; 8]) };
    let ring = (n / 8 + WG + 1).min(RING);
    sc.cells.clear();
    sc.cells.resize(ring, fresh);
    let (cells, done) = (&mut sc.cells[..ring], &mut sc.done);
    done.clear(n / 8 + 1);
    cells[0].cost.0[0] = 0.0;
    let mut base = 0;
    let run_cost = c.len[MAX_MATCH] + c.dist[0];
    // The positions with 258 bytes on either side: `MAX_MATCH < k && k + MAX_MATCH <= n`, as one comparison.
    let run_span = n.saturating_sub(2 * MAX_MATCH);
    let price = dist_prices(c);
    let shifts = shifts(c);
    // A match is at least 3 bytes, so a smaller `nice` acts as 3.
    let nice = nice.max(MIN_MATCH);
    // The per-position lists, taken once for the whole range.
    let data = &data[a..b];
    let from = a - m.from;
    let same = &m.same[from..][..n];
    let (start, all) = (&m.start[from..][..=n], &m.steps[..]);
    // How many of each position's steps are alternatives (after its chain); none without them.
    let farther: &[u8] = if m.alternatives { &m.farther[from..][..n] } else { &[] };
    // The literal step from the position before into this one, relaxed here rather than there: that position
    // was the last to relax this one, so this is the same as relaxing it there. Infinity when there is none.
    let mut lit = f32::INFINITY;
    let mut k = 0;
    while k < n {
        let (w, o) = (k / 8, k % 8);
        let mut i = w - base;
        if i > ring - WG {
            // Slide the groups up to this one (a step moves at most `MAX_MATCH`, so it is still kept).
            done.take(&cells[..i]);
            cells.copy_within(i.., 0);
            cells[ring - i..].fill(fresh);
            (base, i) = (w, 0);
        }
        // Length `l` of this position is lane `(o + l) % 8` of group `(o + l) / 8` in these windows.
        let win: &mut [Cell; WG] = (&mut cells[i..i + WG]).try_into().unwrap();
        let mut c0 = win[0].cost.0[o];
        if lit < c0 {
            c0 = lit;
            win[0].cost.0[o] = lit;
            win[0].choice.0[o] = pack(1, 0);
        }
        lit = f32::INFINITY;
        // An unreachable position offers nothing, and must not skip positions that are reachable (with `nice`).
        if c0 == f32::INFINITY {
            k += 1;
            continue;
        }
        // Deep inside a long run of one byte (more than 2 x 258 left, and 258 back still inside it), the only
        // sensible step is a 258-byte match at distance 1; skip the full pricing there. (`same` counts the
        // lengths of `RUNS` a run is longer than: 258, 258 + 7, 2 x 258, 2 x 258 + 7.)
        if same[k] >= 3 && k.wrapping_sub(MAX_MATCH + 1) < run_span && same[k - MAX_MATCH] >= 1 {
            // At the start of a group with all eight positions that deep in the run (each run is one byte
            // repeated, so 7 more of it covers the next 7 positions), the whole group at once: each position
            // only writes 258 ahead, where none of the others reads.
            if o == 0 && same[k] >= 4 && k + MAX_MATCH + 7 <= n && same[k - MAX_MATCH] >= 2 {
                let c0s = win[0].cost.0;
                for (i, c0) in c0s.into_iter().enumerate() {
                    let v = c0 + run_cost;
                    let (g, j) = ((i + MAX_MATCH) / 8, (i + MAX_MATCH) % 8);
                    if v < win[g].cost.0[j] {
                        win[g].cost.0[j] = v;
                        win[g].choice.0[j] = pack(MAX_MATCH, 1);
                    }
                }
                k += 8;
                continue;
            }
            let v = c0 + run_cost;
            let (g, j) = ((o + MAX_MATCH) / 8, (o + MAX_MATCH) % 8);
            if v < win[g].cost.0[j] {
                win[g].cost.0[j] = v;
                win[g].choice.0[j] = pack(MAX_MATCH, 1);
            }
            k += 1;
            continue;
        }
        lit = c0 + c.lit[data[k] as usize];
        // Every step of this position: the chain (steps longer than every closer one), then with alternatives
        // the farther ones, which only matter with `alt`.
        let steps = &all[start[k] as usize..start[k + 1] as usize];
        let (chain, far) = steps.split_at(steps.len().saturating_sub(farther.get(k).map_or(0, |&f| f as usize)));
        let Some((ml, _)) = chain.last().map(|s| s.get()) else {
            k += 1;
            continue;
        };
        // (The longest match reaches `nice` when both the step and the bytes left do.)
        if ml as usize >= nice && n - k >= nice {
            // The next position is not tried: its literal goes in now.
            let (g, j) = ((o + 1) / 8, (o + 1) % 8);
            if lit < win[g].cost.0[j] {
                win[g].cost.0[j] = lit;
                win[g].choice.0[j] = pack(1, 0);
            }
            lit = f32::INFINITY;
            let longest = (ml as usize).min(n - k);
            // The smallest distance reaching that length.
            let d = chain.iter().map(|s| s.get()).find(|s| s.0 as usize >= longest).map(|s| s.1).unwrap();
            let v = c0 + price[dist_slot(d)] + c.len[longest];
            let (g, j) = ((o + longest) / 8, (o + longest) % 8);
            if v < win[g].cost.0[j] {
                win[g].cost.0[j] = v;
                win[g].choice.0[j] = pack(longest, d);
            }
            // The positions this match covers are not tried.
            k += longest;
            continue;
        }
        let shift = &shifts[o];
        if !alt {
            // The smallest-distance chain: steps whose length beats every earlier (closer) one.
            let steps = chain.iter().map(|s| {
                let (ml, d) = s.get();
                (ml as usize, c0 + price[dist_slot(d)], (d as u32) << 16)
            });
            relax_matches(win, shift, o, steps);
            k += 1;
            continue;
        }
        // The first 32 steps the walk found (nearly always all of them), sorted by length, shortest first (of
        // equal lengths, the later one first), packed as length << 16 | distance. Sorted as length << 16 |
        // !distance, an order that does not depend on the order the steps come in.
        let key = |s: &Step| {
            let (ml, d) = s.get();
            (ml as u32) << 16 | !d as u32
        };
        let mut cand = [0u32; 32];
        let cnt = steps.len().min(32);
        if far.is_empty() {
            // The chain alone, already in order.
            for (e, s) in cand.iter_mut().zip(chain) {
                *e = key(s) ^ 0xffff;
            }
        } else if steps.len() <= 32 {
            // The chain and the farther steps (which the matcher puts in this order too), merged.
            let (mut x, mut y) = (0, 0);
            for e in &mut cand[..cnt] {
                let kc = chain.get(x).map_or(u32::MAX, key);
                let kf = far.get(y).map_or(u32::MAX, key);
                if kc < kf {
                    *e = kc ^ 0xffff;
                    x += 1;
                } else {
                    *e = kf ^ 0xffff;
                    y += 1;
                }
            }
        } else {
            // The walk found them in order of distance: the chain and the farther steps merged by distance,
            // each sorted in.
            let (mut x, mut y) = (0, 0);
            for i in 0..32 {
                let s = if y == far.len() || (x < chain.len() && chain[x].get().1 < far[y].get().1) {
                    x += 1;
                    &chain[x - 1]
                } else {
                    y += 1;
                    &far[y - 1]
                };
                let v = key(s);
                let mut j = i;
                while j > 0 && cand[j - 1] > v {
                    cand[j] = cand[j - 1];
                    j -= 1;
                }
                cand[j] = v;
            }
            for e in &mut cand {
                *e ^= 0xffff;
            }
        }
        let Some(&e) = cand[..cnt].last() else {
            k += 1;
            continue;
        };
        // Lengths from the longest down; each uses the cheapest distance among the candidates long enough,
        // which only changes where a cheaper candidate joins. Those changes are the steps: written over the
        // candidates already passed, from the end, as hi << 16 | distance (distance 0 costs infinity).
        let (mut bc, mut cur, mut ns) = (f32::INFINITY, e & !0xffff, 0);
        for i in (0..cnt).rev() {
            let e = cand[i];
            let p = price[dist_slot(e as u16)];
            if p < bc {
                bc = p;
                if e >> 16 != cur >> 16 {
                    cand[cnt - 1 - ns] = cur;
                    ns += 1;
                }
                cur = e;
            }
        }
        cand[cnt - 1 - ns] = cur;
        let steps = cand[cnt - 1 - ns..cnt]
            .iter()
            .map(|&s| ((s >> 16) as usize, c0 + price[dist_slot(s as u16)], (s & 0xffff) << 16));
        relax_matches(win, shift, o, steps);
        k += 1;
    }
    // The last literal, into the end.
    let end = &mut cells[n / 8 - base];
    if lit < end.cost.0[n % 8] {
        end.cost.0[n % 8] = lit;
        end.choice.0[n % 8] = pack(1, 0);
    }
    done.take(&cells[..=n / 8 - base]);
    // Walk back from the end.
    let choices = &done.choices[..];
    let first = out.len();
    let mut k = n;
    while k > 0 {
        let ch = choices[k / 8].0[k % 8];
        let (l, d) = (ch & 0xffff, ch >> 16);
        if d == 0 {
            out.push(Sym::lit(data[k - 1]));
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

/// `c.dist[dist_code(d)]` for every distance, by `dist_slot`: distances up to 256 one by one, longer ones in
/// steps of 128 (one distance code throughout a step).
fn dist_prices(c: &Costs) -> [f32; 768] {
    let mut p = [f32::INFINITY; 768];
    for (s, x) in p.iter_mut().enumerate() {
        let d = if s < 256 { s + 1 } else { ((s - 256) << 7) + 1 };
        if d <= WINDOW {
            *x = c.dist[dist_code(d)];
        }
    }
    p
}

#[inline(always)]
fn dist_slot(d: u16) -> usize {
    let x = (d as usize).wrapping_sub(1) & 0xffff;
    if x < 256 {
        x
    } else {
        256 + (x >> 7)
    }
}

/// The window groups for each lane `o` of a position, with `c.len` laid out as the windows see it.
fn shifts(c: &Costs) -> [[Shift; WG]; 8] {
    let mut t = [[Shift { len: Lanes([f32::INFINITY; 8]), lane: Lanes([0; 8]) }; WG]; 8];
    for (o, row) in t.iter_mut().enumerate() {
        for (g, group) in row.iter_mut().enumerate() {
            for j in 0..8 {
                let l = (8 * g + j) as i32 - o as i32;
                group.lane.0[j] = l;
                if (MIN_MATCH as i32..=MAX_MATCH as i32).contains(&l) {
                    group.len.0[j] = c.len[l as usize];
                }
            }
        }
    }
    t
}

/// Relaxes the match lengths of one position at lane `o`. `steps` covers them in order from `MIN_MATCH` as
/// `(hi, base, tag)`: the lengths after the previous step's `hi` up to this one's cost `base + len[l]` and are
/// chosen as `l | tag`. Each length is relaxed once, exactly as `if v < cost[l] { cost[l] = v; choice[l] =
/// l | tag }`, so the order among lengths does not matter: this goes group by group, through the steps that
/// touch a group (each lane takes its own) and then through the groups that lie wholly inside a step.
#[inline(always)]
fn relax_matches(
    win: &mut [Cell; WG],
    shift: &[Shift; WG],
    o: usize,
    mut steps: impl Iterator<Item = (usize, f32, u32)>,
) {
    let Some((mut hi, mut base, mut tag)) = steps.next() else {
        return;
    };
    // The group of length 3, and its longest length.
    let mut g = (o + MIN_MATCH) / 8;
    let mut last = 8 * g + 7 - o;
    loop {
        // The steps that touch group `g`: first which step each lane belongs to (its base and tag), then one
        // relaxation of the group. A loop in a loop on purpose: the compiler then makes vector code of the
        // work on one group (rather than working across groups). The lanes before the first step (lengths
        // under 3) cost infinity already.
        let lane = &shift[g].lane.0;
        let (mut bases, mut tags) = ([base; 8], [tag; 8]);
        let more = loop {
            if hi > last {
                // The step goes on in the next group.
                break true;
            }
            let lo = hi + 1;
            let Some(s) = steps.next() else {
                // No lengths past the last step.
                pick(&mut bases, &mut tags, lane, lo, f32::INFINITY, 0);
                break false;
            };
            (hi, base, tag) = s;
            if lo > last {
                break true;
            }
            pick(&mut bases, &mut tags, lane, lo, base, tag);
        };
        relax_lanes(&mut win[g], &shift[g], &bases, &tags);
        if !more {
            return;
        }
        // The next groups that end by `hi` lie wholly inside the step (it started by the end of group `g`).
        let whole = (hi - last) / 8;
        g += 1;
        last += 8;
        if whole == 0 {
            continue;
        }
        let end = g + whole;
        // Two groups at a time, then the odd one.
        let (cells, shifts) = (win[g..end].chunks_exact_mut(2), shift[g..end].chunks_exact(2));
        for (pair, sh) in cells.zip(shifts) {
            relax_whole(&mut pair[0], &sh[0], base, tag);
            relax_whole(&mut pair[1], &sh[1], base, tag);
        }
        if whole % 2 == 1 {
            relax_whole(&mut win[end - 1], &shift[end - 1], base, tag);
        }
        g = end;
        last += 8 * whole;
    }
}

/// The lanes from length `lo` on belong to a new step. (Picked with bit masks rather than `if`: written so,
/// the compiler makes a few vector instructions of it.)
#[inline(always)]
fn pick(bases: &mut [f32; 8], tags: &mut [u32; 8], lane: &[i32; 8], lo: usize, base: f32, tag: u32) {
    let (b, t) = (*bases, *tags);
    let (mut nb, mut nt) = ([0f32; 8], [0u32; 8]);
    for j in 0..8 {
        let m = 0u32.wrapping_sub((lane[j] >= lo as i32) as u32);
        nb[j] = f32::from_bits(base.to_bits() & m | b[j].to_bits() & !m);
        nt[j] = tag & m | t[j] & !m;
    }
    *bases = nb;
    *tags = nt;
}

/// One group, each lane with its own step: `cost = base + len` and `choice = lane | tag` where that is lower
/// than `cost`. Written with bit masks and without branches so that it is a few vector instructions.
#[inline(always)]
fn relax_lanes(cell: &mut Cell, shift: &Shift, bases: &[f32; 8], tags: &[u32; 8]) {
    let (c, h) = (cell.cost.0, cell.choice.0);
    let (mut nc, mut nh) = ([0f32; 8], [0u32; 8]);
    for j in 0..8 {
        let v = bases[j] + shift.len.0[j];
        let m = 0u32.wrapping_sub((v < c[j]) as u32);
        nc[j] = f32::from_bits(v.to_bits() & m | c[j].to_bits() & !m);
        nh[j] = (shift.lane.0[j] as u32 | tags[j]) & m | h[j] & !m;
    }
    cell.cost.0 = nc;
    cell.choice.0 = nh;
}

/// `relax_lanes` for a group whose lanes all lie inside one step. Along a long step few lengths improve, often
/// none: the group is only written back when one does (which also keeps the next position's reads of it from
/// waiting on a write).
#[inline(always)]
fn relax_whole(cell: &mut Cell, shift: &Shift, base: f32, tag: u32) {
    let (c, h) = (cell.cost.0, cell.choice.0);
    let (mut nc, mut nh, mut any) = ([0f32; 8], [0u32; 8], 0);
    for j in 0..8 {
        let v = base + shift.len.0[j];
        let m = 0u32.wrapping_sub((v < c[j]) as u32);
        any |= m;
        nc[j] = f32::from_bits(v.to_bits() & m | c[j].to_bits() & !m);
        nh[j] = (shift.lane.0[j] as u32 | tag) & m | h[j] & !m;
    }
    if any != 0 {
        cell.cost.0 = nc;
        cell.choice.0 = nh;
    }
}

/// Greedy parse: the longest match wherever there is one (closest distance), otherwise a literal.
pub fn greedy(data: &[u8], a: usize, b: usize, m: &Matches, out: &mut Vec<Sym>) {
    let mut i = a;
    while i < b {
        let maxl = (b - i).min(MAX_MATCH);
        let mut best: Option<(usize, usize)> = None;
        if maxl >= MIN_MATCH {
            // Every step: an alternative is never longer than an earlier step, so the chain's choice wins.
            for (ml, d) in m.at(i).iter().map(|s| s.get()) {
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

#[cfg(test)]
mod tests {
    use super::super::matcher::{find_matches, find_matches_keeping_all};
    use super::*;

    /// The parse as it was before the windows: one range of lengths at a time, through the accessors.
    #[allow(clippy::too_many_arguments)]
    fn reference(
        data: &[u8],
        a: usize,
        b: usize,
        m: &Matches,
        c: &Costs,
        alt: bool,
        nice: usize,
    ) -> (Vec<f32>, Vec<u32>, Vec<Sym>) {
        let n = b - a;
        let mut cost = vec![f32::INFINITY; n + 1];
        let mut choice = vec![0u32; n + 1];
        cost[0] = 0.0;
        let run_cost = c.len[MAX_MATCH] + c.dist[0];
        let mut skip_to = 0usize;
        for k in 0..n {
            if k < skip_to {
                continue;
            }
            let c0 = cost[k];
            if c0 == f32::INFINITY {
                continue;
            }
            let i = a + k;
            if k > MAX_MATCH && i + MAX_MATCH <= b && m.same_at(i) >= 3 && m.same_at(i - MAX_MATCH) >= 1 {
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
            let chain = crate::flate::matcher::pairs(m.chain_at(i));
            if chain.is_empty() {
                continue;
            }
            if nice <= MAX_MATCH {
                let longest = (chain[chain.len() - 1].0 as usize).min(maxl);
                if longest >= nice {
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
                let mut lo = MIN_MATCH;
                for &(ml, d) in &chain {
                    let hi = (ml as usize).min(maxl);
                    relax(cost_k, choice_k, &c.len, lo, hi, c0 + c.dist[dist_code(d as usize)], d);
                    lo = hi + 1;
                    if lo > maxl {
                        break;
                    }
                }
                continue;
            }
            let mut cand = [(0u16, 0f32, 0u16); 32];
            let mut cnt = 0;
            for &(ml, d) in m.walk_at(i).iter().take(32) {
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
                relax(cost_k, choice_k, &c.len, lo, l, c0 + bc, bd);
                l = lo - 1;
            }
        }
        let mut out = Vec::new();
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
        out.reverse();
        (cost, choice, out)
    }

    fn relax(cost: &mut [f32], choice: &mut [u32], len: &[f32], lo: usize, hi: usize, base: f32, d: u16) {
        let tag = (d as u32) << 16;
        let (cost, choice, len) = (&mut cost[lo..=hi], &mut choice[lo..=hi], &len[lo..=hi]);
        for (i, ((c, ch), &x)) in cost.iter_mut().zip(choice.iter_mut()).zip(len).enumerate() {
            let v = base + x;
            let better = v < *c;
            *c = if better { v } else { *c };
            *ch = if better { (lo + i) as u32 | tag } else { *ch };
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Text-like, random, copied, periodic (pixels) and long runs, in random pieces.
    fn sample(r: &mut Rng, len: usize) -> Vec<u8> {
        let mut d: Vec<u8> = Vec::with_capacity(len + 2000);
        while d.len() < len {
            let n = 1 + r.below(900);
            match r.below(6) {
                0 => (0..n).for_each(|_| d.push(b"etaoin shrdlu "[r.below(14)])),
                1 => (0..n).for_each(|_| d.push(r.next() as u8)),
                2 if d.len() > 10 => {
                    let from = r.below(d.len());
                    (0..n).for_each(|k| d.push(d[from + k % (d.len() - from)]));
                }
                3 => {
                    let p = 1 + r.below(4);
                    let px: Vec<u8> = (0..p).map(|_| r.next() as u8).collect();
                    (0..n).for_each(|k| d.push(px[k % p].wrapping_add((k / 97) as u8 & r.below(2) as u8)));
                }
                4 => {
                    let v = r.next() as u8;
                    (0..n + 600 * r.below(3)).for_each(|_| d.push(v));
                }
                _ => (0..n).for_each(|k| d.push((k % 7) as u8 + r.below(2) as u8)),
            }
        }
        d.truncate(len);
        d
    }

    /// Prices on a coarse grid (many ties) or fine-grained ones.
    fn costs(r: &mut Rng) -> Costs {
        let coarse = r.below(2) == 0;
        let v =
            |r: &mut Rng, lo: f32| if coarse { lo + r.below(4) as f32 } else { lo + r.below(1 << 20) as f32 / 65536.0 };
        let mut c = Costs { lit: [0.0; 256], len: [0.0; MAX_MATCH + 1], dist: [0.0; DIST_SYMS] };
        c.lit.iter_mut().for_each(|x| *x = v(r, 4.0));
        c.len[MIN_MATCH..].iter_mut().for_each(|x| *x = v(r, 2.0));
        c.dist.iter_mut().for_each(|x| *x = v(r, 1.0));
        c
    }

    #[test]
    fn every_distance_gets_its_own_code_price() {
        let mut c = Costs::fixed();
        c.dist.iter_mut().enumerate().for_each(|(k, x)| *x = 1.0 + k as f32);
        let p = dist_prices(&c);
        for d in 1..=WINDOW {
            assert_eq!(p[dist_slot(d as u16)].to_bits(), c.dist[dist_code(d)].to_bits(), "distance {d}");
        }
    }

    #[test]
    fn windowed_parse_matches_the_reference() {
        let mut r = Rng(0x9e37_79b9_7f4a_7c15);
        let mut sc = ParseScratch::default();
        for case in 0..300 {
            let len = 200 + r.below(12_000);
            let data = sample(&mut r, len);
            let from = if r.below(2) == 0 { 0 } else { r.below(len / 3) };
            let to = len - if r.below(2) == 0 { 0 } else { r.below(len / 3) };
            let alternatives = r.below(2) == 0;
            let depth = [16, 64, 400][r.below(3)];
            let m = find_matches(&data, from, to, depth, alternatives);
            // The reference reads every alternative the walk found, the parse only those that can win.
            let all = if alternatives { find_matches_keeping_all(&data, from, to, depth) } else { m.clone() };
            let c = match r.below(4) {
                0 => Costs::fixed(),
                _ => costs(&mut r),
            };
            for _ in 0..4 {
                let a = if r.below(2) == 0 { from } else { from + r.below(to - from) };
                let b = match r.below(3) {
                    0 => to,
                    1 => (a + 1 + r.below(20)).min(to),
                    _ => a + 1 + r.below(to - a),
                };
                let alt = r.below(2) == 0;
                let nice = [MAX_MATCH + 1, MAX_MATCH, 2, 3, 16, 64][r.below(6)];
                let (cost, choice, want) = reference(&data, a, b, &all, &c, alt, nice);
                let at = format!("case {case}: {a}..{b} of {from}..{to}, alt {alt}, nice {nice}");
                let check = |sc: &ParseScratch, got: &[Sym], which: &str| {
                    assert_eq!(got, &want[..], "{which} symbols, {at}");
                    for k in 0..=b - a {
                        let (gc, gh) = (sc.done.costs[k / 8].0[k % 8], sc.done.choices[k / 8].0[k % 8]);
                        assert_eq!(gc.to_bits(), cost[k].to_bits(), "{which} cost at {k}, {at}");
                        assert_eq!(gh, choice[k], "{which} choice at {k}, {at}");
                    }
                };
                // The plain build, then the dispatching entry (the AVX2 build where the processor has it).
                let mut got = Vec::new();
                parse(&data, a, b, &m, &c, alt, nice, &mut sc, &mut got);
                check(&sc, &got, "plain");
                got.clear();
                optimal_nice(&data, a, b, &m, &c, alt, nice, &mut sc, &mut got);
                check(&sc, &got, "dispatched");
            }
        }
    }
}
