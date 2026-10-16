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
use super::parse::{greedy, optimal_nice, Costs, ParseScratch};
use super::tables::*;
use crate::clock::{self, Deadline};
use crate::par::{self, Memo, Pool};
use std::cell::Cell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    /// Matches at least this long are taken at once (259 = never).
    pub nice: usize,
    /// Merge/split polishing of block boundaries.
    pub polish: bool,
    /// Threads and earlier results shared by the encodes of one job. Never changes the output.
    pub shared: Option<Arc<Shared>>,
    /// Once this has passed, the segments still to do are written stored (cheap) so the job ends soon; the
    /// caller checks the deadline afterwards and never uses such an output. Never changes a finished output.
    pub deadline: Deadline,
}

impl Options {
    /// Our strongest setting.
    pub fn max() -> Options {
        Options {
            iterations: 8,
            exact_rounds: 4,
            max_chain: 8192,
            segment: 1 << 20,
            split: true,
            alternatives: true,
            multi_start_limit: 1 << 18,
            nice: 259,
            polish: true,
            shared: None,
            deadline: Deadline::NONE,
        }
    }
    /// Fast but still optimal-parsed.
    pub fn quick() -> Options {
        Options {
            iterations: 2,
            exact_rounds: 1,
            max_chain: 128,
            segment: 1 << 20,
            split: true,
            alternatives: false,
            multi_start_limit: 0,
            nice: 64,
            polish: false,
            shared: None,
            deadline: Deadline::NONE,
        }
    }

    /// Settings for an effort from 0 (`quick`) to 100 (`max`), in steps of 20. Measured on 92 sampled PDF
    /// streams (3.4 MB) and on 6 large images (14.9 MB), against `max` (CPU time; bytes):
    ///   0: 0.34x, 0.23x; +0.47%, +1.75%      20: 0.43x, 0.39x; +0.15%, +0.10%
    ///  40: 0.47x, 0.45x; +0.13%, +0.09%      60: 0.77x, 0.71x; +0.03%, +0.00%      80: 0.93x, 0.90x; +0.01%, +0.00%
    pub fn effort(effort: u8) -> Options {
        let o = |iterations, exact_rounds, max_chain, alternatives, polish| Options {
            iterations,
            exact_rounds,
            max_chain,
            segment: 1 << 20,
            split: true,
            alternatives,
            multi_start_limit: 0,
            nice: 259,
            polish,
            shared: None,
            deadline: Deadline::NONE,
        };
        match effort {
            0..=19 => Options::quick(),
            20..=39 => o(2, 1, 1024, false, false),
            40..=59 => o(3, 1, 1024, false, false),
            60..=79 => o(4, 2, 2048, true, true),
            80..=99 => o(6, 3, 4096, true, true),
            _ => Options::max(),
        }
    }

    /// Whether two settings give the same output (`shared` and `deadline` never change it).
    pub fn same_output(&self, other: &Options) -> bool {
        self.fingerprint() == other.fingerprint()
    }

    /// The settings that decide the output (everything but `shared` and `deadline`).
    fn fingerprint(&self) -> [usize; 9] {
        [
            self.iterations,
            self.exact_rounds,
            self.max_chain,
            self.segment,
            self.split as usize,
            self.alternatives as usize,
            self.multi_start_limit,
            self.nice,
            self.polish as usize,
        ]
    }
}

/// What the encodes of one job share: a pool of threads, and (optionally) the output for inputs already
/// encoded with the same settings, so that an input is encoded once. Neither changes any output.
pub struct Shared {
    pub pool: Arc<Pool>,
    /// Kept as long as this `Shared` lives; None: nothing is remembered.
    memo: Option<Memo<MemoKey, Arc<Done>>>,
    /// The encoding time the remembered outputs saved, in nanoseconds (for the debug figures).
    saved: AtomicU64,
    /// Segments written from the plans of an earlier one like them (for the debug figures).
    reused_segments: AtomicUsize,
}

/// What an encoded input is remembered by: the hash and length of the data and the settings that matter.
type MemoKey = (u64, usize, [usize; 9]);

/// A remembered output, and how long it took to make (for the debug figures only).
struct Done {
    z: Vec<u8>,
    took: Duration,
}

