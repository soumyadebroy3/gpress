//! Huffman code lengths (optimal under a maximum length) and canonical codes.
//!
//! Every method here orders the used symbols by the key `count << 16 | symbol` (by count, ties by symbol),
//! so lengths come out the same whichever way the keys were sorted.

/// Optimal code lengths for `freqs` with no code longer than `max_bits`, by package-merge
/// (Larmore and Hirschberg). Symbols with frequency 0 get length 0. One used symbol gets length 1.
pub fn limited_lengths(freqs: &[u32], max_bits: u32) -> Vec<u8> {
    let mut lens = vec![0u8; freqs.len()];
    limited_lengths_into(freqs, max_bits, &mut lens);
    lens
}

pub fn limited_lengths_into(freqs: &[u32], max_bits: u32, lens: &mut [u8]) {
    lens.fill(0);
    with_scratch(2 * freqs.len(), |s| {
        let (keys, room) = s.split_at_mut(freqs.len());
        let n = sorted_keys(freqs, keys, room);
        limited_sorted(&keys[..n], max_bits, lens, &mut Guide::new());
    });
}

/// Huffman code lengths without a length limit, computed in place (Moffat and Katajainen's method), with no
/// heap allocation for alphabets up to 320 symbols. Returns the longest length; callers fall back to
/// `limited_lengths` when it exceeds their limit. `lens` receives one length per symbol (0 for unused symbols).
pub fn fast_lengths(freqs: &[u32], lens: &mut [u8]) -> u8 {
    lens[..freqs.len()].fill(0);
    with_scratch(2 * freqs.len(), |s| {
        let (keys, room) = s.split_at_mut(freqs.len());
        let n = sorted_keys(freqs, keys, room);
        unlimited_sorted(&keys[..n], room, lens).0
    })
}

/// Optimal lengths limited to `max_bits`: the fast method when it fits, package-merge otherwise (on the keys
/// already sorted).
pub fn lengths_into(freqs: &[u32], max_bits: u32, lens: &mut [u8]) {
    lens[..freqs.len()].fill(0);
    with_scratch(2 * freqs.len(), |s| {
        let (keys, room) = s.split_at_mut(freqs.len());
        let n = sorted_keys(freqs, keys, room);
        let keys = &keys[..n];
        if unlimited_sorted(keys, room, lens).0 as u32 > max_bits {
            lens.fill(0);
            limited_sorted(keys, max_bits, lens, &mut Guide::new());
        }
    })
}

/// Code lengths for one alphabet, worked out again and again for counts that change a little from one call
/// to the next (the block-split search); the lengths are those of `lengths_into`. Counts below 256 but for a
/// few, the usual case for the literal/length code there, are worked out by runs of equal counts (`Runs`).
/// Otherwise the used symbols' order is kept from the last call: the symbols whose count stayed keep their
/// places and only the others are sorted again. All is of a fixed size, and nothing is allocated for
/// Deflate's alphabets and limits but the runs (some 27 KB, once) and package-merge's lists for more than 128
/// symbols (for the call).
pub struct LengthCoder<const N: usize> {
    /// For alphabets of `RUN_MIN` to `RUN_SYMS` symbols, made at the first call; whether the lengths are from it.
    runs: Option<Box<Runs>>,
    from_runs: bool,
    /// The keys of the symbols used in the last call, sorted, in `keys[..used]`, and whether each symbol is
    /// among them; only when `sorted` (the last call did not use `runs`).
    keys: [u64; N],
    member: [bool; N],
    sorted: bool,
    used: usize,
    /// Room for the keys that moved and for the code tree.
    work: [u64; N],
    /// The lengths of the last call, one per symbol; its counts, limit (none before the first) and result.
    lens: [u8; N],
    counts: [u32; N],
    max_bits: Option<u32>,
    bits: u64,
    /// What package-merge learnt in its last run here.
    guide: Guide,
}

impl<const N: usize> Default for LengthCoder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> LengthCoder<N> {
    pub const fn new() -> Self {
        const { assert!(N <= 1 << 16, "symbols must fit in 16 bits") };
        LengthCoder {
            runs: None,
            from_runs: false,
            keys: [0; N],
            member: [false; N],
            sorted: true,
            used: 0,
            work: [0; N],
            lens: [0; N],
            counts: [0; N],
            max_bits: None,
            bits: 0,
            guide: Guide::new(),
        }
    }

    /// Sets the lengths to what `lengths_into(freqs, max_bits, ..)` gives and returns the sum of count times
    /// length.
    pub fn lengths(&mut self, freqs: &[u32; N], max_bits: u32) -> u64 {
        // The same counts as last time give the same lengths; worth checking for small alphabets only (the
        // distances often stay).
        if N > 32 {
            return self.work_out(freqs, max_bits);
        }
        if self.max_bits == Some(max_bits) && *freqs == self.counts {
            return self.bits;
        }
        self.bits = self.work_out(freqs, max_bits);
        (self.counts, self.max_bits) = (*freqs, Some(max_bits));
        self.bits
    }

    fn work_out(&mut self, freqs: &[u32; N], max_bits: u32) -> u64 {
        // Runs take few counts of 256 or more and 48 symbols or more: not worth trying while the last call (which
        // then did not use them) had more large counts (its keys are sorted, so those are the last), nor when
        // the last call and this one have few symbols (counted once, for `resort` too).
        let mut used = None;
        let skip = self.sorted
            && (self.used > RUN_BIG && self.keys[self.used - RUN_BIG - 1] >> 16 >= 256
                || self.used < RUN_MIN && *used.insert(freqs.iter().filter(|&&f| f != 0).count()) < RUN_MIN);
        let (mut keyed, mut deep) = (false, false);
        if (RUN_MIN..=RUN_SYMS).contains(&N) && !skip {
            let runs = self.runs.get_or_insert_with(|| Box::new(Runs::new()));
            if let Some((bits, used)) = runs.lengths(freqs, max_bits, &mut self.lens, self.from_runs) {
                self.used = used;
                self.sorted = false;
                self.from_runs = true;
                return bits;
            }
            // The runs have this call's keys in order (and know when the lengths are too long).
            if runs.current {
                self.used = runs.keys_into(&mut self.keys);
                for (m, &f) in self.member.iter_mut().zip(freqs) {
                    *m = f != 0;
                }
                (keyed, deep) = (true, runs.deep);
            }
        }
        self.from_runs = false;
        if !keyed && self.sorted {
            self.resort(freqs, used);
        } else if !keyed {
            self.used = sorted_keys(freqs, &mut self.keys, &mut self.work);
            for (m, &f) in self.member.iter_mut().zip(freqs) {
                *m = f != 0;
            }
        }
        self.sorted = true;
        let keys = &self.keys[..self.used];
        self.lens.fill(0);
        let (max, mut bits) = match deep {
            true => (u8::MAX, 0),
            false => unlimited_sorted(keys, &mut self.work, &mut self.lens),
        };
        if max as u32 > max_bits {
            limited_sorted(keys, max_bits, &mut self.lens, &mut self.guide);
            bits = keys.iter().map(|&k| (k >> 16) * self.lens[(k & 0xffff) as usize] as u64).sum();
        }
        bits
    }

    /// Sorts the keys for `freqs` from those of the last call; `used` is how many counts are not 0, when known.
    fn resort(&mut self, freqs: &[u32; N], used: Option<usize>) {
        // The symbols used last time whose count stayed keep their order; the others are taken out (the ones
        // still used go to `work`), sorted on their own and merged back, and so are the newly used symbols.
        let (mut kept, mut moved) = (0, 0);
        for k in 0..self.used.min(N) {
            let key = self.keys[k];
            let s = (key & 0xffff) as usize;
            let f = freqs[s];
            let new = ((f as u64) << 16) | s as u64;
            self.keys[kept] = key;
            self.work[moved] = new;
            kept += (new == key) as usize;
            moved += ((new != key) & (f != 0)) as usize;
            if f == 0 {
                self.member[s] = false;
            }
        }
        let added = used.unwrap_or_else(|| freqs.iter().filter(|&&f| f != 0).count()) - kept - moved;
        // The keys that moved are often still nearly in order.
        if !insertion_sort_within(&mut self.work[..moved], 8 * moved) {
            self.work[..moved].sort_unstable();
        }
        merge(&mut self.keys[..kept + moved], kept, &self.work[..moved]);
        self.used = kept + moved;
        if added > 0 {
            let mut a = 0;
            for (s, &f) in freqs.iter().enumerate() {
                if f != 0 && !self.member[s] {
                    self.work[a] = ((f as u64) << 16) | s as u64;
                    self.member[s] = true;
                    a += 1;
                    if a == added {
                        break;
                    }
                }
            }
            if !insertion_sort_within(&mut self.work[..a], 8 * a) {
                self.work[..a].sort_unstable();
            }
            merge(&mut self.keys[..self.used + a], self.used, &self.work[..a]);
            self.used += a;
        }
    }

