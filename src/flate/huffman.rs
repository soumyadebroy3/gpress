//! Huffman code lengths (optimal under a maximum length) and canonical codes.

/// Optimal code lengths for `freqs` with no code longer than `max_bits`, by package-merge
/// (Larmore and Hirschberg). Symbols with frequency 0 get length 0. One used symbol gets length 1.
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
    (code.reverse_bits() >> (16 - len)) as u16
}

/// Kraft sum scaled by 2^15: equals 32768 for a complete code.
pub fn kraft(lens: &[u8]) -> u64 {
    lens.iter().filter(|&&l| l > 0).map(|&l| 1u64 << (15 - l as u32)).sum()
}

#[cfg(test)]
mod tests {
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
                .map(|_| if rng(&mut s) % 4 == 0 { 0 } else { (rng(&mut s) % (1 + round * 50)) as u32 + 1 })
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
    fn canonical_codes_match_rfc_example() {
        // RFC 1951 3.2.2 example: lengths (3, 3, 3, 3, 3, 2, 4, 4) give 010 011 100 101 110 00 1110 1111.
        let lens = [3u8, 3, 3, 3, 3, 2, 4, 4];
        let codes = canonical_codes(&lens);
        let expect = [0b010u16, 0b011, 0b100, 0b101, 0b110, 0b00, 0b1110, 0b1111];
        for i in 0..8 {
            assert_eq!(codes[i], reverse_bits(expect[i], lens[i] as u32));
        }
    }
}