/// What a store of outputs has done and holds (debug figures; they never change an output).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// Outputs given again, and outputs made.
    pub hits: usize,
    pub computed: usize,
    /// Outputs kept now, and the most bytes they ever took ([`par::ENTRY_COST`] each included).
    pub kept: usize,
    pub peak: usize,
    /// About how much encoding time the outputs given again saved.
    pub saved: Duration,
    /// Segments written from the plans of an earlier segment like them, so not planned again (counted with or
    /// without a store).
    pub reused_segments: usize,
}

impl Shared {
    /// A new pool of `threads` threads, remembering every output.
    pub fn new(threads: usize) -> Arc<Shared> {
        Shared::with_pool(Pool::new(threads), true)
    }

    /// Encodes that use `pool`; with `remember`, every output is kept for later encodes of the same input
    /// while this `Shared` lives (one per stream, say, so memory doesn't grow with the file).
    pub fn with_pool(pool: Arc<Pool>, remember: bool) -> Arc<Shared> {
        Shared::with_memo(pool, remember.then(Memo::new))
    }

    /// Encodes that use `pool` and one store of outputs (one per file, say): outputs are kept while they
    /// take at most `budget` bytes in all, [`par::ENTRY_COST`] each included, and the oldest go first. 0: no
    /// store at all.
    pub fn with_store(pool: Arc<Pool>, budget: usize) -> Arc<Shared> {
        Shared::with_memo(pool, (budget > 0).then(|| Memo::bounded(budget, |d: &Arc<Done>| d.z.len())))
    }

    fn with_memo(pool: Arc<Pool>, memo: Option<Memo<MemoKey, Arc<Done>>>) -> Arc<Shared> {
        Arc::new(Shared { pool, memo, saved: AtomicU64::new(0), reused_segments: AtomicUsize::new(0) })
    }

    /// What the store of outputs has done and holds (zero without one), and how many segments were not
    /// planned again.
    pub fn stats(&self) -> StoreStats {
        let m = self.memo.as_ref().map(|m| m.stats()).unwrap_or_default();
        StoreStats {
            hits: m.hits,
            computed: m.computed,
            kept: m.kept,
            peak: m.peak,
            saved: Duration::from_nanos(self.saved.load(Ordering::Relaxed)),
            reused_segments: self.reused_segments.load(Ordering::Relaxed),
        }
    }
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Shared({:?})", self.pool)
    }
}

/// Segments optimised together before their blocks are written (bounds the memory held by finished plans).
const SEGMENTS_AT_ONCE: usize = 8;

/// Raw Deflate data for `data`.
pub fn deflate(data: &[u8], opt: &Options) -> Vec<u8> {
    deflate_with(data, opt, true)
}

/// Whether two segments get the same plans: the same length, the same history length (`min(a, WINDOW)`) and
/// the same bytes from the start of their history to their end. Planning a segment reads nothing else:
/// - `find_matches` reads only `data[a - h..b]` and makes its tables afresh for each call; its tree slots
///   (position mod 64 KiB) only hold nodes, and the steps it gives hold lengths and distances;
/// - the parses read `data[a..b]` and those steps by their offset from `a`, so the blocks they give have the
///   same symbols, and bounds at the same offsets from `a`.
///
/// The matcher keeps positions in 32 bits, so this holds only for segments that end by 4 GiB.
fn same_segment(data: &[u8], (a, b): (usize, usize), (c, d): (usize, usize)) -> bool {
    let (h, k) = (a.min(WINDOW), c.min(WINDOW));
    u32::try_from(b.max(d)).is_ok() && b - a == d - c && h == k && data[a - h..b] == data[c - k..d]
}

