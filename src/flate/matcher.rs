//! Finds, for every position, the smallest distance for every possible match length.
//!
//! The result is a step function per position: lengths up to a step's length can use its distance, with
//! lengths and distances both increasing (followed, with alternatives, by farther matches). The optimal parser
//! reads it to price every length.

use super::tables::*;

const HASH_BITS: u32 = 16;
const HASH_SIZE: usize = 1 << HASH_BITS;

/// One step in three bytes: its length less 3 (lengths are 3 to 258), then its distance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Step([u8; 3]);

impl Step {
    #[inline(always)]
    fn new(len: usize, dist: usize) -> Step {
        let [lo, hi] = (dist as u16).to_le_bytes();
        Step([(len - MIN_MATCH) as u8, lo, hi])
    }
    /// The length and the distance.
    #[inline(always)]
    pub fn get(self) -> (u16, u16) {
        (self.0[0] as u16 + MIN_MATCH as u16, u16::from_le_bytes([self.0[1], self.0[2]]))
    }
}

/// Steps as (length, distance) pairs.
#[cfg(test)]
pub fn pairs(steps: &[Step]) -> Vec<(u16, u16)> {
    steps.iter().map(|s| s.get()).collect()
}

/// How many steps of each walk (the first it finds) the parse weighs with alternatives. Which alternatives are
/// dropped as never winning depends on it.
pub const CANDIDATES: usize = 32;

/// The run lengths the parser tells apart (see `Matches::same`).
pub const RUNS: [usize; 4] = [MAX_MATCH, MAX_MATCH + 7, 2 * MAX_MATCH, 2 * MAX_MATCH + 7];

#[cfg_attr(test, derive(Clone))]
pub struct Matches {
    /// Position `from + k` has steps `steps[start[k]..start[k + 1]]`.
    pub(crate) start: Vec<u32>,
    /// Each position's chain first: the steps longer than every closer one (the smallest distance for each
    /// length), lengths and distances increasing. With alternatives, the farther matches that are no longer
    /// than a closer step follow it: by length (of equal lengths the farther first) when the walk found at
    /// most `CANDIDATES` steps, otherwise by distance. In order of distance, the two together are the order the walk
    /// found them in.
    pub(crate) steps: Vec<Step>,
    /// Whether the steps include alternatives; without them every step is in the chain.
    pub(crate) alternatives: bool,
    /// With alternatives: how many of each position's steps (at the end) are alternatives. Empty without.
    pub(crate) farther: Vec<u8>,
    /// How far the run of one byte from each position goes (itself included, cut at `to`), as the number of
    /// `RUNS` lengths it is longer than: all the parser asks of it.
    pub(crate) same: Vec<u8>,
    pub(crate) from: usize,
}

impl Matches {
    #[inline(always)]
    pub fn at(&self, pos: usize) -> &[Step] {
        let k = pos - self.from;
        &self.steps[self.start[k] as usize..self.start[k + 1] as usize]
    }
    /// The steps longer than every closer one: for each length, the smallest distance that reaches it.
    #[inline(always)]
    pub fn chain_at(&self, pos: usize) -> &[Step] {
        let steps = self.at(pos);
        if self.alternatives {
            &steps[..steps.len() - self.farther[pos - self.from] as usize]
        } else {
            steps
        }
    }
    /// Every step in the order the walk found them (ever farther).
    #[cfg(test)]
    pub fn walk_at(&self, pos: usize) -> Vec<(u16, u16)> {
        let mut all = pairs(self.at(pos));
        all.sort_by_key(|s| s.1);
        all
    }
    #[inline(always)]
    pub fn same_at(&self, pos: usize) -> u8 {
        self.same[pos - self.from]
    }
}