    /// The lengths of the last call, one per symbol.
    pub fn lens(&self) -> &[u8; N] {
        &self.lens
    }

    /// How many symbols had a nonzero count in the last call.
    pub fn used(&self) -> usize {
        self.used
    }
}

/// Merges the sorted `add` into the sorted first `m` entries of `keys`, which has room for both.
fn merge(keys: &mut [u64], m: usize, add: &[u64]) {
    let (mut i, mut j) = (m.min(keys.len()), add.len());
    for out in (0..keys.len()).rev() {
        if j == 0 {
            break;
        }
        if i > 0 && keys[i - 1] > add[j - 1] {
            keys[out] = keys[i - 1];
            i -= 1;
        } else {
            keys[out] = add[j - 1];
            j -= 1;
        }
    }
}

/// Sorts `keys` by insertion, which is quick when they are nearly in order. Gives up (returning false, the
/// keys in some order) once that has moved keys more than `budget` places in all.
fn insertion_sort_within(keys: &mut [u64], budget: usize) -> bool {
    let Some(&first) = keys.first() else { return true };
    // `top` is the largest key so far; a key below it moves back to its place.
    let (mut top, mut moved) = (first, 0usize);
    for k in 1..keys.len() {
        let x = keys[k];
        if x >= top {
            top = x;
            continue;
        }
        let mut p = k;
        while p > 0 && keys[p - 1] > x {
            keys[p] = keys[p - 1];
            p -= 1;
        }
        keys[p] = x;
        moved += k - p;
        if moved > budget {
            return false;
        }
    }
    true
}

/// Most symbols `Runs` takes, room for a set of them (a bit each), most counts of 256 or more it takes and
/// fewest symbols it is used for.
const RUN_SYMS: usize = 320;
const RUN_WORDS: usize = 8;
const RUN_BIG: usize = 8;
const RUN_MIN: usize = 48;

/// Moffat and Katajainen's method worked on runs of equal weights, for alphabets of up to 320 symbols whose
/// counts are below 256 but for a few. Such counts often repeat (a few dozen different counts among a few
/// hundred symbols), and then there are far fewer runs than leaves; the counts need no sorting either, only
/// tallying (the few larger ones are sorted). The lengths are those of `unlimited_sorted` on the sorted keys:
/// the same internal nodes are made in the same order, so the tree has the same shape, and a symbol's length
/// depends only on its rank. The tallies are kept from one call to the next and mended where the counts
/// changed, and so are the lengths.
struct Runs {
    /// The counts the tallies are of (once `synced`).
    count: [u32; RUN_SYMS],
    synced: bool,
    /// How many symbols have each count below 256, the counts from 1 on that some symbol has (a bit each, and
    /// maybe some no symbol has any more), the symbols that have each count (a bit each), and those whose count
    /// is 256 or more.
    tally: [u16; 256],
    seen: [u64; 4],
    set: [[u64; RUN_WORDS]; 256],
    big: [u64; RUN_WORDS],
    /// The length of the symbols of each count below 256 used in the last lengths given (255 when they do
    /// not all have the same length); 0 for count 0.
    depth: [u8; 256],
    /// The symbols whose count changed in this call, and the sorted keys of those with counts of 256 or more.
    changed: [u16; RUN_SYMS],
    big_keys: [u64; RUN_BIG],
    /// The counts used, lightest first, and how many symbols have each; then a weight above all.
    leaf_w: [u64; 256 + RUN_BIG + 1],
    leaf_k: [u16; 256 + RUN_BIG + 1],
    /// The internal nodes in the order they are made, as runs of equal weight: weight (then one above all),
    /// how many, the index of the first, how many leaves are taken before them and the place of the last
    /// (arrays of one length, so that an index is checked once for all).
    node_w: [u64; RUN_SYMS + 1],
    node_k: [u16; RUN_SYMS + 1],
    node_first: [u16; RUN_SYMS + 1],
    node_taken: [u16; RUN_SYMS + 1],
    node_last: [u16; RUN_SYMS + 1],
    /// How many leaves are at each depth; whether the tallies are of the counts of this call and whether it
    /// failed because the longest length was over the limit.
    bl: [u16; 64],
    current: bool,
    deep: bool,
}

impl Runs {
    fn new() -> Runs {
        Runs {
            count: [0; RUN_SYMS],
            synced: false,
            tally: [0; 256],
            seen: [0; 4],
            set: [[0; RUN_WORDS]; 256],
            big: [0; RUN_WORDS],
            depth: [0; 256],
            changed: [0; RUN_SYMS],
            big_keys: [0; RUN_BIG],
            leaf_w: [0; 256 + RUN_BIG + 1],
            leaf_k: [0; 256 + RUN_BIG + 1],
            node_w: [0; RUN_SYMS + 1],
            node_k: [0; RUN_SYMS + 1],
            node_first: [0; RUN_SYMS + 1],
            node_taken: [0; RUN_SYMS + 1],
            node_last: [0; RUN_SYMS + 1],
            bl: [0; 64],
            current: false,
            deep: false,
        }
    }

    /// Takes symbol `s` (in word `w` of a set) from the tallies of count `old` to those of count `new`.
    #[inline(always)]
    fn move_symbol(&mut self, s: usize, w: usize, old: u32, new: u32) {
        let bit = 1 << (s % 64);
        if old < 256 {
            let old = old as usize;
            self.tally[old] -= 1;
            self.set[old][w] &= !bit;
        } else {
            self.big[w] &= !bit;
        }
        if new < 256 {
            let new = new as usize;
            self.tally[new] += 1;
            self.seen[new / 64] |= 1 << (new % 64);
            self.set[new][w] |= bit;
        } else {
            self.big[w] |= bit;
        }
    }