/// `deflate`. With `reuse`, a segment like an earlier one in its group (`same_segment`) is not planned again
/// but written from that one's plans (a blank band of an image, say); the output is the same either way.
fn deflate_with(data: &[u8], opt: &Options, reuse: bool) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(data.len() / 2 + 64);
    if data.is_empty() {
        write_fixed(&mut w, &[], true);
        return w.finish();
    }
    let seg = opt.segment.max(1);
    let bounds: Vec<(usize, usize)> = (0..data.len()).step_by(seg).map(|a| (a, (a + seg).min(data.len()))).collect();
    let dbg = std::env::var_os("PDFE_DEBUG").is_some();
    let pool = opt.shared.as_ref().map(|s| &*s.pool);
    // Segments are optimised independently (in parallel when threads are free) and written in order, so the
    // output is the same whatever the number of threads.
    for group in bounds.chunks(SEGMENTS_AT_ONCE) {
        // For each segment, the first segment of the group like it (itself when there is none). Only those
        // are planned.
        let mut first: Vec<usize> = Vec::with_capacity(group.len());
        for k in 0..group.len() {
            let like = (0..k).find(|&j| reuse && first[j] == j && same_segment(data, group[j], group[k]));
            first.push(like.unwrap_or(k));
        }
        let todo: Vec<usize> = (0..group.len()).filter(|&k| first[k] == k).collect();
        let reused = group.len() - todo.len();
        if reused > 0 {
            if let Some(sh) = &opt.shared {
                sh.reused_segments.fetch_add(reused, Ordering::Relaxed);
            }
            if dbg {
                let (a, b) = (group[0].0, group[group.len() - 1].1);
                eprintln!("segments {a}..{b}: {reused} like an earlier one, planned once");
            }
        }
        let plans = par::map(pool, todo.len(), None, |t| {
            let (a, b) = group[todo[t]];
            // Out of time: this segment goes out stored, and the caller drops the whole output.
            if opt.deadline.passed() {
                return None;
            }
            let t0 = clock::now();
            let m = find_matches(data, a, b, opt.max_chain, opt.alternatives);
            let t1 = clock::since(t0);
            let plans = optimize_segment(data, a, b, &m, opt);
            if dbg {
                eprintln!(
                    "segment {}..{}: matches {:?} ({} steps), optimise {:?}, {} blocks",
                    a,
                    b,
                    t1,
                    m.steps.len(),
                    clock::since(t0).saturating_sub(t1),
                    plans.len()
                );
            }
            Some(plans)
        });
        for (k, &(a, b)) in group.iter().enumerate() {
            let last_segment = b == data.len();
            // The plans of `first[k]`, which is in `todo`. Their stored blocks read that segment's bytes,
            // which are this one's.
            match &plans[todo.partition_point(|&t| t < first[k])] {
                Some(plans) => {
                    let n = plans.len();
                    for (i, p) in plans.iter().enumerate() {
                        write_best(&mut w, data, p, last_segment && i + 1 == n);
                    }
                }
                None => write_stored(&mut w, &data[a..b], last_segment),
            }
        }
    }
    w.finish()
}

/// The largest input whose output is remembered. Repeated inputs are small (the largest repeat in the sample
/// corpus is 152,965 bytes), and an input this small is one segment, so encoding it on one thread (as a
/// memo's compute does, see `par::Memo`) costs no parallelism.
pub(crate) const REMEMBER_UP_TO: usize = 256 << 10;

