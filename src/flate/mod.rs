//! Flate (Deflate + zlib wrapper): the decoder and the encoders. Parts follow zlib, libdeflate and the LZMA
//! SDK, as each file says.

pub mod bits;
pub mod block;
pub mod deflate;
pub mod fast;
pub mod huffman;
pub mod inflate;
pub mod lz;
pub mod matcher;
pub mod parse;
pub mod rules;
pub mod tables;
pub mod zlib;
// The exact re-creation of zlib's own streams, which only the archive uses.
#[cfg(feature = "archive")]
pub mod zlibexact;

pub use deflate::{deflate, deflate_zlib, Options};
pub use inflate::{inflate, InflateError};
pub use rules::FlateRules;

#[cfg(test)]
pub(crate) mod poppler {
    use super::{huffman, tables};

    /// Raw Deflate data read as Poppler 24.02's FlateStream reads it: before each Huffman code it fetches whole
    /// bytes until it holds the table's longest code length, and a stored block's length comes straight from the
    /// input, after the bits it holds are dropped. Also the number of empty fixed blocks just before a stored
    /// one. None where Poppler would fail or read other data.
    pub fn read(src: &[u8]) -> Option<(Vec<u8>, usize)> {
        struct R<'a> {
            src: &'a [u8],
            pos: usize,
            buf: u64,
            size: u32,
        }
        impl R<'_> {
            fn byte(&mut self) -> Option<u8> {
                let b = *self.src.get(self.pos)?;
                self.pos += 1;
                Some(b)
            }
            fn bits(&mut self, n: u32) -> Option<u32> {
                while self.size < n {
                    let c = self.byte()?;
                    self.buf |= (c as u64) << self.size;
                    self.size += 8;
                }
                let v = (self.buf & ((1 << n) - 1)) as u32;
                self.buf >>= n;
                self.size -= n;
                Some(v)
            }
            fn code(&mut self, t: &Table) -> Option<usize> {
                let longest = t.0;
                while self.size < longest {
                    match self.byte() {
                        Some(c) => {
                            self.buf |= (c as u64) << self.size;
                            self.size += 8;
                        }
                        None => break,
                    }
                }
                // `canonical_codes` gives the codes bit-reversed, as they lie in the input.
                for len in 1..=longest.min(self.size) {
                    if let Some(&sym) = t.1.get(&(len, (self.buf & ((1 << len) - 1)) as u32)) {
                        self.buf >>= len;
                        self.size -= len;
                        return Some(sym);
                    }
                }
                None
            }
        }
        /// The longest code length, and each symbol by its (length, code).
        struct Table(u32, std::collections::HashMap<(u32, u32), usize>);
        fn table(lengths: &[u8]) -> Table {
            let codes = huffman::canonical_codes(lengths);
            let map = (0..lengths.len())
                .filter(|&s| lengths[s] > 0)
                .map(|s| ((lengths[s] as u32, codes[s] as u32), s))
                .collect();
            Table(lengths.iter().copied().max().unwrap_or(0) as u32, map)
        }
        let mut r = R { src, pos: 0, buf: 0, size: 0 };
        let (mut out, mut empty_fixed, mut run) = (Vec::new(), 0, 0);
        loop {
            let last = r.bits(1)?;
            let kind = r.bits(2)?;
            let (ll, d) = match kind {
                0 => {
                    let mut len = [0u16; 2];
                    for v in &mut len {
                        *v = r.byte()? as u16 | (r.byte()? as u16) << 8;
                    }
                    if len[1] != !len[0] {
                        return None;
                    }
                    r.buf = 0;
                    r.size = 0;
                    for _ in 0..len[0] {
                        out.push(r.byte()?);
                    }
                    empty_fixed += run;
                    run = 0;
                    if last == 1 {
                        return Some((out, empty_fixed));
                    }
                    continue;
                }
                1 => (table(&tables::fixed_litlen_lengths()), table(&tables::fixed_dist_lengths())),
                2 => {
                    let (hlit, hdist, hclen) = (r.bits(5)? as usize + 257, r.bits(5)? as usize + 1, r.bits(4)? + 4);
                    let mut cl = [0u8; 19];
                    for &s in tables::CL_ORDER.iter().take(hclen as usize) {
                        cl[s] = r.bits(3)? as u8;
                    }
                    let cl = table(&cl);
                    let mut l = Vec::new();
                    while l.len() < hlit + hdist {
                        match r.code(&cl)? {
                            s @ 0..=15 => l.push(s as u8),
                            16 => l.extend(std::iter::repeat_n(*l.last()?, 3 + r.bits(2)? as usize)),
                            17 => l.extend(std::iter::repeat_n(0, 3 + r.bits(3)? as usize)),
                            _ => l.extend(std::iter::repeat_n(0, 11 + r.bits(7)? as usize)),
                        }
                    }
                    (table(&l[..hlit]), table(&l[hlit..hlit + hdist]))
                }
                _ => return None,
            };
            let start = out.len();
            loop {
                match r.code(&ll)? {
                    s @ 0..=255 => out.push(s as u8),
                    256 => break,
                    s => {
                        let c = s - 257;
                        let len = tables::LEN_BASE[c] as usize + r.bits(tables::LEN_EXTRA[c] as u32)? as usize;
                        let k = r.code(&d)?;
                        let dist = tables::DIST_BASE[k] as usize + r.bits(tables::DIST_EXTRA[k] as u32)? as usize;
                        for _ in 0..len {
                            out.push(out[out.len().checked_sub(dist)?]);
                        }
                    }
                }
            }
            run = if out.len() == start && kind == 1 { run + 1 } else { 0 };
            if last == 1 {
                return Some((out, empty_fixed));
            }
        }
    }
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

    fn samples() -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = vec![
            vec![],
            vec![0],
            vec![1, 2],
            b"abc".to_vec(),
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec(),
            vec![0u8; 100_000],
        ];
        let text = b"BT /F1 12 Tf 72 712 Td [(Hello)-250(World)]TJ ET q 0.5 0 0 0.5 0 0 cm /Im1 Do Q\n".repeat(400);
        out.push(text);
        let mut s = 99u64;
        out.push((0..70_000).map(|_| rng(&mut s) as u8).collect());
        let mut mixed = Vec::new();
        for k in 0..200_000usize {
            let v = rng(&mut s);
            mixed.push(if k % 1000 < 500 { (v % 3) as u8 + b'a' } else { (v >> 8) as u8 });
        }
        out.push(mixed);
        out
    }

    #[test]
    fn round_trips_with_our_decoder() {
        for opt in [
            Options::quick(),
            Options {
                iterations: 4,
                exact_rounds: 3,
                max_chain: 512,
                segment: 50_000,
                split: true,
                alternatives: true,
                multi_start_limit: 1 << 18,
                nice: 259,
                polish: true,
                shared: None,
                deadline: crate::clock::Deadline::NONE,
                fast: 0,
                rules: FlateRules::default(),
            },
        ] {
            for d in samples() {
                let z = deflate(&d, &opt);
                let back = inflate(&z, 1 << 30).expect("decodes");
                assert_eq!(back.data, d, "length {}", d.len());
                assert_eq!(back.consumed, z.len());
            }
        }
    }

    #[test]
    fn poppler_reads_what_the_encoders_write() {
        // Text and random bytes in turn, so that stored blocks follow Huffman-coded ones. (Their end-of-block
        // codes are long, so none of these needs the empty fixed block; the block and fast tests make one.)
        let mut s = 7u64;
        let text = b"BT /F1 12 Tf 72 712 Td [(Hello)-250(World)]TJ ET q 0.5 0 0 0.5 0 0 cm /Im1 Do Q\n";
        for n in 0..40 {
            let mut d = Vec::new();
            while d.len() < 30_000 {
                let len = 200 + (rng(&mut s) % 3000) as usize;
                if rng(&mut s).is_multiple_of(2) {
                    d.extend(text.iter().cycle().take(len));
                } else {
                    d.extend((0..len).map(|_| (rng(&mut s) >> 11) as u8));
                }
            }
            for effort in [0, 20, 40] {
                let z = deflate(&d, &Options::effort(effort));
                let (back, _) = poppler::read(&z).unwrap_or_else(|| panic!("sample {n}, effort {effort}"));
                assert!(back == d, "sample {n}, effort {effort}: other data");
            }
        }
    }

    #[test]
    fn zlib_wrapper_round_trips() {
        for d in samples() {
            let z = deflate_zlib(&d, &Options::quick());
            let u = zlib::unwrap(&z, 1 << 30).expect("decodes");
            assert_eq!(u.data, d);
            assert_eq!(u.trailing, 0);
        }
    }
}