    /// The lengths and the sum of count times length of `unlimited_sorted`, and how many symbols are used.
    /// None (and `lens` left alone) when there are few symbols or too many, more than `RUN_BIG` counts of 256
    /// or more, or the longest length is over `max_bits` (then package-merge decides). `fresh` says that `lens`
    /// holds the lengths this gave last; then only those that change are written.
    fn lengths(&mut self, freqs: &[u32], max_bits: u32, lens: &mut [u8], fresh: bool) -> Option<(u64, usize)> {
        let len = freqs.len();
        (self.current, self.deep) = (false, false);
        if !(RUN_MIN..=RUN_SYMS).contains(&len) || max_bits == 0 {
            return None;
        }
        let mut fresh = fresh && self.synced;
        let mut nch = 0;
        if self.synced {
            // Mend the tallies where the counts changed: first which ones did (a bit each, found eight at a
            // time), then each of those.
            let mut flag = [0u8; RUN_SYMS];
            for ((g, &f), &c) in flag.iter_mut().zip(freqs).zip(&self.count) {
                *g = (f != c) as u8;
            }
            let mut diff = [0u64; RUN_WORDS];
            for (k, g8) in flag.as_chunks::<8>().0.iter().enumerate() {
                let b = u64::from_le_bytes(*g8);
                diff[(k / 8) & 7] |= (b.wrapping_mul(0x0102_0408_1020_4080) >> 56) << (8 * (k % 8));
            }
            for (w, &word) in diff.iter().enumerate() {
                let mut m = word;
                while m != 0 {
                    let s = 64 * w + m.trailing_zeros() as usize;
                    m &= m - 1;
                    let (old, new) = (self.count[s], freqs[s]);
                    self.count[s] = new;
                    self.move_symbol(s, w, old, new);
                    self.changed[nch] = s as u16;
                    nch += 1;
                }
            }
        } else {
            self.tally = [0; 256];
            self.seen = [0; 4];
            self.set = [[0; RUN_WORDS]; 256];
            self.big = [0; RUN_WORDS];
            self.tally[0] = len as u16;
            self.count = [0; RUN_SYMS];
            for (s, &f) in freqs.iter().enumerate() {
                self.count[s] = f;
                self.move_symbol(s, s / 64, 0, f);
            }
            self.synced = true;
            fresh = false;
        }
        self.current = true;
        let nbig: usize = self.big.iter().map(|w| w.count_ones() as usize).sum();
        let used = len - self.tally[0] as usize;
        if nbig > RUN_BIG || used < RUN_MIN {
            return None;
        }
        let (mut ra, mut n) = (0, 0);
        for i in 0..4 {
            let mut b = if i == 0 { self.seen[0] & !1 } else { self.seen[i] };
            while b != 0 {
                let c = 64 * i + b.trailing_zeros() as usize;
                b &= b - 1;
                let k = self.tally[c];
                if k == 0 {
                    self.seen[i] &= !(1 << (c % 64));
                    continue;
                }
                (self.leaf_w[ra], self.leaf_k[ra]) = (c as u64, k);
                ra += 1;
                n += k as usize;
            }
        }
        // The few larger counts, sorted, after the others.
        let small = ra;
        let mut nb = 0;
        for (w, &word) in self.big.iter().enumerate() {
            let mut b = word;
            while b != 0 {
                let s = 64 * w + b.trailing_zeros() as usize;
                b &= b - 1;
                self.big_keys[nb] = (self.count[s] as u64) << 16 | s as u64;
                nb += 1;
            }
        }
        insertion_sort(&mut self.big_keys[..nb]);
        for &key in &self.big_keys[..nb] {
            if ra > small && self.leaf_w[ra - 1] == key >> 16 {
                self.leaf_k[ra - 1] += 1;
            } else {
                (self.leaf_w[ra], self.leaf_k[ra]) = (key >> 16, 1);
                ra += 1;
            }
        }
        n += nb;
        let Some((max, bits)) = self.tree(ra, n, max_bits) else {
            self.deep = true;
            return None;
        };
        // The leaves of a depth are those of a range of ranks, the deepest the lightest, and the symbols of a
        // count have the ranks of a range, in symbol order.
        let mut depth_of_rank = [0u8; RUN_SYMS];
        let (mut r0, mut d) = (0, max as usize);
        for &k in self.bl[..=d].iter().rev() {
            depth_of_rank[r0..r0 + k as usize].fill(d as u8);
            r0 += k as usize;
            d = d.saturating_sub(1);
        }
        // A count's symbols are written again when their length changed, or when they do not all have the
        // same length; the symbols whose count changed get the length of their new count. Those of the larger
        // counts are written each time.
        let mut rank = 0;
        for (&c, &k) in self.leaf_w[..small].iter().zip(&self.leaf_k[..small]) {
            let c = c as usize & 255;
            let (d, e) = (depth_of_rank[rank], depth_of_rank[rank + k as usize - 1]);
            let depth = if d == e { d } else { 255 };
            if depth == 255 || depth != self.depth[c] || !fresh {
                self.depth[c] = depth;
                if depth == 255 || fresh {
                    let mut r = rank;
                    for (i, &word) in self.set[c].iter().enumerate() {
                        let mut b = word;
                        while b != 0 {
                            lens[64 * i + b.trailing_zeros() as usize] = depth_of_rank[r];
                            b &= b - 1;
                            r += 1;
                        }
                    }
                }
            }
            rank += k as usize;
        }
        if fresh {
            for &s in &self.changed[..nch] {
                let s = s as usize;
                let c = self.count[s];
                if c < 256 && self.depth[c as usize] != 255 {
                    lens[s] = self.depth[c as usize];
                }
            }
        } else {
            self.depth[0] = 0;
            for (l, &f) in lens.iter_mut().zip(freqs) {
                let depth = self.depth[f as usize & 255];
                if f < 256 && depth != 255 {
                    *l = depth;
                }
            }
        }
        for (&key, &d) in self.big_keys[..nb].iter().zip(&depth_of_rank[rank..]) {
            lens[(key & 0xffff) as usize] = d;
        }
        Some((bits, n))
    }

    /// Makes `k` internal nodes of weight `w`, joined to the last run when that has the same weight and is not
    /// taken yet (the first `b` are); returns their weight.
    #[inline(always)]
    fn make(&mut self, w: u64, k: usize, b: usize, nb: &mut usize, made: &mut usize) -> u64 {
        if *nb > b && self.node_w[*nb - 1] == w {
            self.node_k[*nb - 1] += k as u16;
        } else {
            (self.node_w[*nb], self.node_k[*nb], self.node_first[*nb]) = (w, k as u16, *made as u16);
            *nb += 1;
            self.node_w[*nb] = u64::MAX;
        }
        *made += k;
        w * k as u64
    }

    /// The sorted keys of the used symbols of the counts the tallies are of, into `keys`; returns how many.
    fn keys_into(&self, keys: &mut [u64]) -> usize {
        let mut n = 0;
        for (i, &word) in self.seen.iter().enumerate() {
            let mut b = if i == 0 { word & !1 } else { word };
            while b != 0 {
                let c = 64 * i + b.trailing_zeros() as usize;
                b &= b - 1;
                for (w, &symbols) in self.set[c].iter().enumerate() {
                    let mut m = symbols;
                    while m != 0 {
                        keys[n] = (c as u64) << 16 | (64 * w + m.trailing_zeros() as usize) as u64;
                        m &= m - 1;
                        n += 1;
                    }
                }
            }
        }
        let small = n;
        for (w, &word) in self.big.iter().enumerate() {
            let mut b = word;
            while b != 0 {
                let s = 64 * w + b.trailing_zeros() as usize;
                b &= b - 1;
                keys[n] = (self.count[s] as u64) << 16 | s as u64;
                n += 1;
            }
        }
        if n - small <= 32 {
            insertion_sort(&mut keys[small..n]);
        } else {
            keys[small..n].sort_unstable();
        }
        n
    }

    /// Moffat and Katajainen's passes on the `ra` runs of leaves (`n` of them, at least 48): the leaves at
    /// each depth go into `bl`. Returns the longest length and the sum of count times length, or None when the
    /// longest length is over `max_bits`.
    fn tree(&mut self, ra: usize, n: usize, max_bits: u32) -> Option<(u8, u64)> {
        // Pass 1: each step takes a whole run of the lightest items, leaves before internal nodes of the same
        // weight; the nodes made meanwhile are all heavier, so nothing comes in between. Items taken one after
        // the other make the internal nodes pairwise, beginning with the one left over from the step before
        // (`left`, its weight; weights are at least 1). After the leaves and after the nodes made comes a
        // weight above all.
        self.leaf_w[ra] = u64::MAX;
        self.node_w[0] = u64::MAX;
        let (mut a, mut b, mut nb, mut made, mut taken) = (0, 0, 0, 0, 0);
        let mut left = 0;
        let mut bits = 0u64;
        while made < n - 1 {
            let (x, mut c);
            if self.leaf_w[a] <= self.node_w[b] {
                if a == ra {
                    return None;
                }
                (x, c) = (self.leaf_w[a], self.leaf_k[a] as usize);
                a += 1;
                taken += c;
            } else {
                (x, c) = (self.node_w[b], self.node_k[b] as usize);
                self.node_taken[b] = taken as u16;
                self.node_last[b] = (self.node_first[b] as usize + c - 1 + taken) as u16;
                b += 1;
            }
            if left != 0 {
                bits += self.make(left + x, 1, b, &mut nb, &mut made);
                c -= 1;
                left = 0;
            }
            if c >= 2 {
                bits += self.make(2 * x, c / 2, b, &mut nb, &mut made);
            }
            if c % 2 == 1 {
                left = x;
            }
        }
        // Passes 2 and 3: node t (not the root) is the item taken at place t + (the leaves taken before it), and
        // the items at places 2s and 2s + 1 make node s. Those places only grow with t, so the nodes no deeper
        // than d + 1 are those from some index on: the first whose place reaches twice the first node of depth
        // d. Below the internal nodes of one depth there are twice as many places one level deeper; those the
        // internal nodes of that depth do not take are leaves.
        self.node_taken[nb - 1] = n as u16;
        let (mut lo, mut hi, mut places, mut j, mut d) = (n - 2, n - 1, 1usize, nb - 1, 0);
        loop {
            let inner = hi - lo;
            *self.bl.get_mut(d)? = places.checked_sub(inner)? as u16;
            if inner == 0 {
                break;
            }
            places = 2 * inner;
            let target = 2 * lo;
            while j > 0 && self.node_last[j - 1] as usize >= target {
                j -= 1;
            }
            let (first, taken) = (self.node_first[j] as usize, self.node_taken[j] as usize);
            (hi, lo) = (lo, first.max(target.saturating_sub(taken)).min(lo));
            d += 1;
        }
        if d > max_bits as usize {
            return None;
        }
        Some((d as u8, bits))
    }
}

