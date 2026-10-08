//! JPEG Huffman tables: reading them (checked as libjpeg checks them), and building optimal ones.

/// A table as stored in a DHT segment: how many codes each length 1..=16 has, and the symbols in code order.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub counts: [u8; 16],
    pub values: Vec<u8>,
}

/// A table ready for decoding (canonical codes, most significant bit first).
#[derive(Clone, Debug)]
pub struct DecodeTable {
    /// The first 9 bits of the input -> (length << 8 | symbol) for codes up to 9 bits; 0 for longer codes.
    fast: Vec<u16>,
    /// The largest code of each length (-1: none), and the offset from a code to its symbol's index.
    maxcode: [i32; 17],
    valoff: [i32; 17],
    values: Vec<u8>,
}

pub const FAST_BITS: u32 = 9;

impl DecodeTable {
    /// Checks the table the way libjpeg does: no length overfull, no code of all one bits (the standard
    /// reserves it), DC symbols at most 15.
    pub fn new(spec: &Spec, dc: bool) -> Result<DecodeTable, &'static str> {
        let total: usize = spec.counts.iter().map(|&c| c as usize).sum();
        if total > 256 || total != spec.values.len() {
            return Err("bad Huffman table");
        }
        if dc && spec.values.iter().any(|&v| v > 15) {
            return Err("bad DC Huffman table");
        }
        let mut maxcode = [-1i32; 17];
        let mut valoff = [0i32; 17];
        let mut fast = vec![0u16; 1 << FAST_BITS];
        let (mut code, mut k) = (0u32, 0usize);
        for len in 1..=16usize {
            let c = spec.counts[len - 1] as u32;
            if c > 0 {
                // The codes of this length are code..code+c; the last one must not be all ones.
                if code + c >= 1 << len {
                    return Err("bad Huffman table");
                }
                valoff[len] = k as i32 - code as i32;
                for i in 0..c {
                    if len as u32 <= FAST_BITS {
                        let shift = FAST_BITS - len as u32;
                        let lo = ((code + i) << shift) as usize;
                        let hi = ((code + i + 1) << shift) as usize;
                        let e = (len as u16) << 8 | spec.values[k + i as usize] as u16;
                        fast[lo..hi].iter_mut().for_each(|x| *x = e);
                    }
                }
                code += c;
                k += c as usize;
                maxcode[len] = code as i32 - 1;
            }
            code <<= 1;
        }
        Ok(DecodeTable { fast, maxcode, valoff, values: spec.values.clone() })
    }

    /// The symbol whose code starts `peek` (16 bits, most significant first), and the code's length.
    #[inline]
    pub fn lookup(&self, peek: u32) -> Option<(u32, u8)> {
        let e = self.fast[(peek >> (16 - FAST_BITS)) as usize];
        if e != 0 {
            return Some(((e >> 8) as u32, e as u8));
        }
        for len in FAST_BITS as usize + 1..=16 {
            let code = (peek >> (16 - len)) as i32;
            if code <= self.maxcode[len] {
                let i = code + self.valoff[len];
                return self.values.get(usize::try_from(i).ok()?).map(|&v| (len as u32, v));
            }
        }
        None
    }
}