#[cfg(test)]
mod threads {
    use super::deflate::Shared;
    use super::*;
    use crate::par::{self, Pool};
    use std::time::Duration;

    /// The default of the rule `remember_up_to`.
    const REMEMBER_UP_TO: usize = FlateRules::RULES[FlateRules::at("remember_up_to")].1 as usize;

    #[test]
    fn segments_in_parallel_give_the_same_bytes() {
        // Many small segments, so that several threads really share the work.
        let mut s = 5u64;
        let mut data = Vec::new();
        for k in 0..400_000usize {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            data.push(if k % 3000 < 1800 { b"stream of text "[k % 15] } else { (s >> 7) as u8 % 7 });
        }
        for base in [Options::quick(), Options::effort(50)] {
            let opt = Options { segment: 40_000, ..base };
            let one = deflate_zlib(&data, &opt);
            assert_eq!(zlib::unwrap(&one, data.len() + 1).unwrap().data, data);
            for t in [2, 3, 8] {
                let o = Options { shared: Some(deflate::Shared::new(t)), ..opt.clone() };
                assert!(deflate_zlib(&data, &o) == one, "{t} threads");
                // 400,000 bytes is over REMEMBER_UP_TO, so a second encode of the same input is done again.
                assert!(deflate_zlib(&data, &o) == one);
            }
        }
    }