/// Runs `f` on a zeroed scratch array of at least `len` entries, on the stack for twice Deflate's largest
/// alphabet.
fn with_scratch<R>(len: usize, f: impl FnOnce(&mut [u64]) -> R) -> R {
    if len <= 64 {
        f(&mut [0; 64])
    } else if len <= 640 {
        f(&mut [0; 640])
    } else {
        f(&mut vec![0; len])
    }
}

/// Writes the keys of the used symbols into `keys` (as long as `freqs` at least), sorted, and returns how
/// many there are; `room` (as long) is scratch. A few keys are sorted by insertion, more by their counts'
/// bytes, the lowest byte first (a radix sort): each pass keeps the order of the keys whose byte is the same,
/// so the keys of equal counts stay in symbol order.
fn sorted_keys(freqs: &[u32], keys: &mut [u64], room: &mut [u64]) -> usize {
    let mut n = 0;
    for (i, &f) in freqs.iter().enumerate() {
        keys[n] = ((f as u64) << 16) | i as u64;
        n += (f > 0) as usize;
    }
    if n <= 32 {
        insertion_sort(&mut keys[..n]);
        return n;
    }
    // As few passes as bytes would take, each on as many bits.
    let bits = 32 - freqs.iter().fold(0, |a, &f| a | f).leading_zeros() as usize;
    let passes = bits.div_ceil(8);
    let digit = bits.div_ceil(passes);
    let (mut from, mut to) = (&mut keys[..n], &mut room[..n]);
    for pass in 0..passes {
        let shift = 16 + digit * pass;
        let mask = (1 << digit) - 1;
        let mut at = [0usize; 256];
        for &k in from.iter() {
            at[((k >> shift) & mask) as usize] += 1;
        }
        let mut p = 0;
        for a in &mut at[..1 << digit] {
            (*a, p) = (p, p + *a);
        }
        for &k in from.iter() {
            let d = ((k >> shift) & mask) as usize;
            to[at[d]] = k;
            at[d] += 1;
        }
        std::mem::swap(&mut from, &mut to);
    }
    if passes % 2 == 1 {
        keys[..n].copy_from_slice(&room[..n]);
    }
    n
}

/// Sorts a few keys by insertion.
fn insertion_sort(keys: &mut [u64]) {
    for k in 1..keys.len() {
        let x = keys[k];
        let mut p = k;
        while p > 0 && keys[p - 1] > x {
            keys[p] = keys[p - 1];
            p -= 1;
        }
        keys[p] = x;
    }
}

/// Huffman lengths without a limit for the used symbols, given their sorted keys, by Moffat and Katajainen's
/// method: `tree` (at least as long as `keys`) holds the internal nodes. Writes each used symbol's length into
/// `lens` and returns the longest and the sum of count times length. Always inlined, so that the arrays' sizes
/// are known.
#[inline(always)]
fn unlimited_sorted(keys: &[u64], tree: &mut [u64], lens: &mut [u8]) -> (u8, u64) {
    let n = keys.len();
    if n == 0 {
        return (0, 0);
    }
    if n <= 2 {
        for &k in keys {
            lens[(k & 0xffff) as usize] = 1;
        }
        return (1, keys.iter().map(|&k| k >> 16).sum());
    }
    let w = |i: usize| keys[i] >> 16;
    let q = &mut tree[..n - 1];
    // Pass 1: internal node weights in the order they are made; each takes the two lightest of the leaves and
    // the internal nodes not taken yet, a leaf before an internal node of the same weight. Their sum is the sum
    // of count times length. A taken internal node's weight makes way for its parent's index.
    q[0] = w(0) + w(1);
    let mut bits = q[0];
    let (mut leaf, mut root, mut next) = (2, 0, 1);
    // While two leaves are left: both lists are sorted, so the two lightest are the next two leaves, the next
    // two internal nodes, or one of each. There is always an internal node left, and a node not made yet
    // (`q[next]`) weighs more than all.
    while leaf + 1 < n {
        q[next] = u64::MAX;
        let s = if w(leaf + 1) <= q[root] {
            leaf += 2;
            w(leaf - 2) + w(leaf - 1)
        } else if q[root + 1] < w(leaf) {
            let s = q[root] + q[root + 1];
            q[root] = next as u64;
            q[root + 1] = next as u64;
            root += 2;
            s
        } else {
            let s = q[root] + w(leaf);
            q[root] = next as u64;
            root += 1;
            leaf += 1;
            s
        };
        q[next] = s;
        bits += s;
        next += 1;
    }
    // One leaf left at most, taken one item at a time; then internal nodes in pairs.
    while leaf < n && next < n - 1 {
        let x = if w(leaf) <= q[root] {
            leaf += 1;
            w(leaf - 1)
        } else {
            let x = q[root];
            q[root] = next as u64;
            root += 1;
            x
        };
        let y = if leaf < n && (root >= next || w(leaf) <= q[root]) {
            leaf += 1;
            w(leaf - 1)
        } else {
            let y = q[root];
            q[root] = next as u64;
            root += 1;
            y
        };
        q[next] = x + y;
        bits += x + y;
        next += 1;
    }
    while next < n - 1 {
        let s = q[root] + q[root + 1];
        q[root] = next as u64;
        q[root + 1] = next as u64;
        root += 2;
        q[next] = s;
        bits += s;
        next += 1;
    }
    // Passes 2 and 3: depths, from the root down. Internal nodes are taken in the order they are made, so their
    // parents' indices never go down: the nodes no deeper than d (but the root) are those whose parent is above
    // depth d, which are the nodes from some index on (found by a binary search, or by a step at a time when
    // there are few). Below the internal nodes of one depth there are twice as many places one level deeper;
    // those the internal nodes of that depth do not take are leaves, the heaviest leaves first.
    let parents = &q[..n - 2];
    let (mut places, mut lo, mut hi, mut next) = (1, n - 2, n - 1, n);
    let mut depth = 0usize;
    loop {
        // The internal nodes of this depth are `lo..hi`.
        let leaves = places - (hi - lo);
        let len = depth.min(255) as u8;
        for &k in &keys[next - leaves..next] {
            lens[(k & 0xffff) as usize] = len;
        }
        next -= leaves;
        if lo == hi {
            // The lightest leaf is the deepest.
            debug_assert_eq!(next, 0);
            return (len, bits);
        }
        places = 2 * (hi - lo);
        hi = lo;
        if lo > 64 {
            lo = parents[..lo].partition_point(|&p| (p as usize) < hi);
        } else {
            while lo > 0 && parents[lo - 1] as usize >= hi {
                lo -= 1;
            }
        }
        depth += 1;
    }
}

/// What package-merge learns for a later run on related counts: how many items it took from each level.
#[derive(Clone, Copy)]
struct Guide {
    /// Items taken from each level (level 0 the deepest), out of `cap`, for `levels` levels; no levels for none.
    want: [usize; 15],
    cap: usize,
    levels: usize,
}

impl Guide {
    const fn new() -> Guide {
        Guide { want: [0; 15], cap: 0, levels: 0 }
    }
}

