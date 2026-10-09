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
    with_scratch(freqs.len(), |keys| {
        let n = sorted_keys(freqs, keys);
        limited_sorted(&keys[..n], max_bits, lens);
    });
}

/// Huffman code lengths without a length limit, computed in place (Moffat and Katajainen's method), with no
/// heap allocation for alphabets up to 320 symbols. Returns the longest length; callers fall back to
/// `limited_lengths` when it exceeds their limit. `lens` receives one length per symbol (0 for unused symbols).
pub fn fast_lengths(freqs: &[u32], lens: &mut [u8]) -> u8 {
    lens[..freqs.len()].fill(0);
    with_scratch(freqs.len(), |keys| {
        let n = sorted_keys(freqs, keys);
        with_scratch(n, |work| unlimited_sorted(&keys[..n], work, lens).0)
    })
}

/// Optimal lengths limited to `max_bits`: the fast method when it fits, package-merge otherwise (on the keys
/// already sorted).
pub fn lengths_into(freqs: &[u32], max_bits: u32, lens: &mut [u8]) {
    lens[..freqs.len()].fill(0);
    with_scratch(freqs.len(), |keys| {
        let n = sorted_keys(freqs, keys);
        let keys = &keys[..n];
        if with_scratch(n, |work| unlimited_sorted(keys, work, lens).0) as u32 > max_bits {
            lens.fill(0);
            limited_sorted(keys, max_bits, lens);
        }
    })
}

/// Code lengths for one alphabet, worked out again and again for counts that change a little from one call
/// to the next (the block-split search). The used symbols' order is kept from the last call: the symbols whose
/// count stayed keep their places and only the others are sorted again. Nothing is allocated for Deflate's
/// alphabets and limits, and the lengths are those of `lengths_into`.
pub struct LengthCoder<const N: usize> {
    /// The keys of the symbols used in the last call, sorted, in `keys[..used]`.
    keys: [u64; N],
    used: usize,
    /// Whether each symbol is among them.
    member: [bool; N],
    work: [u64; N],
    /// The lengths of the last call, one per symbol.
    pub lens: [u8; N],
}

impl<const N: usize> Default for LengthCoder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> LengthCoder<N> {
    pub const fn new() -> Self {
        const { assert!(N <= 1 << 16, "symbols must fit in 16 bits") };
        LengthCoder { keys: [0; N], used: 0, member: [false; N], work: [0; N], lens: [0; N] }
    }