#[inline]
fn hash3(d: &[u8], i: usize) -> usize {
    let v = d[i] as u32 | (d[i + 1] as u32) << 8 | (d[i + 2] as u32) << 16;
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

/// Length of the common prefix of `d[a..]` and `d[b..]`, at most `max`, starting the comparison at `skip`.
#[inline]
fn common(d: &[u8], a: usize, b: usize, skip: usize, max: usize) -> usize {
    // Both sides are cut to `max` once, so the word and byte reads below need no bounds checks.
    let (x, y) = (&d[a..a + max], &d[b..b + max]);
    let mut n = skip;
    for (u, v) in x[skip..].as_chunks::<8>().0.iter().zip(y[skip..].as_chunks::<8>().0) {
        let z = u64::from_le_bytes(*u) ^ u64::from_le_bytes(*v);
        if z != 0 {
            return n + (z.trailing_zeros() / 8) as usize;
        }
        n += 8;
    }
    while n < max && x[n] == y[n] {
        n += 1;
    }
    n
}

/// Matches for positions `from..to`. Matches never reach past `to` and look back at most 32 KiB
/// (into data before `from` too). `max_depth` bounds how many earlier positions are compared.
///
/// Positions are kept in binary search trees (one per 3-byte hash), ordered by the text that follows them
/// and with newer positions above older ones. Walking down a tree visits ever older positions with ever
/// longer common prefixes, which gives exactly the smallest distance for each length. Each comparison
/// starts after the prefix already known to match, so repetitive data stays fast.
pub fn find_matches(data: &[u8], from: usize, to: usize, max_depth: usize, alternatives: bool) -> Matches {
    // Two builds of the search: without alternatives the distance buckets are never looked at.
    if alternatives {
        search::<true>(data, from, to, max_depth, false)
    } else {
        search::<false>(data, from, to, max_depth, false)
    }
}

/// `find_matches` with alternatives, keeping the ones that never win as well (to prove they never do).
#[cfg(test)]
pub fn find_matches_keeping_all(data: &[u8], from: usize, to: usize, max_depth: usize) -> Matches {
    search::<true>(data, from, to, max_depth, true)
}

fn search<const ALT: bool>(data: &[u8], from: usize, to: usize, max_depth: usize, keep_all: bool) -> Matches {
    const NIL: u32 = u32::MAX;
    let hist = from.saturating_sub(WINDOW);
    let n = to - from;
    let mut start = Vec::with_capacity(n + 1);
    let mut farther_count = if ALT { vec![0u8; n] } else { Vec::new() };
    let mut steps: Vec<Step> = Vec::with_capacity(n * 2);

    // Only the segment's own positions: each run looks forward, never back into the history.
    let mut same = vec![0u8; n];
    let mut run = 0;
    for i in (from..to).rev() {
        run = if i + 1 < to && data[i + 1] == data[i] { run + 1 } else { 1 };
        same[i - from] = RUNS.iter().map(|&t| (run > t) as u8).sum();
    }

    // Tree slots cycle every 64 KiB: twice the window, so the node exactly 32 KiB back (a legal distance)
    // never shares a slot with the position being inserted.
    const CYC: usize = 2 * WINDOW;
    const CMASK: usize = CYC - 1;
    let mut head = vec![NIL; HASH_SIZE];
    let mut son = vec![NIL; 2 * CYC];
    // A position's alternatives, put after its chain once the walk is done, each marked once a longer step
    // with the same distance code follows it.
    let mut farther: Vec<(Step, bool)> = Vec::new();
    for i in hist..to {
        let searching = i >= from;
        if searching {
            start.push(steps.len() as u32);
        }
        let lim = (to - i).min(MAX_MATCH);
        if lim < MIN_MATCH {
            continue;
        }
        let h = hash3(data, i);
        let mut cur = head[h];
        head[h] = i as u32;
        let slot = 2 * (i & CMASK);
        let (mut p1, mut p0) = (slot, slot + 1); // where the next smaller / larger node attaches
        let (mut len0, mut len1) = (0usize, 0usize);
        let mut best = MIN_MATCH - 1;
        // Besides the smallest distance for each length, keep the longest match seen in each distance code
        // bucket: a farther match can be cheaper when its distance code is common in the data.
        let (mut bucket, mut bucket_best) = (usize::MAX, MIN_MATCH - 1);
        // The bucket of the last step kept, when that step was an alternative.
        let mut far_bucket = usize::MAX;
        let mut kept = 0;
        let mut depth = max_depth;
        loop {
            if cur == NIL || i - cur as usize > WINDOW || depth == 0 {
                son[p0] = NIL;
                son[p1] = NIL;
                break;
            }
            depth -= 1;
            let c = cur as usize;
            let pair = 2 * (c & CMASK);
            let mut len = len0.min(len1);
            if data[c + len] == data[i + len] {
                len = common(data, c, i, len + 1, lim);
                if searching {
                    let dist = i - c;
                    if ALT {
                        let bk = dist_code(dist);
                        if bk != bucket {
                            bucket = bk;
                            bucket_best = MIN_MATCH - 1;
                        }
                    }
                    if len > best || (ALT && len > bucket_best && kept < 24) {
                        if ALT && bucket == far_bucket {
                            // Longer than that alternative (as `bucket_best` was), at the same price.
                            farther.last_mut().unwrap().1 = true;
                        }
                        if len > best {
                            steps.push(Step::new(len, dist));
                            far_bucket = usize::MAX;
                        } else {
                            farther.push((Step::new(len, dist), false));
                            far_bucket = bucket;
                        }
                        kept += 1;
                        best = best.max(len);
                        bucket_best = len;
                    }
                }
                if len >= lim {
                    // Same text up to the limit: `i` takes this node's place.
                    son[p1] = son[pair];
                    son[p0] = son[pair + 1];
                    break;
                }
            }
            if data[c + len] < data[i + len] {
                son[p1] = cur;
                p1 = pair + 1;
                cur = son[p1];
                len1 = len;
            } else {
                son[p0] = cur;
                p0 = pair;
                cur = son[p0];
                len0 = len;
            }
        }
        if ALT && searching {
            // The parse prices each length with the cheapest distance among the first `CANDIDATES` steps of the
            // walk that reach it, and of equal prices takes the longer step: so an alternative followed by a
            // longer step with the same distance code (the same price) never wins when the whole walk is in those.
            let walk = steps.len() - start[i - from] as usize + farther.len();
            let before = steps.len();
            steps.extend(
                farther.drain(..).filter(|&(_, beaten)| keep_all || !beaten || walk > CANDIDATES).map(|(s, _)| s),
            );
            if walk <= CANDIDATES {
                // In the order the parse sorts its candidates in: by length, of equal lengths the farther first.
                // At most 24 steps, each distance once: sorted in one by one (the library's sort, built for
                // any length, is 6 KB of the WebAssembly module).
                let key = |s: &Step| {
                    let (len, dist) = s.get();
                    (len, !dist)
                };
                let far = &mut steps[before..];
                for i in 1..far.len() {
                    let (s, mut j) = (far[i], i);
                    while j > 0 && key(&far[j - 1]) > key(&s) {
                        far[j] = far[j - 1];
                        j -= 1;
                    }
                    far[j] = s;
                }
            }
            // At most 24 alternatives.
            farther_count[i - from] = (steps.len() - before) as u8;
        }
    }
    start.push(steps.len() as u32);
    Matches { start, steps, alternatives: ALT, farther: farther_count, same, from }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute force: for each length, the smallest distance.
    fn brute(data: &[u8], to: usize, i: usize) -> Vec<(u16, u16)> {
        let maxlen = (to - i).min(MAX_MATCH);
        let mut out: Vec<(u16, u16)> = Vec::new();
        let mut best = 2;
        for dist in 1..=WINDOW.min(i) {
            let p = i - dist;
            let mut l = 0;
            while l < maxlen && data[p + l] == data[i + l] {
                l += 1;
            }
            if l > best {
                out.push((l as u16, dist as u16));
                best = l;
            }
        }
        out
    }

    fn sample(seed: u64, len: usize) -> Vec<u8> {
        let mut s = seed;
        let mut data = Vec::new();
        for k in 0..len {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let b = match k % 900 {
                0..=99 => (s % 4) as u8,
                100..=499 => 0,
                500..=520 => 7,
                521..=600 => [0u8, 0, 0, 9, 9][k % 5],
                _ => b"the quick brown fox "[k % 20],
            };
            data.push(b);
        }
        data
    }

    #[test]
    fn exhaustive_search_matches_brute_force() {
        for seed in [7u64, 99] {
            let data = sample(seed, 7000);
            let (from, to) = (1000, 6000);
            let m = find_matches(&data, from, to, usize::MAX, false);
            for i in from..to {
                assert_eq!(pairs(m.at(i)), brute(&data, to, i), "position {i}");
                assert_eq!(m.chain_at(i), m.at(i), "position {i}");
            }
        }
    }

    #[test]
    fn chain_with_alternatives_is_the_smallest_distance_chain() {
        for seed in [7u64, 99] {
            let data = sample(seed, 7000);
            let (from, to) = (1000, 6000);
            let plain = find_matches(&data, from, to, 300, false);
            let alt = find_matches(&data, from, to, 300, true);
            let mut extra = 0;
            for i in from..to {
                assert_eq!(alt.chain_at(i), plain.at(i), "position {i}");
                extra += alt.at(i).len() - plain.at(i).len();
            }
            assert!(extra > 0, "alternatives add steps");
        }
    }
}

#[cfg(test)]
mod validity {
    use super::*;

    #[test]
    fn every_reported_match_is_real() {
        let mut s = 99u64;
        let mut data = Vec::new();
        for k in 0..300_000usize {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            data.push(if k % 1000 < 500 { (s % 3) as u8 + b'a' } else { (s >> 8) as u8 });
        }
        for (from, to) in [(0usize, 100_000usize), (100_000, 150_000), (150_000, 300_000)] {
            let m = find_matches(&data, from, to, 64, false);
            for i in from..to {
                let mut prev = (0u16, 0u16);
                for (l, d) in pairs(m.at(i)) {
                    let (l, d) = (l as usize, d as usize);
                    assert!((1..=WINDOW).contains(&d) && d <= i, "pos {i}: dist {d}");
                    assert!(i + l <= to, "pos {i}: len {l} past end");
                    assert!(
                        l > prev.0 as usize && (d > prev.1 as usize),
                        "pos {i}: steps not increasing {:?}",
                        m.at(i)
                    );
                    assert_eq!(&data[i - d..i - d + l], &data[i..i + l], "pos {i}: len {l} dist {d}");
                    prev = (l as u16, d as u16);
                }
            }
        }
    }
}

/// The search as it was before its inner loop was tuned, kept to prove the tuned one finds exactly the same
/// steps, chains and runs.
#[cfg(test)]
mod same_as_before {
    use super::*;

    struct Old {
        steps: Vec<Vec<(u16, u16)>>,
        chain: Vec<Vec<(u16, u16)>>,
        same: Vec<u16>,
    }

    fn common(d: &[u8], a: usize, b: usize, skip: usize, max: usize) -> usize {
        let mut n = skip;
        while n + 8 <= max {
            let x = u64::from_le_bytes(d[a + n..a + n + 8].try_into().unwrap());
            let y = u64::from_le_bytes(d[b + n..b + n + 8].try_into().unwrap());
            let z = x ^ y;
            if z != 0 {
                return n + (z.trailing_zeros() / 8) as usize;
            }
            n += 8;
        }
        while n < max && d[a + n] == d[b + n] {
            n += 1;
        }
        n
    }

    /// The walk's steps without the alternatives the parse never takes: those followed by a step with the same
    /// distance code, when the walk has at most `CANDIDATES` steps.
    fn can_win(walk: &[(u16, u16)], chain: &[(u16, u16)]) -> Vec<(u16, u16)> {
        let beaten = |j: usize| {
            walk.len() <= CANDIDATES
                && !chain.contains(&walk[j])
                && walk.get(j + 1).is_some_and(|n| dist_code(n.1 as usize) == dist_code(walk[j].1 as usize))
        };
        (0..walk.len()).filter(|&j| !beaten(j)).map(|j| walk[j]).collect()
    }

    fn old_find_matches(data: &[u8], from: usize, to: usize, max_depth: usize, alternatives: bool) -> Old {
        const NIL: u32 = u32::MAX;
        const CYC: usize = 2 * WINDOW;
        const CMASK: usize = CYC - 1;
        let hist = from.saturating_sub(WINDOW);
        let mut out = Old { steps: Vec::new(), chain: Vec::new(), same: vec![0u16; to - hist] };
        for i in (hist..to).rev() {
            let k = i - hist;
            out.same[k] = if i + 1 < to && data[i + 1] == data[i] { out.same[k + 1].saturating_add(1) } else { 1 };
        }
        out.same.drain(..from - hist);
        let mut head = vec![NIL; HASH_SIZE];
        let mut son = vec![NIL; 2 * CYC];
        for i in hist..to {
            let searching = i >= from;
            let (mut steps, mut chain) = (Vec::new(), Vec::new());
            let lim = (to - i).min(MAX_MATCH);
            if lim >= MIN_MATCH {
                let h = hash3(data, i);
                let mut cur = head[h];
                head[h] = i as u32;
                let slot = 2 * (i & CMASK);
                let (mut p1, mut p0) = (slot, slot + 1);
                let (mut len0, mut len1) = (0usize, 0usize);
                let mut best = MIN_MATCH - 1;
                let (mut bucket, mut bucket_best) = (usize::MAX, MIN_MATCH - 1);
                let mut kept = 0;
                let mut depth = max_depth;
                loop {
                    if cur == NIL || i - cur as usize > WINDOW || depth == 0 {
                        son[p0] = NIL;
                        son[p1] = NIL;
                        break;
                    }
                    depth -= 1;
                    let c = cur as usize;
                    let pair = 2 * (c & CMASK);
                    let mut len = len0.min(len1);
                    if data[c + len] == data[i + len] {
                        len = common(data, c, i, len + 1, lim);
                        if searching {
                            let dist = i - c;
                            let bk = dist_code(dist);
                            if bk != bucket {
                                bucket = bk;
                                bucket_best = MIN_MATCH - 1;
                            }
                            if len > best || (alternatives && len > bucket_best && kept < 24) {
                                steps.push((len as u16, dist as u16));
                                if len > best {
                                    chain.push((len as u16, dist as u16));
                                }
                                kept += 1;
                                best = best.max(len);
                                bucket_best = len;
                            }
                        }
                        if len >= lim {
                            son[p1] = son[pair];
                            son[p0] = son[pair + 1];
                            break;
                        }
                    }
                    if data[c + len] < data[i + len] {
                        son[p1] = cur;
                        p1 = pair + 1;
                        cur = son[p1];
                        len1 = len;
                    } else {
                        son[p0] = cur;
                        p0 = pair;
                        cur = son[p0];
                        len0 = len;
                    }
                }
            }
            if searching {
                out.steps.push(steps);
                out.chain.push(chain);
            }
        }
        out
    }

    fn data(seed: u64, len: usize) -> Vec<u8> {
        let mut s = seed;
        (0..len)
            .map(|k| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                match (k / 3000 + seed as usize) % 4 {
                    0 => (s % 3) as u8,
                    1 => (s >> 9) as u8,
                    2 => b"abcabdabcabcabd 0 0 0 1 0 1"[(k * 7 + (s % 2) as usize) % 27],
                    _ => [0u8, 0, 0, 0, 0, 0, 0, 0, 255][k % 9],
                }
            })
            .collect()
    }

    #[test]
    fn the_search_finds_what_it_found_before() {
        for seed in 1..=6u64 {
            let d = data(seed, 90_000);
            for (from, to) in [(0usize, 40_000usize), (40_000, 90_000), (89_990, 90_000), (5, 7)] {
                for depth in [1usize, 16, 300, 8192] {
                    for alternatives in [false, true] {
                        let new = find_matches(&d, from, to, depth, alternatives);
                        let old = old_find_matches(&d, from, to, depth, alternatives);
                        // What the parse test's reference reads: every step the old search kept.
                        let all = alternatives.then(|| find_matches_keeping_all(&d, from, to, depth));
                        for i in from..to {
                            let k = i - from;
                            let want =
                                if alternatives { can_win(&old.steps[k], &old.chain[k]) } else { old.steps[k].clone() };
                            assert_eq!(new.walk_at(i), want, "steps {seed} {from} {depth} {alternatives} {i}");
                            if let Some(all) = &all {
                                assert_eq!(all.walk_at(i), old.steps[k], "all steps {seed} {from} {depth} {i}");
                                assert_eq!(all.chain_at(i), new.chain_at(i), "all chain {seed} {from} {depth} {i}");
                            }
                            assert_eq!(
                                pairs(new.chain_at(i)),
                                old.chain[k],
                                "chain {seed} {from} {depth} {alternatives} {i}"
                            );
                            let class = RUNS.iter().map(|&t| (old.same[k] as usize > t) as u8).sum::<u8>();
                            assert_eq!(new.same_at(i), class, "same {seed} {from} {i}");
                        }
                    }
                }
            }
        }
    }
}