/// Package-merge lengths for the used symbols, given their sorted keys; other entries of `lens` are left
/// alone. The lists live in fixed arrays for Deflate's alphabets and limits: on the stack for up to 128 symbols,
/// and for more in one heap block made for the call. With a `guide` from a run on related counts, the lists of
/// many symbols are first built only a little beyond what that run took from them; when that is not enough,
/// they are built whole.
fn limited_sorted(keys: &[u64], max_bits: u32, lens: &mut [u8], guide: &mut Guide) {
    let n = keys.len();
    if n <= 2 {
        for &k in keys {
            lens[(k & 0xffff) as usize] = 1;
        }
        return;
    }
    assert!(1usize.checked_shl(max_bits).is_none_or(|m| n <= m), "too many symbols for {max_bits} bits");
    let levels = (max_bits as usize).min(n - 1);
    let cap = 2 * n - 2;
    if levels <= 15 && n <= 320 {
        let (mut want, mut taken) = ([0; 15], [0; 15]);
        // Arrays of a known size let the compiler leave out most index checks. Those for over 128 symbols take
        // 11 KB: on the heap, made and freed here, so as not to add that much to the stack.
        fn run<const CUT: bool>(
            keys: &[u64],
            levels: usize,
            limit: &[usize],
            want: &mut [usize],
            taken: &mut [usize],
        ) -> bool {
            let built = &mut [(0, false); 15];
            if keys.len() <= 128 {
                package_merge::<CUT>(
                    keys,
                    levels,
                    limit,
                    &mut [0; 254],
                    &mut [0; 254],
                    &mut [0; 60],
                    built,
                    want,
                    taken,
                )
            } else {
                let (a, b, leaf) = &mut *Box::new(([0; 638], [0; 638], [0; 150]));
                package_merge::<CUT>(keys, levels, limit, a, b, leaf, built, want, taken)
            }
        }
        // A list must give what this run takes from it and two items at each level below to look ahead, with
        // some room for the counts that changed. A list nearly whole is made whole: whole lists that repeat the
        // level below end the building early. Worth it for many symbols only.
        let mut done = false;
        if guide.levels == levels && n >= 128 {
            let mut limit = [cap; 15];
            for (lvl, l) in limit[..levels].iter_mut().enumerate() {
                let w = guide.want[lvl] + cap.saturating_sub(guide.cap);
                let w = w + w / 16 + 2 * (levels - lvl) + 8;
                *l = if w + cap / 8 < cap { w } else { cap };
            }
            done = run::<true>(keys, levels, &limit, &mut want, &mut taken);
        }
        if !done {
            done = run::<false>(keys, levels, &[cap; 15], &mut want, &mut taken);
        }
        debug_assert!(done);
        *guide = Guide { want, cap, levels };
        assign(keys, &mut taken[..levels], lens);
    } else {
        let (mut want, mut taken) = (vec![0; levels], vec![0; levels]);
        let leaf = &mut vec![0; levels * cap.div_ceil(64)];
        let built = &mut vec![(0, false); levels];
        let (a, b) = (&mut vec![0; cap], &mut vec![0; cap]);
        package_merge::<false>(keys, levels, &vec![cap; levels], a, b, leaf, built, &mut want, &mut taken);
        assign(keys, &mut taken, lens);
    }
}

/// Package-merge on n >= 3 leaves given by their sorted keys. Level 0 (the deepest) holds the leaves; each
/// higher level merges the leaves with pairs ("packages") of the level below, kept sorted and cut at 2n-2
/// items, a leaf before a package of equal weight. Level `lvl` is built up to `limit[lvl]` items, and only as
/// far as the level below decides it, so every item made is that of the whole list. Only the weights of the
/// level below are kept (`prev`, `cur`: 2n-2 entries each), with one bit per item that says whether it is a
/// leaf (`leaf`: level by level, each level's 2n-2 bits in whole words), and in `built`
/// how many items each level has and whether that is the whole list. Returns in `taken[lvl]` how many leaves
/// the items taken from each level hold and in `want[lvl]` how many items it took; false (and nothing
/// returned) when a list was cut short of that. Without `CUT` the limits are all 2n-2 and the lists are all
/// whole. Always inlined, so that the arrays' sizes are known.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn package_merge<'a, const CUT: bool>(
    keys: &[u64],
    levels: usize,
    limit: &[usize],
    mut prev: &'a mut [u64],
    mut cur: &'a mut [u64],
    leaf: &mut [u64],
    built: &mut [(usize, bool)],
    want: &mut [usize],
    taken: &mut [usize],
) -> bool {
    let n = keys.len();
    let cap = 2 * n - 2;
    let words = cap.div_ceil(64);
    for (p, &k) in prev.iter_mut().zip(keys) {
        *p = k >> 16;
    }
    built[0] = (n, true);
    // Once a whole list's weights equal those of the level below, every level above repeats it: they are not
    // built again.
    let mut top = levels;
    for lvl in 1..levels {
        let bits = &mut leaf[lvl * words..(lvl + 1) * words];
        let (below, below_whole) = built[lvl - 1];
        // Both bounds are within the lists anyway; saying so lets the compiler leave out the index checks.
        let npk = (below / 2).min(prev.len() / 2);
        let end = if CUT { limit[lvl].min(cap) } else { cap }.min(cur.len());
        // A package not made below weighs more than all when the level below is whole, else it is unknown and
        // ends the list here (weight 0 stops it).
        let unknown = if !CUT || below_whole { u64::MAX } else { 0 };
        let package = |j: usize| if j < npk { prev[2 * j] + prev[2 * j + 1] } else { unknown };
        // The leaf bits are gathered a word at a time.
        let (mut i, mut j, mut k) = (0usize, 0usize, 0usize);
        let mut pw = package(0);
        let mut word = 0u64;
        while k < end {
            if i < n && (keys[i] >> 16) <= pw {
                cur[k] = keys[i] >> 16;
                word |= 1 << (k % 64);
                i += 1;
            } else if j < npk {
                cur[k] = pw;
                j += 1;
                pw = package(j);
            } else {
                break;
            }
            k += 1;
            if k % 64 == 0 {
                bits[k / 64 - 1] = word;
                word = 0;
            }
        }
        if k % 64 != 0 {
            bits[k / 64] = word;
        }
        let whole = !CUT || k == cap || (i == n && j == npk && below_whole);
        built[lvl] = (k, whole);
        let same = whole && below_whole && k == below && cur[..k] == prev[..k];
        std::mem::swap(&mut prev, &mut cur);
        if same {
            top = lvl;
            break;
        }
    }
    // Take the cheapest 2n-2 items of the top level; each package taken pulls in two items of the level
    // below. A symbol's length is the number of levels whose taken items include its leaf.
    let mut m = cap;
    for lvl in (0..levels).rev() {
        let src = lvl.min(top);
        let (len, whole) = built[src];
        if m > len && !whole {
            return false;
        }
        let take = m.min(len);
        let nl = if src == 0 { take } else { count_ones(&leaf[src * words..], take) };
        want[lvl] = m;
        taken[lvl] = nl;
        m = 2 * (take - nl);
    }
    true
}

/// Set bits among the first `count` bits of `bits`.
fn count_ones(bits: &[u64], count: usize) -> usize {
    let full: usize = bits[..count / 64].iter().map(|w| w.count_ones() as usize).sum();
    match count % 64 {
        0 => full,
        r => full + (bits[count / 64] & ((1u64 << r) - 1)).count_ones() as usize,
    }
}

/// Lengths from package-merge's `taken` counts: at each level the lightest `taken[lvl]` leaves gain a bit.
fn assign(keys: &[u64], taken: &mut [usize], lens: &mut [u8]) {
    taken.sort_unstable_by(|a, b| b.cmp(a));
    let mut c = taken.len();
    for (k, &key) in keys.iter().enumerate() {
        while c > 0 && taken[c - 1] <= k {
            c -= 1;
        }
        lens[(key & 0xffff) as usize] = c.min(255) as u8;
    }
}

/// Canonical codes (RFC 1951 3.2.2), returned bit-reversed so they can be written least-significant-bit first.
pub fn canonical_codes(lens: &[u8]) -> Vec<u16> {
    let mut bl_count = [0u16; 16];
    for &l in lens {
        if l > 0 {
            bl_count[l as usize] += 1;
        }
    }
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for bits in 1..16 {
        code = (code + bl_count[bits - 1]) << 1;
        next[bits] = code;
    }
    let mut codes = vec![0u16; lens.len()];
    for (i, &l) in lens.iter().enumerate() {
        if l > 0 {
            let c = next[l as usize];
            next[l as usize] += 1;
            codes[i] = reverse_bits(c, l as u32);
        }
    }
    codes
}

#[inline]
pub fn reverse_bits(code: u16, len: u32) -> u16 {
    code.reverse_bits() >> (16 - len)
}

/// Kraft sum scaled by 2^15: equals 32768 for a complete code.
pub fn kraft(lens: &[u8]) -> u64 {
    lens.iter().filter(|&&l| l > 0).map(|&l| 1u64 << (15 - l as u32)).sum()
}

/// The length code as it was before the sorted order was kept between calls: the reference that the tests
/// hold the faster code to.
#[cfg(test)]
pub(crate) mod old {
    pub fn limited_lengths(freqs: &[u32], max_bits: u32) -> Vec<u8> {
        let mut lens = vec![0u8; freqs.len()];
        limited_lengths_into(freqs, max_bits, &mut lens);
        lens
    }