/// Zlib-wrapped data, as PDF's FlateDecode expects. With a remembering `shared`, an input already encoded with
/// the same settings is not encoded again: the earlier output is reused after checking that it decodes to
/// this input. Only inputs up to `REMEMBER_UP_TO` bytes and one segment are remembered, and never an output
/// made after the deadline.
pub fn deflate_zlib(data: &[u8], opt: &Options) -> Vec<u8> {
    let encode = || super::zlib::wrap(&deflate(data, opt), data);
    let Some(sh) = opt.shared.as_deref() else { return encode() };
    let memo = match &sh.memo {
        Some(memo) if data.len() <= REMEMBER_UP_TO.min(opt.segment.max(1)) => memo,
        _ => return encode(),
    };
    let key = (par::hash64(data), data.len(), opt.fingerprint());
    let fits =
        |d: &Arc<Done>| super::zlib::unwrap(&d.z, data.len() + 1).is_ok_and(|u| u.data == data && u.trailing == 0);
    let made = Cell::new(false);
    let done = memo.get_kept(key, fits, || {
        made.set(true);
        let t = clock::now();
        let z = encode();
        // Past the deadline some segments may have gone out stored: such an output is never kept.
        (Arc::new(Done { z, took: clock::since(t) }), !opt.deadline.passed())
    });
    if !made.get() {
        let took = u64::try_from(done.took.as_nanos()).unwrap_or(u64::MAX);
        sh.saved.fetch_add(took, Ordering::Relaxed);
    }
    done.z.to_vec()
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
    let t = clock::now();
    let mut sc = ParseScratch::default();
    // 1. The whole segment, from neutral prices and (for smaller segments) also from a greedy parse's
    //    statistics; the better result goes on.
    let mut first = Vec::new();
    optimal_nice(data, a, b, m, &Costs::fixed(), false, opt.nice, &mut sc, &mut first);
    let mut syms = refine(data, a, b, m, opt, first, &mut sc);
    if b - a <= opt.multi_start_limit {
        let mut g = Vec::new();
        greedy(data, a, b, m, &mut g);
        let mut second = Vec::new();
        optimal_nice(data, a, b, m, &Costs::entropy(&Hist::of(&g)), false, opt.nice, &mut sc, &mut second);
        let other = refine(data, a, b, m, opt, second, &mut sc);
        if price(&Hist::of(&other), b - a) < price(&Hist::of(&syms), b - a) {
            syms = other;
        }
    }
    let t1 = clock::since(t);
    // 2. Split into blocks.
    let mut plans = if opt.split { split(data, a, b, syms) } else { vec![Plan { a, b, syms }] };
    let t2 = clock::since(t);
    // 3. Each block under its own codes.
    for p in plans.iter_mut() {
        let s = std::mem::take(&mut p.syms);
        let s = refine(data, p.a, p.b, m, opt, s, &mut sc);
        p.syms = exact(data, p.a, p.b, m, opt, s, &mut sc);
    }
    let t3 = clock::since(t);
    // 4. Polish: merge neighbours that are cheaper together, split blocks that are cheaper apart.
    if opt.split && opt.polish {
        polish(data, m, opt, &mut plans, &mut sc);
    }
    if dbg {
        eprintln!("  global {:?}, split {:?}, blocks {:?}, polish {:?}", t1, t2 - t1, t3 - t2, clock::since(t) - t3);
    }
    plans
}

fn plan_price(p: &Plan) -> u64 {
    price(&Hist::of(&p.syms), p.b - p.a)
}

fn polish(data: &[u8], m: &Matches, opt: &Options, plans: &mut Vec<Plan>, sc: &mut ParseScratch) {
    // A merge or split that was turned down is turned down again while its blocks are the same, so the
    // second pass skips those (blocks get a new id whenever they change); the result is unchanged.
    let mut ids: Vec<usize> = (0..plans.len()).collect();
    let mut next_id = plans.len();
    let mut no_merge: HashSet<(usize, usize)> = HashSet::new();
    let mut no_split: HashSet<usize> = HashSet::new();
    for _pass in 0..2 {
        let mut changed = false;
        // Merges.
        let mut k = 0;
        while k + 1 < plans.len() {
            if no_merge.contains(&(ids[k], ids[k + 1])) {
                k += 1;
                continue;
            }
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
                    ids[k] = next_id;
                    ids.remove(k + 1);
                    next_id += 1;
                    changed = true;
                    continue;
                }
            }
            no_merge.insert((ids[k], ids[k + 1]));
            k += 1;
        }
        // Splits: try the best single cut of each block.
        let mut k = 0;
        while k < plans.len() {
            let p = &plans[k];
            let n = p.syms.len();
            if n < 64 || no_split.contains(&ids[k]) {
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
                let v = estimate_dynamic_bits(&left).min(fixed_bits(&left))
                    + estimate_dynamic_bits(&right).min(fixed_bits(&right));
                if v < best.0 {
                    best = (v, c);
                }
            }
            if best.1 == 0 || best.0 + 6 >= whole {
                no_split.insert(ids[k]);
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
                ids[k] = next_id;
                ids.insert(k + 1, next_id + 1);
                next_id += 2;
                changed = true;
                k += 2;
            } else {
                no_split.insert(ids[k]);
                k += 1;
            }
        }
        if !changed {
            break;
        }
    }
}

