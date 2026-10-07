//! Finds, for every position, the smallest distance for every possible match length.
//!
//! The result is a step function per position: lengths up to `steps[k].0` can use distance `steps[k].1`,
//! with lengths and distances both increasing. The optimal parser reads it to price every length.

use super::tables::*;

const HASH_BITS: u32 = 16;
const HASH_SIZE: usize = 1 << HASH_BITS;

pub struct Matches {
    /// Position `from + k` has steps `steps[start[k]..start[k + 1]]`.
    pub start: Vec<u32>,
    pub steps: Vec<(u16, u16)>,
    /// Equal bytes starting at each position (itself included), cut at `to` and at 65535.
    pub same: Vec<u16>,
    pub from: usize,
    pub to: usize,
}

impl Matches {
    #[inline]
    pub fn at(&self, pos: usize) -> &[(u16, u16)] {
        let k = pos - self.from;
        &self.steps[self.start[k] as usize..self.start[k + 1] as usize]
    }
    #[inline]
    pub fn same_at(&self, pos: usize) -> usize {
        self.same[pos - self.from] as usize
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

/// Matches for positions `from..to`. Matches never reach past `to` and look back at most 32 KiB
/// (into data before `from` too). `max_depth` bounds how many earlier positions are compared.
///
/// Positions are kept in binary search trees (one per 3-byte hash), ordered by the text that follows them
/// and with newer positions above older ones. Walking down a tree visits ever older positions with ever
/// longer common prefixes, which gives exactly the smallest distance for each length. Each comparison
/// starts after the prefix already known to match, so repetitive data stays fast.
pub fn find_matches(data: &[u8], from: usize, to: usize, max_depth: usize, alternatives: bool) -> Matches {
    const NIL: u32 = u32::MAX;
    let hist = from.saturating_sub(WINDOW);
    let n = to - from;
    let mut start = Vec::with_capacity(n + 1);
    let mut steps: Vec<(u16, u16)> = Vec::with_capacity(n * 2);

    let mut same = vec![0u16; to - hist];
    for i in (hist..to).rev() {
        let k = i - hist;
        same[k] = if i + 1 < to && data[i + 1] == data[i] { same[k + 1].saturating_add(1) } else { 1 };
    }

    // Tree slots cycle every 64 KiB: twice the window, so the node exactly 32 KiB back (a legal distance)
    // never shares a slot with the position being inserted.
    const CYC: usize = 2 * WINDOW;
    const CMASK: usize = CYC - 1;
    let mut head = vec![NIL; HASH_SIZE];
    let mut son = vec![NIL; 2 * CYC];
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
    }
    start.push(steps.len() as u32);
    let same_seg = same[from - hist..].to_vec();
    Matches { start, steps, same: same_seg, from, to }
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
                assert_eq!(m.at(i), &brute(&data, to, i)[..], "position {i}");
            }
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
                for &(l, d) in m.at(i) {
                    let (l, d) = (l as usize, d as usize);
                    assert!(d >= 1 && d <= WINDOW && d <= i, "pos {i}: dist {d}");
                    assert!(i + l <= to, "pos {i}: len {l} past end");
                    assert!(l > prev.0 as usize && (d > prev.1 as usize), "pos {i}: steps not increasing {:?}", m.at(i));
                    assert_eq!(&data[i - d..i - d + l], &data[i..i + l], "pos {i}: len {l} dist {d}");
                    prev = (l as u16, d as u16);
                }
            }
        }
    }
}