    /// Two inputs of the same length and the same 64-bit hash, so the same memo key. Each 8-byte word goes
    /// into the hash as h = ((h ^ word) * M) rotated left by 29: after a different first word, a second word
    /// with the right difference brings the hash back to the same state.
    fn colliding_pair() -> (Vec<u8>, Vec<u8>) {
        const M: u64 = 0x9E37_79B9_7F4A_7C15;
        let step = |h: u64, w: u64| (h ^ w).wrapping_mul(M).rotate_left(29);
        let word = |d: &[u8], k: usize| u64::from_le_bytes(d[8 * k..8 * k + 8].try_into().unwrap());
        let a = b"BT /F1 12 Tf 72 712 Td [(Hello)-250(World)]TJ ET\n".repeat(300);
        let mut b = a.clone();
        b[..8].copy_from_slice(b"q 1 0 0 ");
        let h0 = (a.len() as u64).wrapping_mul(M) ^ 0x2545_F491_4F6C_DD1D;
        let second = word(&a, 1) ^ step(h0, word(&a, 0)) ^ step(h0, word(&b, 0));
        b[8..16].copy_from_slice(&second.to_le_bytes());
        assert!(a != b && crate::par::hash64(&a) == crate::par::hash64(&b), "the planted collision");
        (a, b)
    }

    #[test]
    fn a_remembered_output_is_never_used_for_another_input() {
        let (a, b) = colliding_pair();
        for t in [1, 2] {
            let o = Options { shared: Some(deflate::Shared::new(t)), ..Options::quick() };
            let za = deflate_zlib(&a, &o);
            assert_eq!(zlib::unwrap(&za, a.len() + 1).unwrap().data, a);
            // The output remembered for `a` fails the check, so `b` is encoded afresh.
            let zb = deflate_zlib(&b, &o);
            assert_eq!(zlib::unwrap(&zb, b.len() + 1).unwrap().data, b, "{t} threads");
            assert!(zb == deflate_zlib(&b, &Options::quick()));
        }
    }

    #[test]
    fn a_stored_output_is_never_used_for_another_input() {
        let (a, b) = colliding_pair();
        for t in [1, 2] {
            let store = Shared::with_store(Pool::new(t), 1 << 20);
            let o = Options { shared: Some(store.clone()), ..Options::quick() };
            let za = deflate_zlib(&a, &o);
            assert_eq!(zlib::unwrap(&za, a.len() + 1).unwrap().data, a);
            // The output kept for `a` fails the check, so `b` is encoded afresh.
            let zb = deflate_zlib(&b, &o);
            assert!(zb == deflate_zlib(&b, &Options::quick()), "{t} threads");
            let st = store.stats();
            assert_eq!((st.hits, st.computed, st.kept), (0, 2, 1), "{t} threads");
            // `a` itself is still given from the store, but not for other settings.
            assert!(deflate_zlib(&a, &o) == za);
            assert_eq!(store.stats().hits, 1);
            let other = Options { shared: Some(store.clone()), ..Options::effort(60) };
            assert!(deflate_zlib(&a, &other) == deflate_zlib(&a, &Options::effort(60)), "{t} threads");
            assert_eq!(store.stats().hits, 1);
        }
    }

    #[test]
    fn an_output_made_after_the_deadline_is_not_kept() {
        let data = b"BT /F1 12 Tf 72 712 Td [(Hello)-250(World)]TJ ET\n".repeat(300);
        let fresh = deflate_zlib(&data, &Options::quick());
        let store = Shared::with_store(Pool::new(2), 1 << 20);
        let late = Options {
            shared: Some(store.clone()),
            deadline: crate::clock::Deadline::after(Duration::ZERO),
            ..Options::quick()
        };
        // Past the deadline the segments go out stored: a good output, but not the encoder's best.
        let stored = deflate_zlib(&data, &late);
        assert_eq!(zlib::unwrap(&stored, data.len() + 1).unwrap().data, data);
        assert!(stored != fresh && stored.len() > data.len());
        let o = Options { shared: Some(store.clone()), ..Options::quick() };
        assert!(deflate_zlib(&data, &o) == fresh);
        assert!(deflate_zlib(&data, &o) == fresh);
        let st = store.stats();
        assert_eq!((st.hits, st.computed, st.kept), (1, 2, 1));
    }