/// Entropy-priced rounds: price symbols by how often the current parse uses them, re-parse, repeat.
fn refine(
    data: &[u8],
    a: usize,
    b: usize,
    m: &Matches,
    opt: &Options,
    start: Vec<Sym>,
    sc: &mut ParseScratch,
) -> Vec<Sym> {
    let mut best_bits = price(&Hist::of(&start), b - a);
    let mut best = start;
    let mut stats = Hist::of(&best);
    let mut last_bits = best_bits;
    let mut cur = Vec::with_capacity(best.len());
    let mut stuck = 0;
    for it in 0..opt.iterations {
        cur.clear();
        optimal_nice(data, a, b, m, &Costs::entropy(&stats), false, opt.nice, sc, &mut cur);
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
fn exact(
    data: &[u8],
    a: usize,
    b: usize,
    m: &Matches,
    opt: &Options,
    start: Vec<Sym>,
    sc: &mut ParseScratch,
) -> Vec<Sym> {
    let mut best_bits = price(&Hist::of(&start), b - a);
    let mut best = start;
    let mut cur = Vec::with_capacity(best.len());
    // The prices (and `alt`) of this round's parses and the last round's, by round parity and variant. The
    // same again give the same parse, which was priced then and so cannot beat `best_bits` now: it is not
    // made again. Without alternatives the third variant is the first, and a round whose code is the last
    // round's repeats it whole. Kept on the stack.
    let mut tried: [Option<(bool, Costs)>; 6] = Default::default();
    for round in 0..opt.exact_rounds {
        let h = Hist::of(&best);
        let code = best_dynamic(&h);
        let maxlen = *code.ll.iter().max().unwrap_or(&15) as f32;
        let mut improved = false;
        let variants = [(maxlen + 1.0, false), (maxlen + 4.0, false), (maxlen + 1.0, opt.alternatives)];
        for (k, (missing, alt)) in variants.into_iter().enumerate() {
            if !alt && missing > maxlen + 1.0 && opt.alternatives {
                // keep the variety small when alternatives are tried
            }
            let costs = Costs::lengths(&code.ll, &code.d, missing);
            if tried.iter().flatten().any(|t| t.0 == alt && same_costs(&t.1, &costs)) {
                continue;
            }
            cur.clear();
            optimal_nice(data, a, b, m, &costs, alt, opt.nice, sc, &mut cur);
            let bits = price(&Hist::of(&cur), b - a);
            if bits < best_bits {
                best_bits = bits;
                best.clone_from(&cur);
                improved = true;
            }
            tried[round % 2 * 3 + k] = Some((alt, costs));
        }
        if !improved {
            break;
        }
    }
    best
}

/// Whether two sets of prices are the same bit for bit (then a parse gives the same symbols under either).
fn same_costs(x: &Costs, y: &Costs) -> bool {
    fn bits(c: &Costs) -> impl Iterator<Item = u32> + '_ {
        c.lit.iter().chain(&c.len).chain(&c.dist).map(|v| v.to_bits())
    }
    bits(x).eq(bits(y))
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
            let v = estimate_dynamic_bits(&left).min(fixed_bits(&left))
                + estimate_dynamic_bits(&right).min(fixed_bits(&right));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Noise of 23 letters, so that short matches abound but long ones do not.
    fn letters(s: &mut u64, n: usize) -> Vec<u8> {
        (0..n)
            .map(|_| {
                *s ^= *s << 13;
                *s ^= *s >> 7;
                *s ^= *s << 17;
                (*s % 23) as u8 + b'a'
            })
            .collect()
    }

    /// Segments [head, u, u, u, other, u] of `seg` bytes each: 1, 2, 3 and 5 hold the same bytes, but only 2
    /// and 3 have the same history too.
    fn units(seg: usize) -> Vec<u8> {
        let mut s = 9u64;
        let (head, unit, other) = (letters(&mut s, seg), letters(&mut s, seg), letters(&mut s, seg));
        [&head[..], &unit, &unit, &unit, &other, &unit].concat()
    }

    #[test]
    fn a_segment_like_an_earlier_one_has_the_same_plans() {
        for seg in [1 << 16, 40_000] {
            let data = units(seg);
            let at = |k: usize| (k * seg, (k + 1) * seg);
            assert!(same_segment(&data, at(2), at(3)));
            for (j, k) in [(0, 1), (1, 2), (1, 3), (1, 5), (2, 5), (3, 5)] {
                assert!(!same_segment(&data, at(j), at(k)), "segments {j} and {k}");
            }
            for opt in [Options::quick(), Options::effort(60)] {
                let opt = Options { segment: seg, ..opt };
                let plans = |k: usize| {
                    let (a, b) = at(k);
                    let m = find_matches(&data, a, b, opt.max_chain, opt.alternatives);
                    optimize_segment(&data, a, b, &m, &opt)
                };
                let (p2, p3) = (plans(2), plans(3));
                assert_eq!(p2.len(), p3.len());
                for (x, y) in p2.iter().zip(&p3) {
                    assert_eq!((x.a + seg, x.b + seg), (y.a, y.b));
                    assert!(x.syms == y.syms, "the same bytes and history gave another parse");
                }
                // The plans use the history: some matches reach back before the segment.
                let mut at_byte = 0;
                let mut back = 0;
                for s in p2.iter().flat_map(|p| &p.syms) {
                    back += (s.dist as usize > at_byte) as usize;
                    at_byte += s.span();
                }
                assert!(back > 0, "{seg} {opt:?}");
            }
        }
        // With 40,000-byte segments, 2 and 3 sit at other offsets in the matcher's 64 KiB cycle of tree slots.
        assert_ne!(2 * 40_000 % (2 * WINDOW), 3 * 40_000 % (2 * WINDOW));
    }

    #[test]
    fn reuse_gives_the_bytes_of_no_reuse() {
        // (input, segment size, segments written from an earlier one's plans)
        let mut cases: Vec<(Vec<u8>, usize, usize)> = Vec::new();
        for seg in [40_000, 1 << 20] {
            // The second segment is 32 KiB shorter than the first, so its history and bytes together are as
            // long as the first's bytes, and all zero too; but it is not like the first.
            cases.push((vec![0; seg + seg - WINDOW], seg, 0));
        }
        cases.push((vec![0; 3 << 20], 1 << 20, 1));
        let seg = 40_000;
        // Segment 0 has no history; 1 to 7 are alike, and so are 8 to 15, but not across groups.
        cases.push((vec![0; 8 * seg], seg, 6));
        cases.push((vec![0; 17 * seg], seg, 6 + 7));
        // Segments shorter than the window: the first four have shorter histories than the rest.
        cases.push((vec![0; 12 * 10_000], 10_000, 3 + 3));
        // Noise in segments just over the window, so that each history lies in the segment before.
        let seg = WINDOW + 232;
        cases.push((units(seg), seg, 1));
        // [head, u, u, x, u]: x is u with one byte changed, at the first byte of the last segment's history or
        // just before it.
        let mut s = 3u64;
        let (head, u) = (letters(&mut s, seg), letters(&mut s, seg));
        for (at, like) in [(seg - WINDOW, 0), (seg - WINDOW - 1, 1)] {
            let mut x = u.clone();
            x[at] ^= 1;
            cases.push(([&head[..], &u, &u, &x, &u].concat(), seg, like));
        }
        // A unit of 10,000 bytes repeated after a head: from segment 5 on, the history lies in the repeats too,
        // so 6 and 7 are like 5, and 9 and 10 like 8 in the next group.
        let seg = 10_000;
        cases.push(([&head[..seg], &u[..seg].repeat(10)].concat(), seg, 2 + 2));
        for (n, (data, seg, like)) in cases.iter().enumerate() {
            let opt = Options { segment: *seg, ..Options::quick() };
            let want = deflate_with(data, &opt, false);
            assert_eq!(crate::flate::inflate(&want, data.len() + 1).unwrap().data, *data, "case {n}");
            // Threads and jitter seeds (0: no jitter).
            for (t, seed) in [(1, 0), (2, 1), (2, 2), (4, 3), (4, 4)] {
                let sh = Shared::new(t);
                let o = Options { shared: Some(sh.clone()), ..opt.clone() };
                let got = par::jitter::with(seed, || deflate_with(data, &o, true));
                assert!(got == want, "case {n}, {t} threads, seed {seed}");
                assert_eq!(sh.stats().reused_segments, *like, "case {n}");
            }
        }
    }

    /// `exact` with every variant of every round parsed afresh, and how many of those parses had the prices
    /// and `alt` of an earlier one in the same call (checked to give the same symbols).
    fn exact_every_parse(
        data: &[u8],
        a: usize,
        b: usize,
        m: &Matches,
        opt: &Options,
        start: Vec<Sym>,
    ) -> (Vec<Sym>, usize) {
        let mut sc = ParseScratch::default();
        let mut best_bits = price(&Hist::of(&start), b - a);
        let mut best = start;
        let mut made: Vec<(bool, Costs, Vec<Sym>)> = Vec::new();
        let mut repeats = 0;
        for _ in 0..opt.exact_rounds {
            let code = best_dynamic(&Hist::of(&best));
            let maxlen = *code.ll.iter().max().unwrap_or(&15) as f32;
            let mut improved = false;
            for (missing, alt) in [(maxlen + 1.0, false), (maxlen + 4.0, false), (maxlen + 1.0, opt.alternatives)] {
                let costs = Costs::lengths(&code.ll, &code.d, missing);
                let mut cur = Vec::new();
                optimal_nice(data, a, b, m, &costs, alt, opt.nice, &mut sc, &mut cur);
                if let Some(e) = made.iter().find(|e| e.0 == alt && same_costs(&e.1, &costs)) {
                    assert!(e.2 == cur, "the same prices gave another parse");
                    repeats += 1;
                }
                let bits = price(&Hist::of(&cur), b - a);
                if bits < best_bits {
                    best_bits = bits;
                    best.clone_from(&cur);
                    improved = true;
                }
                made.push((alt, costs, cur));
            }
            if !improved {
                break;
            }
        }
        (best, repeats)
    }

    fn samples() -> Vec<Vec<u8>> {
        let mut s = 7u64;
        let mut rng = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let text = b"BT /F1 12 Tf 72 712 Td [(Hello)-250(World)]TJ ET q 0.5 0 0 0.5 0 0 cm /Im1 Do Q\n".repeat(60);
        let mut mixed = Vec::new();
        for k in 0..30_000usize {
            let v = rng();
            mixed.push(if k % 1000 < 600 { (v % 5) as u8 + b'a' } else { (v >> 8) as u8 % 40 });
        }
        let mut rows = Vec::new();
        for y in 0..120u32 {
            for x in 0..200u32 {
                rows.push(((x * x + y * 3) / 7 % 256) as u8 ^ (rng() % 3) as u8);
            }
        }
        vec![b"abcabcabd".to_vec(), text, mixed, rows]
    }

    #[test]
    fn exact_skips_only_parses_it_has_made() {
        let opts = [
            Options::quick(),
            Options::effort(50),
            Options::effort(60),
            Options::max(),
            Options { exact_rounds: 6, alternatives: false, ..Options::max() },
        ];
        let mut repeats = [0usize; 2];
        for opt in &opts {
            for data in samples() {
                let n = data.len();
                let m = find_matches(&data, 0, n, opt.max_chain, opt.alternatives);
                for (a, b) in [(0, n), (0, n / 2), (n / 3, n)] {
                    let mut sc = ParseScratch::default();
                    let mut first = Vec::new();
                    optimal_nice(&data, a, b, &m, &Costs::fixed(), false, opt.nice, &mut sc, &mut first);
                    let start = refine(&data, a, b, &m, opt, first, &mut sc);
                    let got = exact(&data, a, b, &m, opt, start.clone(), &mut sc);
                    let (want, r) = exact_every_parse(&data, a, b, &m, opt, start);
                    assert!(got == want, "{opt:?} on {a}..{b} of {n} bytes");
                    repeats[opt.alternatives as usize] += r;
                }
            }
        }
        // Parses were skipped both without alternatives and with them.
        assert!(repeats[0] > 0 && repeats[1] > 0, "{repeats:?}");
    }
}
