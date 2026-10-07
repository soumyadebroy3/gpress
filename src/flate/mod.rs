//! Flate (Deflate + zlib wrapper): our own decoder and encoder.

pub mod bits;
pub mod block;
pub mod deflate;
pub mod huffman;
pub mod inflate;
pub mod lz;
pub mod matcher;
pub mod parse;
pub mod tables;
pub mod zlib;
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