    pub fn limited_lengths_into(freqs: &[u32], max_bits: u32, lens: &mut [u8]) {
        for l in lens.iter_mut() {
            *l = 0;
        }
        let mut leaves: Vec<(u64, u16)> =
            freqs.iter().enumerate().filter(|(_, &f)| f > 0).map(|(i, &f)| (f as u64, i as u16)).collect();
        let n = leaves.len();
        match n {
            0 => return,
            1 => {
                lens[leaves[0].1 as usize] = 1;
                return;
            }
            2 => {
                lens[leaves[0].1 as usize] = 1;
                lens[leaves[1].1 as usize] = 1;
                return;
            }
            _ => {}
        }
        assert!(n <= 1usize << max_bits, "too many symbols for {max_bits} bits");
        leaves.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let levels = (max_bits as usize).min(n - 1);
        let cap = 2 * n - 2;
        // lists[0] holds the deepest level (leaves only); each higher level merges the leaves with
        // pairs ("packages") of the level below, kept sorted and cut at 2n-2 items.
        let mut lists: Vec<Vec<(u64, bool)>> = Vec::with_capacity(levels);
        lists.push(leaves.iter().map(|&(w, _)| (w, true)).collect());
        for _ in 1..levels {
            let prev = lists.last().unwrap();
            let npk = prev.len() / 2;
            let mut cur = Vec::with_capacity(cap.min(n + npk));
            let (mut i, mut j) = (0usize, 0usize);
            while cur.len() < cap && (i < n || j < npk) {
                let pw = if j < npk { prev[2 * j].0 + prev[2 * j + 1].0 } else { u64::MAX };
                if i < n && leaves[i].0 <= pw {
                    cur.push((leaves[i].0, true));
                    i += 1;
                } else {
                    cur.push((pw, false));
                    j += 1;
                }
            }
            lists.push(cur);
        }
        // Take the cheapest 2n-2 items of the top level; each package taken pulls in two items of the
        // level below. A symbol's length is the number of levels whose taken items include its leaf.
        let mut counts = vec![0u8; n];
        let mut m = cap;
        for lvl in (0..levels).rev() {
            let list = &lists[lvl];
            debug_assert!(m <= list.len());
            let take = m.min(list.len());
            let (mut nl, mut np) = (0usize, 0usize);
            for it in &list[..take] {
                if it.1 {
                    nl += 1
                } else {
                    np += 1
                }
            }
            for c in counts[..nl].iter_mut() {
                *c += 1;
            }
            m = 2 * np;
        }
        for (k, &(_, sym)) in leaves.iter().enumerate() {
            lens[sym as usize] = counts[k];
        }
    }

    pub fn fast_lengths(freqs: &[u32], lens: &mut [u8]) -> u8 {
        const MAXN: usize = 320;
        let mut w = [0u64; MAXN];
        let mut sym = [0u16; MAXN];
        let mut n = 0;
        for (i, &f) in freqs.iter().enumerate() {
            lens[i] = 0;
            if f > 0 {
                w[n] = ((f as u64) << 16) | i as u64; // ties broken by symbol, like `limited_lengths`
                n += 1;
            }
        }
        if n == 0 {
            return 0;
        }
        if n <= 2 {
            for k in 0..n {
                lens[(w[k] & 0xffff) as usize] = 1;
            }
            return 1;
        }
        w[..n].sort_unstable();
        for k in 0..n {
            sym[k] = (w[k] & 0xffff) as u16;
            w[k] >>= 16;
        }
        let a = &mut w[..n];
        // Pass 1: internal node weights, left to right.
        a[0] += a[1];
        let (mut root, mut leaf) = (0usize, 2usize);
        for next in 1..n - 1 {
            if leaf >= n || a[root] < a[leaf] {
                a[next] = a[root];
                a[root] = next as u64;
                root += 1;
            } else {
                a[next] = a[leaf];
                leaf += 1;
            }
            if leaf >= n || (root < next && a[root] < a[leaf]) {
                a[next] += a[root];
                a[root] = next as u64;
                root += 1;
            } else {
                a[next] += a[leaf];
                leaf += 1;
            }
        }
        // Pass 2: internal node depths, right to left.
        a[n - 2] = 0;
        for next in (0..n - 2).rev() {
            a[next] = a[a[next] as usize] + 1;
        }
        // Pass 3: leaf depths.
        let (mut avbl, mut used, mut depth) = (1i64, 0i64, 0u64);
        let (mut root, mut next) = (n as isize - 2, n as isize - 1);
        while avbl > 0 {
            while root >= 0 && a[root as usize] == depth {
                used += 1;
                root -= 1;
            }
            while avbl > used {
                a[next as usize] = depth;
                next -= 1;
                avbl -= 1;
            }
            avbl = 2 * used;
            depth += 1;
            used = 0;
        }
        let mut max = 0u8;
        for k in 0..n {
            let l = a[k].min(255) as u8;
            lens[sym[k] as usize] = l;
            max = max.max(l);
        }
        max
    }