    #[test]
    fn a_store_keeps_its_budget() {
        let input = |i: usize| -> Vec<u8> { (0..1024).map(|k| ((k * (i + 3)) % 251) as u8 ^ (i as u8)).collect() };
        let budget = 16 << 10;
        let store = Shared::with_store(Pool::new(4), budget);
        let o = Options { shared: Some(store.clone()), ..Options::quick() };
        let pool = Pool::new(4);
        let outs = par::map(Some(&pool), 200, None, |i| deflate_zlib(&input(i), &o));
        for (i, z) in outs.iter().enumerate() {
            assert_eq!(zlib::unwrap(z, 1025).unwrap().data, input(i), "input {i}");
        }
        let st = store.stats();
        assert_eq!((st.hits, st.computed), (0, 200));
        assert!(st.peak <= budget && st.kept >= 1, "{st:?}");
        assert!(st.kept <= budget / (par::ENTRY_COST + 1), "{st:?}");
        // Asked again, every input gives the same bytes, from the store or not.
        for (i, z) in outs.iter().enumerate() {
            assert!(deflate_zlib(&input(i), &o) == *z, "input {i}");
        }
        assert!(store.stats().peak <= budget);
    }

    #[test]
    fn one_compute_for_many_askers() {
        let data = b"q 1 0 0 1 72 720 cm /Im1 Do Q\n".repeat(2000);
        let fresh = deflate_zlib(&data, &Options::quick());
        let pool = Pool::new(4);
        let store = Shared::with_store(pool.clone(), 1 << 20);
        let o = Options { shared: Some(store.clone()), ..Options::quick() };
        let outs = par::map(Some(&pool), 8, None, |_| deflate_zlib(&data, &o));
        assert!(outs.iter().all(|z| *z == fresh));
        let st = store.stats();
        assert_eq!((st.hits, st.computed, st.kept), (7, 1, 1));
        assert_eq!(st.peak, fresh.len() + par::ENTRY_COST);
        // Each hit counts the time the encode took.
        assert!(st.saved > Duration::ZERO);
    }

    #[test]
    fn big_inputs_skip_the_store() {
        let mut s = 11u64;
        let noise: Vec<u8> = (0..REMEMBER_UP_TO + 1)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s % 7) as u8
            })
            .collect();
        let store = Shared::with_store(Pool::new(2), 4 << 20);
        let o = Options { shared: Some(store.clone()), ..Options::quick() };
        for _ in 0..2 {
            assert!(deflate_zlib(&noise, &o) == deflate_zlib(&noise, &Options::quick()));
        }
        assert_eq!(store.stats(), deflate::StoreStats::default());
        // An input of REMEMBER_UP_TO bytes is remembered, unless it is more than one segment.
        let last = &noise[..REMEMBER_UP_TO];
        assert!(deflate_zlib(last, &o) == deflate_zlib(last, &o));
        assert_eq!((store.stats().hits, store.stats().computed), (1, 1));
        let o = Options { segment: 100_000, ..o };
        assert!(deflate_zlib(last, &o) == deflate_zlib(last, &o));
        assert_eq!((store.stats().hits, store.stats().computed), (1, 1));
        // No store at all with a budget of 0.
        let none = Shared::with_store(Pool::new(2), 0);
        let o = Options { shared: Some(none.clone()), ..Options::quick() };
        let small = &noise[..1000];
        assert!(deflate_zlib(small, &o) == deflate_zlib(small, &o));
        assert_eq!(none.stats(), deflate::StoreStats::default());
    }

    #[test]
    fn without_the_store_nothing_is_kept_and_segments_still_count() {
        // Three segments of zeros: the third is like the second (the first has no history).
        let opt = Options { segment: 40_000, ..Options::quick() };
        let data = vec![0u8; 3 * 40_000];
        let store = Shared::with_store(Pool::new(2), 1 << 20);
        let sibling = store.without_store();
        assert!(std::sync::Arc::ptr_eq(&sibling.pool, &store.pool));
        let o = Options { shared: Some(sibling.clone()), ..opt.clone() };
        assert!(deflate_zlib(&data, &o) == deflate_zlib(&data, &opt));
        let small = b"q 1 0 0 1 72 720 cm /Im1 Do Q\n".repeat(30);
        assert!(deflate_zlib(&small, &o) == deflate_zlib(&small, &o));
        let st = store.stats();
        assert_eq!((st.hits, st.computed, st.kept, st.reused_segments), (0, 0, 0, 1));
        assert_eq!(sibling.stats(), st);
    }
}
