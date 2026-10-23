//! Flate (Deflate + zlib wrapper): the decoder and the encoders. Parts follow zlib and libdeflate, as each
//! file says.

pub mod bits;
pub mod block;
pub mod deflate;
pub mod fast;
pub mod huffman;
pub mod inflate;
pub mod lz;
pub mod matcher;
pub mod parse;
pub mod tables;
pub mod zlib;
// The exact re-creation of zlib's own streams, which only the archive uses.
#[cfg(feature = "archive")]
pub mod zlibexact;

pub use deflate::{deflate, deflate_zlib, Options};
pub use inflate::{inflate, InflateError};

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
        let noise: Vec<u8> = (0..deflate::REMEMBER_UP_TO + 1)
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
        let last = &noise[..deflate::REMEMBER_UP_TO];
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