/// Optimal code lengths (at most 16 bits) for the symbol counts `freq`, among the codes that leave the
/// all-ones code word unused, as the standard requires. Unused symbols get 0.
///
/// This is package-merge (Larmore and Hirschberg) over the used symbols plus one reserved code word of weight
/// 0: an optimal code for those, with the reserved word taken out, is an optimal code that leaves a word
/// unused, and a code with a free word has no all-ones code (see `table`).
pub fn optimal_lengths(freq: &[u64; 256]) -> [u8; 256] {
    let mut lens = [0u8; 256];
    let mut leaves: Vec<(u64, usize)> = freq.iter().enumerate().filter(|(_, &f)| f > 0).map(|(i, &f)| (f, i)).collect();
    if leaves.is_empty() {
        return lens;
    }
    const RESERVED: usize = usize::MAX;
    leaves.push((0, RESERVED));
    leaves.sort_unstable();
    let n = leaves.len();
    let levels = 16.min(n - 1);
    let cap = 2 * n - 2;
    // lists[0] is the deepest level (leaves only); each level above merges the leaves with pairs
    // ("packages") of the level below, kept sorted and cut at 2n-2 items.
    let mut lists: Vec<Vec<(u64, bool)>> = Vec::with_capacity(levels);
    lists.push(leaves.iter().map(|&(w, _)| (w, true)).collect());
    for _ in 1..levels {
        let prev = &lists[lists.len() - 1];
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
    // Take the cheapest 2n-2 items of the top level; each package taken pulls in two items of the level
    // below. A symbol's length is the number of levels whose taken items include its leaf (always a prefix
    // of the sorted leaves).
    let mut depth = vec![0u8; n];
    let mut m = cap;
    for list in lists.iter().rev() {
        let take = m.min(list.len());
        let leaves_taken = list[..take].iter().filter(|it| it.1).count();
        depth[..leaves_taken].iter_mut().for_each(|d| *d += 1);
        m = 2 * (take - leaves_taken);
    }
    for (k, &(_, sym)) in leaves.iter().enumerate() {
        if sym != RESERVED {
            lens[sym] = depth[k];
        }
    }
    lens
}

/// The order of the symbols of one code length in a table. Any order is a valid table with the same code
/// lengths; the orders differ in which symbol gets which code word, and so in how often a 0xFF byte (which
/// costs a stuffed 0x00 after it) comes out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Order {
    /// By symbol value (as libjpeg writes them).
    Value,
    /// Most frequent symbol first (the smallest code words of its length).
    Frequent,
    /// The most frequent symbols on the code words with the fewest one bits.
    FewestOnes,
}

/// The DHT form of a code: symbols by length, within a length in the given order. None if the lengths are not
/// a valid JPEG code (longer than 16 bits, or no free code word).
pub fn table(lens: &[u8; 256], freq: &[u64; 256], order: Order) -> Option<Spec> {
    let mut counts = [0u8; 16];
    let mut kraft: u64 = 0;
    for &l in lens.iter().filter(|&&l| l > 0) {
        if l > 16 {
            return None;
        }
        counts[l as usize - 1] += 1;
        kraft += 1 << (16 - l);
    }
    // A free code word: then no code is all ones.
    if kraft >= 1 << 16 {
        return None;
    }
    let mut values = Vec::new();
    let mut first = 0u32; // the first code word of the current length
    for len in 1..=16u8 {
        let mut syms: Vec<usize> = (0..256).filter(|&s| lens[s] == len).collect();
        if order != Order::Value {
            syms.sort_by(|&a, &b| freq[b].cmp(&freq[a]).then(a.cmp(&b)));
        }
        if order == Order::FewestOnes {
            let mut slots: Vec<usize> = (0..syms.len()).collect();
            slots.sort_by_key(|&i| ((first + i as u32).count_ones(), i));
            let mut placed = vec![0usize; syms.len()];
            for (k, &slot) in slots.iter().enumerate() {
                placed[slot] = syms[k];
            }
            syms = placed;
        }
        values.extend(syms.iter().map(|&s| s as u8));
        first = (first + counts[len as usize - 1] as u32) << 1;
    }
    Some(Spec { counts, values })
}