    /// Sets `lens` to what `lengths_into(freqs, max_bits, ..)` gives and returns the sum of count times
    /// length.
    pub fn lengths(&mut self, freqs: &[u32; N], max_bits: u32) -> u64 {
        // The symbols used last time whose count stayed keep their order; the others are taken out (the ones
        // still used go to `work`), sorted on their own and merged back, and so are the newly used symbols.
        let (mut kept, mut moved) = (0, 0);
        for k in 0..self.used {
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
        let added = freqs.iter().filter(|&&f| f != 0).count() - kept - moved;
        // The keys that moved are often still nearly in order.
        if !insertion_sort(&mut self.work[..moved], 8 * moved) {
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
            sort(&mut self.work[..a]);
            merge(&mut self.keys[..self.used + a], self.used, &self.work[..a]);
            self.used += a;
        }
        let keys = &self.keys[..self.used];
        self.lens.fill(0);
        let (max, bits) = unlimited_sorted(keys, &mut self.work, &mut self.lens);
        if max as u32 <= max_bits {
            return bits;
        }
        limited_sorted(keys, max_bits, &mut self.lens);
        keys.iter().map(|&k| (k >> 16) * self.lens[(k & 0xffff) as usize] as u64).sum()
    }

    /// How many symbols had a nonzero count in the last call.
    pub fn used(&self) -> usize {
        self.used
    }
}

/// Merges the sorted `add` into the sorted first `m` entries of `keys`, which has room for both.
fn merge(keys: &mut [u64], m: usize, add: &[u64]) {
    let (mut i, mut j) = (m, add.len());
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

/// Runs `f` on a zeroed scratch array of at least `len` entries, on the stack for Deflate's alphabets.
fn with_scratch<R>(len: usize, f: impl FnOnce(&mut [u64]) -> R) -> R {
    if len <= 32 {
        f(&mut [0; 32])
    } else if len <= 320 {
        f(&mut [0; 320])
    } else {
        f(&mut vec![0; len])
    }
}

/// Writes the keys of the used symbols into `keys` (as long as `freqs` at least), sorted, and returns how
/// many there are.
fn sorted_keys(freqs: &[u32], keys: &mut [u64]) -> usize {
    let mut n = 0;
    for (i, &f) in freqs.iter().enumerate() {
        keys[n] = ((f as u64) << 16) | i as u64;
        n += (f > 0) as usize;
    }
    sort(&mut keys[..n]);
    n
}

/// Sorts keys: by insertion when there are few.
fn sort(keys: &mut [u64]) {
    if keys.len() <= 32 {
        insertion_sort(keys, usize::MAX);
    } else {
        keys.sort_unstable();
    }
}

/// Sorts `keys` by insertion, which is quick when they are nearly in order. Gives up (returning false, the
/// keys in some order) once that has moved keys more than `budget` places in all.
fn insertion_sort(keys: &mut [u64], budget: usize) -> bool {
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

/// Huffman lengths without a limit for the used symbols, given their sorted keys, by Moffat and Katajainen's
/// in-place method on `work` (at least as long as `keys`). Writes each used symbol's length into `lens` and
/// returns the longest and the sum of count times length.
fn unlimited_sorted(keys: &[u64], work: &mut [u64], lens: &mut [u8]) -> (u8, u64) {
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
    let a = &mut work[..n];
    for (x, &k) in a.iter_mut().zip(keys) {
        *x = k >> 16;
    }
    // Pass 1: internal node weights, left to right; each takes the lighter of the next leaf and the next
    // internal node, twice (the leaf when they are equal). Their sum is the sum of count times length.
    a[0] += a[1];
    let mut bits = a[0];
    let (mut root, mut leaf) = (0usize, 2usize);
    for next in 1..n - 1 {
        let x = if leaf < n && a[leaf] <= a[root] {
            leaf += 1;
            a[leaf - 1]
        } else {
            let w = a[root];
            a[root] = next as u64;
            root += 1;
            w
        };
        let y = if leaf < n && (root >= next || a[leaf] <= a[root]) {
            leaf += 1;
            a[leaf - 1]
        } else {
            let w = a[root];
            a[root] = next as u64;
            root += 1;
            w
        };
        a[next] = x + y;
        bits += x + y;
    }
    // Pass 2: internal node depths, right to left.
    a[n - 2] = 0;
    for next in (0..n - 2).rev() {
        a[next] = a[a[next] as usize] + 1;
    }
    // Pass 3: leaf depths, the heaviest leaf first, straight into the lengths.
    let (mut avbl, mut used, mut depth) = (1i64, 0i64, 0u64);
    let (mut root, mut next) = (n as isize - 2, n as isize - 1);
    while avbl > 0 {
        while root >= 0 && a[root as usize] == depth {
            used += 1;
            root -= 1;
        }
        while avbl > used {
            lens[(keys[next as usize] & 0xffff) as usize] = depth.min(255) as u8;
            next -= 1;
            avbl -= 1;
        }
        avbl = 2 * used;
        depth += 1;
        used = 0;
    }
    // The lightest leaf is the deepest.
    (lens[(keys[0] & 0xffff) as usize], bits)
}

/// Package-merge lengths for the used symbols, given their sorted keys; other entries of `lens` are left
/// alone. The lists live in arrays on the stack for Deflate's alphabets and limits.
fn limited_sorted(keys: &[u64], max_bits: u32, lens: &mut [u8]) {
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
    let mut taken = [0usize; 15];
    if levels <= 15 && n <= 32 {
        package_merge(keys, levels, &mut [0; 62], &mut [0; 62], &mut [0; 15], &mut taken);
        assign(keys, &mut taken[..levels], lens);
    } else if levels <= 15 && n <= 320 {
        package_merge(keys, levels, &mut [0; 638], &mut [0; 638], &mut [0; 150], &mut taken);
        assign(keys, &mut taken[..levels], lens);
    } else {
        let mut taken = vec![0; levels];
        let mut leaf = vec![0; levels * cap.div_ceil(64)];
        package_merge(keys, levels, &mut vec![0; cap], &mut vec![0; cap], &mut leaf, &mut taken);
        assign(keys, &mut taken, lens);
    }
}

/// Package-merge on n >= 3 leaves given by their sorted keys. Level 0 (the deepest) holds the leaves; each
/// higher level merges the leaves with pairs ("packages") of the level below, kept sorted and cut at 2n-2
/// items, a leaf before a package of equal weight. Only the weights of the level below are kept (`prev`,
/// `cur`: 2n-2 entries each), with one bit per item that says whether it is a leaf (`leaf`: all zero on entry,
/// level by level, each level's 2n-2 bits in whole words). Returns in `taken[lvl]` how many leaves the items
/// taken from each level hold.
fn package_merge<'a>(
    keys: &[u64],
    levels: usize,
    mut prev: &'a mut [u64],
    mut cur: &'a mut [u64],
    leaf: &mut [u64],
    taken: &mut [usize],
) {
    let n = keys.len();
    let cap = 2 * n - 2;
    let words = cap.div_ceil(64);
    for (p, &k) in prev.iter_mut().zip(keys) {
        *p = k >> 16;
    }
    // While building, `taken[lvl]` holds the length of each level's list. Once a list's weights equal those
    // of the level below, every level above repeats it: they are not built again.
    let mut len = n;
    taken[0] = n;
    let mut top = levels;
    for lvl in 1..levels {
        let bits = &mut leaf[lvl * words..(lvl + 1) * words];
        let npk = len / 2;
        let package = |j: usize| if j < npk { prev[2 * j] + prev[2 * j + 1] } else { u64::MAX };
        let (mut i, mut j, mut k) = (0usize, 0usize, 0usize);
        let mut pw = package(0);
        while k < cap {
            if i < n && (keys[i] >> 16) <= pw {
                cur[k] = keys[i] >> 16;
                bits[k / 64] |= 1 << (k % 64);
                i += 1;
            } else if j < npk {
                cur[k] = pw;
                j += 1;
                pw = package(j);
            } else {
                break;
            }
            k += 1;
        }
        let same = k == len && cur[..k] == prev[..k];
        len = k;
        taken[lvl] = k;
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
        let take = m.min(taken[src]);
        let nl = if src == 0 { take } else { count_ones(&leaf[src * words..], take) };
        taken[lvl] = nl;
        m = 2 * (take - nl);
    }
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
            assert_eq!(c.lens, b, "{f:?}");
            assert_eq!(bits, cost(&f, &b));
            assert_eq!(c.used(), f.iter().filter(|&&x| x > 0).count());
        }
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
}
