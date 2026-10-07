//! Finds, for every position, the smallest distance for every possible match length.
//!
//! The result is a step function per position: lengths up to `steps[k].0` can use distance `steps[k].1`,
//! with lengths and distances both increasing. The optimal parser reads it to price every length.

use super::tables::*;

const HASH_BITS: u32 = 16;
const HASH_SIZE: usize = 1 << HASH_BITS;
const WMASK: usize = WINDOW - 1;

pub struct Matches {
    /// Position `from + k` has steps `steps[start[k]..start[k + 1]]`.
    pub start: Vec<u32>,
    pub steps: Vec<(u16, u16)>,
    pub from: usize,
    pub to: usize,
}

impl Matches {
    #[inline]
    pub fn at(&self, pos: usize) -> &[(u16, u16)] {
        let k = pos - self.from;
        &self.steps[self.start[k] as usize..self.start[k + 1] as usize]
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
/// (into data before `from` too). `max_chain` bounds how many earlier positions are compared.
pub fn find_matches(data: &[u8], from: usize, to: usize, max_chain: usize) -> Matches {
    let hist = from.saturating_sub(WINDOW);
    let n = to - from;
    let mut start = Vec::with_capacity(n + 1);
    let mut steps: Vec<(u16, u16)> = Vec::with_capacity(n * 2);

    // Run lengths of equal bytes (cut at `to`), to skip comparisons inside runs.
    let mut same = vec![0u16; to - hist];
    for i in (hist..to).rev() {
        let k = i - hist;
        same[k] = if i + 1 < to && data[i + 1] == data[i] { same[k + 1].saturating_add(1) } else { 1 };
    }

    let mut head = vec![-1i32; HASH_SIZE];
    let mut prev = vec![-1i32; WINDOW];
    for i in hist..to {
        if i >= from {
            start.push(steps.len() as u32);
            let maxlen = (to - i).min(MAX_MATCH);
            if maxlen >= MIN_MATCH {
                let h = hash3(data, i);
                let mut p = head[h];
                let mut best = MIN_MATCH - 1;
                let mut chain = 0usize;
                let si = same[i - hist] as usize;
                while p >= 0 && chain < max_chain {
                    let pu = p as usize;
                    let dist = i - pu;
                    if dist > WINDOW {
                        break;
                    }
                    chain += 1;
                    if data[pu] == data[i] && data[pu + best] == data[i + best] {
                        let sp = same[pu - hist] as usize;
                        let skip = si.min(sp).min(maxlen);
                        // Inside runs the common prefix is at least the shorter run.
                        let len = common(data, pu, i, skip, maxlen);
                        if len > best {
                            steps.push((len as u16, dist as u16));
                            best = len;
                            if len >= maxlen {
                                break;
                            }
                        }
                    }
                    let np = prev[pu & WMASK];
                    if np >= p {
                        break;
                    }
                    p = np;
                }
            }
        }
        if i + MIN_MATCH <= to {
            let h = hash3(data, i);
            prev[i & WMASK] = head[h];
            head[h] = i as i32;
        }
    }
    start.push(steps.len() as u32);
    Matches { start, steps, from, to }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute force: for each length, the smallest distance.
    fn brute(data: &[u8], from: usize, to: usize, i: usize) -> Vec<(u16, u16)> {
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
        let _ = from;
        out
    }

    #[test]
    fn exhaustive_search_matches_brute_force() {
        let mut s = 7u64;
        let mut data = Vec::new();
        for k in 0..6000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let b = match k % 300 {
                0..=99 => (s % 4) as u8,
                100..=149 => 0,
                _ => b"the quick brown fox "[k % 20],
            };
            data.push(b);
        }
        let (from, to) = (1000, 5000);
        let m = find_matches(&data, from, to, usize::MAX);
        for i in from..to {
            assert_eq!(m.at(i), &brute(&data, from, to, i)[..], "position {i}");
        }
    }
}