/// Each symbol's code word and length under a table (length 0: no code).
pub fn codes(spec: &Spec) -> [(u16, u8); 256] {
    let mut codes = [(0u16, 0u8); 256];
    let mut code = 0u32;
    let mut k = 0;
    for len in 1..=16u8 {
        for _ in 0..spec.counts[len as usize - 1] {
            codes[spec.values[k] as usize] = (code as u16, len);
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    codes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    fn cost(freq: &[u64; 256], lens: &[u8; 256]) -> u64 {
        freq.iter().zip(lens.iter()).map(|(&f, &l)| f * l as u64).sum()
    }

    /// Plain Huffman cost (no length limit) of the used symbols plus a reserved word of weight 0.
    fn huffman_cost_reserved(freq: &[u64; 256]) -> u64 {
        let mut w: Vec<u64> = freq.iter().copied().filter(|&f| f > 0).collect();
        w.push(0);
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

    fn check_valid(freq: &[u64; 256], lens: &[u8; 256]) {
        for s in 0..256 {
            assert_eq!(freq[s] > 0, lens[s] > 0, "every used symbol and only those gets a code");
            assert!(lens[s] <= 16);
        }
        for order in [Order::Value, Order::Frequent, Order::FewestOnes] {
            let spec = table(lens, freq, order).expect("valid code");
            let codes = codes(&spec);
            // Every symbol keeps its length; no code is all ones; the table is accepted by the decoder side.
            for s in 0..256 {
                let (c, l) = codes[s];
                assert_eq!(l, lens[s]);
                if l > 0 {
                    assert_ne!(c as u32, (1u32 << l) - 1, "all-ones code");
                }
            }
            let t = DecodeTable::new(&spec, false).expect("decodable");
            for s in 0..256 {
                let (c, l) = codes[s];
                if l > 0 {
                    let peek = (c as u32) << (16 - l);
                    assert_eq!(t.lookup(peek), Some((l as u32, s as u8)));
                }
            }
        }
    }

    #[test]
    fn optimal_among_codes_with_a_free_word() {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        for round in 0..400 {
            let n = 1 + (rng(&mut s) % 256) as usize;
            let mut freq = [0u64; 256];
            for f in freq.iter_mut().take(n) {
                if !rng(&mut s).is_multiple_of(3) {
                    *f = 1 + rng(&mut s) % (1 + round as u64 * 37);
                }
            }
            if freq.iter().all(|&f| f == 0) {
                freq[0] = 5;
            }
            let lens = optimal_lengths(&freq);
            check_valid(&freq, &lens);
            if *lens.iter().max().unwrap() < 16 {
                // The 16-bit limit didn't bind: the cost must be plain Huffman's (with the reserved word).
                assert_eq!(cost(&freq, &lens), huffman_cost_reserved(&freq));
            }
        }
    }

    #[test]
    fn the_limit_binds_and_stays_valid() {
        // Fibonacci counts force codes far longer than 16 bits without a limit.
        let mut freq = [0u64; 256];
        let (mut a, mut b) = (1u64, 1u64);
        for f in freq.iter_mut().take(60) {
            *f = a;
            let c = a + b;
            a = b;
            b = c;
        }
        let lens = optimal_lengths(&freq);
        check_valid(&freq, &lens);
        assert_eq!(*lens.iter().max().unwrap(), 16);
        // Huge counts don't overflow.
        let freq = [u64::MAX / 1024; 256];
        let lens = optimal_lengths(&freq);
        check_valid(&freq, &lens);
    }

    #[test]
    fn brute_force_on_small_cases() {
        // All length vectors up to 4 bits for 4 symbols, keeping a free word: the best cost must match.
        let mut s = 7u64;
        for _ in 0..200 {
            let mut freq = [0u64; 256];
            for f in freq.iter_mut().take(4) {
                *f = 1 + rng(&mut s) % 50;
            }
            let lens = optimal_lengths(&freq);
            let mut best = u64::MAX;
            for code in 0..(16u32 * 16 * 16 * 16) {
                let l = [code & 15, code >> 4 & 15, code >> 8 & 15, code >> 12 & 15];
                if l.contains(&0) {
                    continue;
                }
                let kraft: u64 = l.iter().map(|&x| 1u64 << (16 - x)).sum();
                if kraft < 1 << 16 {
                    best = best.min((0..4).map(|i| freq[i] * l[i] as u64).sum());
                }
            }
            assert_eq!(cost(&freq, &lens), best);
        }
    }

    #[test]
    fn one_symbol_gets_one_bit() {
        let mut freq = [0u64; 256];
        freq[0x37] = 1000;
        let lens = optimal_lengths(&freq);
        assert_eq!(lens[0x37], 1);
        let spec = table(&lens, &freq, Order::Value).unwrap();
        assert_eq!(codes(&spec)[0x37], (0, 1));
        assert_eq!(spec.counts[0], 1);
    }

    #[test]
    fn rejects_bad_tables() {
        // Two 1-bit codes: the second is all ones.
        let mut counts = [0u8; 16];
        counts[0] = 2;
        assert!(DecodeTable::new(&Spec { counts, values: vec![0, 1] }, false).is_err());
        // Overfull.
        let mut counts = [0u8; 16];
        counts[1] = 5;
        assert!(DecodeTable::new(&Spec { counts, values: vec![0, 1, 2, 3, 4] }, false).is_err());
        // Count and values disagree.
        let mut counts = [0u8; 16];
        counts[1] = 2;
        assert!(DecodeTable::new(&Spec { counts, values: vec![0] }, false).is_err());
        // DC symbol above 15.
        let mut counts = [0u8; 16];
        counts[1] = 1;
        assert!(DecodeTable::new(&Spec { counts, values: vec![16] }, true).is_err());
        assert!(DecodeTable::new(&Spec { counts, values: vec![16] }, false).is_ok());
    }
}