    pub fn lengths_into(freqs: &[u32], max_bits: u32, lens: &mut [u8]) {
        if fast_lengths(freqs, lens) as u32 > max_bits {
            limited_lengths_into(freqs, max_bits, lens);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn cost(freqs: &[u32], lens: &[u8]) -> u64 {
        freqs.iter().zip(lens).map(|(&f, &l)| f as u64 * l as u64).sum()
    }

    /// Plain Huffman (no length limit) for comparison.
    fn huffman_cost(freqs: &[u32]) -> u64 {
        let mut w: Vec<u64> = freqs.iter().filter(|&&f| f > 0).map(|&f| f as u64).collect();
        if w.len() <= 1 {
            return w.iter().sum();
        }
        let mut total = 0;
        while w.len() > 1 {
            w.sort_unstable();
            let a = w.remove(0);
            let b = w.remove(0);
            total += a + b;
            w.push(a + b);
        }
        total
    }

    fn rng(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn matches_huffman_when_unconstrained_and_is_complete() {
        let mut s = 0x1234_5678_9abc_def0u64;
        for round in 0..300 {
            let n = 2 + (rng(&mut s) % 300) as usize;
            let freqs: Vec<u32> = (0..n)
                .map(|_| if rng(&mut s).is_multiple_of(4) { 0 } else { (rng(&mut s) % (1 + round * 50)) as u32 + 1 })
                .collect();
            let used = freqs.iter().filter(|&&f| f > 0).count();
            if used < 2 {
                continue;
            }
            let lens = limited_lengths(&freqs, 15);
            assert_eq!(kraft(&lens), 32768, "complete code");
            if *lens.iter().max().unwrap() < 15 {
                assert_eq!(cost(&freqs, &lens), huffman_cost(&freqs));
            }
            assert!(lens.iter().all(|&l| l <= 15));
        }
    }

    #[test]
    fn respects_the_limit_and_is_optimal_on_small_cases() {
        // Fibonacci weights force long codes without a limit.
        let mut f = vec![1u32, 1];
        for i in 2..20 {
            let x = f[i - 1] + f[i - 2];
            f.push(x);
        }
        for max in 5..=15u32 {
            let lens = limited_lengths(&f, max);
            assert!(lens.iter().all(|&l| l as u32 <= max && l > 0));
            assert_eq!(kraft(&lens), 32768);
        }
        // Brute force: all length vectors for 5 symbols with max 3 bits.
        let fr = [10u32, 7, 3, 2, 1];
        let lens = limited_lengths(&fr, 3);
        let mut best = u64::MAX;
        for a in 1..=3u8 {
            for b in 1..=3u8 {
                for c in 1..=3u8 {
                    for d in 1..=3u8 {
                        for e in 1..=3u8 {
                            let l = [a, b, c, d, e];
                            if kraft(&l) <= 32768 {
                                best = best.min(cost(&fr, &l));
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(cost(&fr, &lens), best);
    }

    #[test]
    fn fast_lengths_are_optimal() {
        let mut s = 0xfeed_beefu64;
        for round in 0..500 {
            let n = 1 + (rng(&mut s) % 300) as usize;
            let freqs: Vec<u32> = (0..n)
                .map(|_| if rng(&mut s).is_multiple_of(3) { 0 } else { (rng(&mut s) % (1 + round * 20)) as u32 + 1 })
                .collect();
            let mut a = vec![0u8; n];
            let max = fast_lengths(&freqs, &mut a);
            let used = freqs.iter().filter(|&&f| f > 0).count();
            if used >= 2 {
                assert_eq!(kraft_wide(&a), 1u64 << 40, "complete");
                assert_eq!(cost(&freqs, &a), huffman_cost(&freqs), "optimal");
            }
            if max <= 15 {
                let b = limited_lengths(&freqs, 15);
                assert_eq!(cost(&freqs, &a), cost(&freqs, &b));
            }
        }
    }

    fn kraft_wide(lens: &[u8]) -> u64 {
        lens.iter().filter(|&&l| l > 0).map(|&l| 1u64 << (40 - l as u32)).sum()
    }

    #[test]
    fn canonical_codes_match_rfc_example() {
        // RFC 1951 3.2.2 example: lengths (3, 3, 3, 3, 3, 2, 4, 4) give 010 011 100 101 110 00 1110 1111.
        let lens = [3u8, 3, 3, 3, 3, 2, 4, 4];
        let codes = canonical_codes(&lens);
        let expect = [0b010u16, 0b011, 0b100, 0b101, 0b110, 0b00, 0b1110, 0b1111];
        for i in 0..8 {
            assert_eq!(codes[i], reverse_bits(expect[i], lens[i] as u32));
        }
    }

    /// Counts of the kinds that decide the lengths' ties and limits: many equal counts, wide ranges, many
    /// zeros, almost no used symbols, Fibonacci-like counts (long codes) and counts up to 2^31.
    pub(crate) fn random_counts(s: &mut u64, f: &mut [u32]) {
        let n = f.len();
        f.fill(0);
        match rng(s) % 8 {
            0 => f.iter_mut().for_each(|c| *c = if rng(s).is_multiple_of(3) { 0 } else { 1 + (rng(s) % 3) as u32 }),
            1 => {
                let bits = rng(s) % 32;
                f.iter_mut()
                    .for_each(|c| *c = if rng(s).is_multiple_of(4) { 0 } else { (rng(s) % (1u64 << bits)) as u32 + 1 });
            }
            2 => f.iter_mut().for_each(|c| *c = if rng(s).is_multiple_of(10) { 1 + (rng(s) % 1000) as u32 } else { 0 }),
            3 => {
                for _ in 0..rng(s) % 3 {
                    f[(rng(s) % n as u64) as usize] = 1 + (rng(s) % (1 << 31)) as u32;
                }
            }
            4 | 5 => {
                // Fibonacci counts on random symbols, some with small counts besides.
                let (mut a, mut b) = (1u32, 1u32);
                for _ in 0..2 + rng(s) % 45 {
                    f[(rng(s) % n as u64) as usize] = a;
                    (a, b) = (b, a.saturating_add(b).min(1 << 31));
                }
                if rng(s).is_multiple_of(2) {
                    for _ in 0..rng(s) % 8 {
                        f[(rng(s) % n as u64) as usize] = 1 + (rng(s) % 4) as u32;
                    }
                }
            }
            6 => f
                .iter_mut()
                .for_each(|c| *c = if rng(s).is_multiple_of(2) { 0 } else { (1 << 31) - (rng(s) % 4) as u32 }),
            _ => f.iter_mut().for_each(|c| *c = if rng(s).is_multiple_of(3) { 0 } else { 1 << (rng(s) % 20) }),
        }
    }

    #[test]
    fn same_lengths_as_before() {
        let mut s = 0x0dd_ba11_cafe_f00du64;
        let mut f = vec![0u32; 320];
        let (mut a, mut b) = (vec![0u8; 320], vec![0u8; 320]);
        // How often package-merge followed the fast method, at limits 7 and 15.
        let mut fallbacks = [0usize; 2];
        for round in 0..100_000 {
            let n = match round % 4 {
                0 => 19,
                1 => 30,
                2 => 286,
                _ => 1 + (rng(&mut s) % 320) as usize,
            };
            let f = &mut f[..n];
            random_counts(&mut s, f);
            let used = f.iter().filter(|&&c| c > 0).count();
            let (a, b) = (&mut a[..n], &mut b[..n]);
            a.fill(9);
            b.fill(9);
            let max = fast_lengths(f, a);
            assert_eq!(max, old::fast_lengths(f, b), "{f:?}");
            assert_eq!(a, b, "{f:?}");
            for max_bits in [7, 15, 2 + (rng(&mut s) % 19) as u32] {
                if used > 1 << max_bits {
                    continue;
                }
                fallbacks[(max_bits == 15) as usize] += (max as u32 > max_bits && max_bits % 8 == 7) as usize;
                lengths_into(f, max_bits, a);
                old::lengths_into(f, max_bits, b);
                assert_eq!(a, b, "{max_bits} {f:?}");
                // Package-merge alone, on the larger alphabets only now and then (the old one is slow).
                if n <= 32 || round % 8 < 2 {
                    a.fill(9);
                    b.fill(9);
                    limited_lengths_into(f, max_bits, a);
                    old::limited_lengths_into(f, max_bits, b);
                    assert_eq!(a, b, "{max_bits} {f:?}");
                }
            }
        }
        assert!(fallbacks.iter().all(|&k| k > 5000), "{fallbacks:?}");
    }

    #[test]
    fn same_lengths_as_before_beyond_deflate() {
        // Alphabets larger than the stack arrays and limits above 15 take the heap paths.
        let mut s = 0x5ca1_ab1e_u64;
        for _ in 0..300 {
            let n = 1 + (rng(&mut s) % 700) as usize;
            let mut f = vec![0u32; n];
            random_counts(&mut s, &mut f);
            let used = f.iter().filter(|&&c| c > 0).count();
            let (mut a, mut b) = (vec![9u8; n], vec![9u8; n]);
            if used <= 320 {
                assert_eq!(fast_lengths(&f, &mut a), old::fast_lengths(&f, &mut b));
                assert_eq!(a, b);
            }
            for max_bits in [10, 15, 16 + (rng(&mut s) % 30) as u32] {
                if used > 1 << max_bits {
                    continue;
                }
                limited_lengths_into(&f, max_bits, &mut a);
                old::limited_lengths_into(&f, max_bits, &mut b);
                assert_eq!(a, b, "{max_bits} {f:?}");
            }
        }
    }

    /// Steps of a random walk over related counts, like the block-split search: blocks of symbols added and
    /// taken away, now and then counts that have nothing to do with the last ones.
    pub(crate) fn walk(s: &mut u64, f: &mut [u32]) {
        let n = f.len() as u64;
        match rng(s) % 16 {
            0 => random_counts(s, f),
            1..=7 => {
                for _ in 0..1 + rng(s) % 40 {
                    let k = (rng(s) % n) as usize;
                    f[k] = f[k].saturating_add(1 + (rng(s) % 3) as u32);
                }
            }
            _ => {
                for _ in 0..1 + rng(s) % 40 {
                    let k = (rng(s) % n) as usize;
                    f[k] = f[k].saturating_sub(1 + (rng(s) % 3) as u32);
                }
            }
        }
    }

    fn length_coder_walk<const N: usize>(seed: u64, steps: usize, max_bits: u32) {
        let mut s = seed;
        let mut c = LengthCoder::<N>::new();
        let mut f = [0u32; N];
        let mut b = [0u8; N];
        for _ in 0..steps {
            walk(&mut s, &mut f);
            if f.iter().filter(|&&x| x > 0).count() > 1 << max_bits {
                continue;
            }
            let bits = c.lengths(&f, max_bits);
            old::lengths_into(&f, max_bits, &mut b);
            assert_eq!(c.lens(), &b, "{f:?}");
            assert_eq!(bits, cost(&f, &b));
            assert_eq!(c.used(), f.iter().filter(|&&x| x > 0).count());
        }
    }

    /// Counts most of which are below 256, as `Runs` takes them: a few counts many symbols share, some of 256
    /// or more (up to a dozen, around `RUN_BIG`), sometimes counts that make a deep tree, sometimes few symbols.
    fn small_counts(s: &mut u64, f: &mut [u32]) {
        let n = f.len() as u64;
        let used = match rng(s) % 4 {
            0 => 30 + rng(s) % 30,
            _ => n / 4 + rng(s) % n,
        };
        let top = [4, 16, 64, 256][(rng(s) % 4) as usize];
        f.fill(0);
        for _ in 0..used {
            f[(rng(s) % n) as usize] = 1 + (rng(s) % top) as u32;
        }
        for _ in 0..rng(s) % 13 {
            f[(rng(s) % n) as usize] = 256 + (rng(s) % (1 << (rng(s) % 24))) as u32;
        }
        if rng(s).is_multiple_of(4) {
            let (mut a, mut b) = (1u32, 1u32);
            for _ in 0..rng(s) % 30 {
                f[(rng(s) % n) as usize] = a;
                (a, b) = (b, a + b);
            }
        }
    }

    /// The block-split search's walk: the sums of runs of blocks `i..j`, `i` going up for each `j`.
    fn split_walk<const N: usize>(seed: u64, blocks: usize, max_bits: u32) {
        let mut s = seed;
        let mut c = LengthCoder::<N>::new();
        let mut pref = vec![[0u32; N]];
        for _ in 0..blocks {
            let mut block = [0u32; N];
            small_counts(&mut s, &mut block);
            for x in block.iter_mut() {
                *x /= 1 + (rng(&mut s) % 16) as u32;
            }
            let mut sum = *pref.last().unwrap();
            sum.iter_mut().zip(&block).for_each(|(a, b)| *a += b);
            pref.push(sum);
        }
        let mut b = [0u8; N];
        for j in 1..pref.len() {
            for i in j.saturating_sub(24)..j {
                let mut f = pref[j];
                f.iter_mut().zip(&pref[i]).for_each(|(a, b)| *a -= b);
                if f.iter().filter(|&&x| x > 0).count() > 1 << max_bits {
                    continue;
                }
                let bits = c.lengths(&f, max_bits);
                old::lengths_into(&f, max_bits, &mut b);
                assert_eq!(c.lens(), &b, "{f:?}");
                assert_eq!(bits, cost(&f, &b));
            }
        }
    }

    #[test]
    fn length_coder_by_runs_keeps_the_lengths_of_before() {
        // Walks mostly in the runs' range: few counts many symbols share, some large counts, now and then a
        // deep tree or few symbols, so that the runs, the sorted keys and package-merge take turns.
        for (seed, max_bits) in [(11, 15), (12, 15), (13, 9), (14, 12)] {
            let mut s = seed;
            let mut c = LengthCoder::<286>::new();
            let mut f = [0u32; 286];
            let mut b = [0u8; 286];
            for step in 0..20_000 {
                match rng(&mut s) % 32 {
                    0 => small_counts(&mut s, &mut f),
                    1 => random_counts(&mut s, &mut f),
                    _ => walk(&mut s, &mut f),
                }
                if step % 3 == 0 {
                    // Back towards small counts, as the walk adds more than it takes away.
                    for x in f.iter_mut().filter(|x| **x > 300) {
                        *x /= 2;
                    }
                }
                let bits = c.lengths(&f, max_bits);
                old::lengths_into(&f, max_bits, &mut b);
                assert_eq!(c.lens(), &b, "{max_bits} {f:?}");
                assert_eq!(bits, cost(&f, &b));
                assert_eq!(c.used(), f.iter().filter(|&&x| x > 0).count());
            }
        }
        split_walk::<286>(21, 300, 15);
        split_walk::<286>(22, 300, 10);
        split_walk::<320>(23, 200, 15);
        split_walk::<48>(24, 300, 15);
        split_walk::<64>(25, 300, 7);
        split_walk::<30>(26, 300, 15);
        split_walk::<19>(27, 300, 7);
    }

    #[test]
    fn runs_are_as_small_as_the_coder_says() {
        assert!(std::mem::size_of::<Runs>() < 28 << 10, "{}", std::mem::size_of::<Runs>());
    }

    #[test]
    fn guided_package_merge_gives_the_lengths_of_before() {
        // Package-merge with a guide from the last run, from a run on other counts, or made up: the lengths are
        // those of the whole lists every time.
        let mut s = 0x9e1d_ed00u64;
        let mut guide = Guide::new();
        let (mut a, mut b) = ([0u8; 320], [0u8; 320]);
        let mut f = [0u32; 320];
        let (mut guided, mut cut_short) = (0, 0);
        for round in 0..8000 {
            let n = [286, 320, 200, 140][round % 4];
            let f = &mut f[..n];
            match rng(&mut s) % 4 {
                0 => {
                    // Many symbols and a deep tree.
                    small_counts(&mut s, f);
                    let (mut x, mut y) = (1u32, 2u32);
                    for _ in 0..24 {
                        f[(rng(&mut s) % n as u64) as usize] = x;
                        (x, y) = (y, x + y);
                    }
                }
                1 => walk(&mut s, f),
                2 => {
                    guide.want.iter_mut().for_each(|w| *w = (rng(&mut s) % 700) as usize);
                    guide.cap = (rng(&mut s) % 700) as usize;
                }
                _ => {}
            }
            let mut keys = [0u64; 320];
            let mut room = [0u64; 320];
            let used = sorted_keys(f, &mut keys, &mut room);
            let max_bits = [15, 15, 12, 9][round % 4];
            if used < 3 || used > 1 << max_bits {
                continue;
            }
            guided += (guide.levels == max_bits as usize && used >= 128) as usize;
            a[..n].fill(0);
            limited_sorted(&keys[..used], max_bits, &mut a, &mut guide);
            old::limited_lengths_into(f, max_bits, &mut b[..n]);
            assert_eq!(a[..n], b[..n], "{max_bits} {f:?}");
            // Lists cut too short say so, and long enough ones give the lengths of before.
            let (levels, cap) = (max_bits as usize, 2 * used - 2);
            let limit = [8 + (rng(&mut s) % cap as u64) as usize; 15];
            let (mut want, mut taken) = ([0; 15], [0; 15]);
            let built = &mut [(0, false); 15];
            let keys = &keys[..used];
            if package_merge::<true>(
                keys,
                levels,
                &limit,
                &mut [0; 638],
                &mut [0; 638],
                &mut [0; 150],
                built,
                &mut want,
                &mut taken,
            ) {
                a[..n].fill(0);
                assign(keys, &mut taken[..levels], &mut a);
                assert_eq!(a[..n], b[..n], "{max_bits} {f:?}");
            } else {
                cut_short += 1;
            }
        }
        assert!(guided > 1000 && cut_short > 1000, "{guided} {cut_short}");
    }

    #[test]
    fn length_coder_keeps_the_lengths_of_before() {
        length_coder_walk::<19>(1, 20_000, 7);
        length_coder_walk::<30>(2, 20_000, 15);
        length_coder_walk::<30>(3, 5_000, 7);
        length_coder_walk::<286>(4, 20_000, 15);
        length_coder_walk::<320>(5, 3_000, 15);
        length_coder_walk::<1>(6, 100, 15);
        length_coder_walk::<2>(7, 100, 15);
        length_coder_walk::<3>(8, 1_000, 2);
    }

    #[test]
    fn length_coder_follows_the_limit_from_call_to_call() {
        // The limit changes from one call to the next, down to 0 (for at most two symbols), also right after
        // calls the runs answered: nothing of an earlier call is taken for this one.
        fn steps<const N: usize>(seed: u64, steps: usize) {
            let mut s = seed;
            let mut c = LengthCoder::<N>::new();
            let (mut f, mut b) = ([0u32; N], [0u8; N]);
            for _ in 0..steps {
                let max_bits = [0, 1, 2, 7, 15, 15, 15, 15][(rng(&mut s) % 8) as usize];
                let most = (1usize << max_bits).max(2);
                match rng(&mut s) % 2 {
                    _ if max_bits <= 2 => {
                        f.fill(0);
                        for _ in 0..rng(&mut s) % (most as u64 + 1) {
                            f[(rng(&mut s) % N as u64) as usize] = 1 + (rng(&mut s) % 300) as u32;
                        }
                    }
                    0 => small_counts(&mut s, &mut f),
                    _ => walk(&mut s, &mut f),
                }
                if f.iter().filter(|&&x| x > 0).count() > most {
                    continue;
                }
                let bits = c.lengths(&f, max_bits);
                old::lengths_into(&f, max_bits, &mut b);
                assert_eq!(c.lens(), &b, "{max_bits} {f:?}");
                assert_eq!(bits, cost(&f, &b));
                assert_eq!(c.used(), f.iter().filter(|&&x| x > 0).count());
            }
            // The runs are not made for alphabets too large for them.
            assert!(N <= RUN_SYMS || c.runs.is_none());
        }
        steps::<286>(31, 6_000);
        steps::<320>(32, 3_000);
        steps::<48>(33, 6_000);
        steps::<30>(34, 6_000);
        steps::<19>(35, 6_000);
        steps::<400>(36, 1_000);
    }
}
