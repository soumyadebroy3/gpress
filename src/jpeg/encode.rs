//! A baseline JPEG encoder for the lossy image step: a grey picture, or a YCbCr one with the colour planes at
//! half size each way (4:2:0), as a JFIF file every reader opens. The quantisation tables are the standard's
//! example tables scaled for a quality the way libjpeg scales them. The Huffman tables are the standard's
//! example tables too: `optimize` makes optimal ones of the result afterwards.

use super::decode::{fdct, ZIGZAG};
use super::huffman::{codes, Spec};
use super::scan::BitWriter;
use super::{Plane, VecSink};
use crate::clock::Deadline;

/// The example quantisation tables of the JPEG standard (Annex K, tables K.1 and K.2), row by row: luminance,
/// chrominance.
const QUANT: [[u8; 64]; 2] = [
    [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29,
        51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121,
        120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ],
    [
        17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99,
        99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
        99, 99, 99, 99, 99, 99, 99, 99,
    ],
];

/// The example Huffman tables of the JPEG standard (Annex K, tables K.3 to K.6), each as the number of codes
/// of every length and the symbols in code order: DC luminance, AC luminance, DC chrominance, AC chrominance.
const HUFFMAN: [([u8; 16], &[u8]); 4] = [
    ([0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
    (
        [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
        &[
            0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71,
            0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72,
            0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
            0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
            0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3,
            0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
            0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
            0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
        ],
    ),
    ([0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
    (
        [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
        &[
            0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22,
            0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1,
            0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36,
            0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
            0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
            0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a,
            0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba,
            0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
            0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
        ],
    ),
];

/// A quantisation table for `quality` (libjpeg's scale, 1 to 100) from one of the standard's.
fn quant(base: &[u8; 64], quality: u8) -> [u8; 64] {
    let q = quality.clamp(1, 100) as u32;
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    base.map(|b| ((b as u32 * scale + 50) / 100).clamp(1, 255) as u8)
}

/// The picture as a baseline JFIF file at `quality` (libjpeg's scale, 1 to 100). `planes` is the luma alone
/// (grey), or luma, Cb and Cr with the two colour planes (w + 1) / 2 by (h + 1) / 2 samples each. None for any
/// other shape, a side of 0 or over 65535, and when `deadline` passes.
pub fn encode(planes: &[Plane], quality: u8, deadline: Deadline) -> Option<Vec<u8>> {
    let (n, y) = (planes.len(), planes.first()?);
    if (n != 1 && n != 3) || !(1..=65535).contains(&y.w) || !(1..=65535).contains(&y.h) {
        return None;
    }
    let (cw, ch) = (y.w.div_ceil(2), y.h.div_ceil(2));
    if y.px.len() != y.w * y.h || planes[1..].iter().any(|p| (p.w, p.h, p.px.len()) != (cw, ch, cw * ch)) {
        return None;
    }
    // Two kinds of component in colour, luma and colour: each has its tables, and an MCU has as many luma
    // blocks each way.
    let kinds = n.min(2);
    let quant = [quant(&QUANT[0], quality), quant(&QUANT[1], quality)];
    // The start marker and the JFIF segment: version 1.01, no unit, square pixels, no thumbnail.
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0];
    out.extend_from_slice(&[0xFF, 0xDB, 0, 2 + 65 * kinds as u8]);
    for (i, t) in quant.iter().enumerate().take(kinds) {
        out.push(i as u8);
        out.extend(ZIGZAG.iter().map(|&z| t[z as usize]));
    }
    out.extend_from_slice(&[0xFF, 0xC0, 0, 8 + 3 * n as u8, 8]);
    out.extend_from_slice(&(y.h as u16).to_be_bytes());
    out.extend_from_slice(&(y.w as u16).to_be_bytes());
    out.push(n as u8);
    for c in 0..n as u8 {
        // Id, sampling, quantisation table.
        out.extend_from_slice(&[c + 1, if c == 0 { 0x11 * kinds as u8 } else { 0x11 }, c.min(1)]);
    }
    let specs: Vec<Spec> =
        HUFFMAN.iter().take(2 * kinds).map(|&(counts, values)| Spec { counts, values: values.to_vec() }).collect();
    let len = 2 + specs.iter().map(|t| 17 + t.values.len()).sum::<usize>();
    out.extend_from_slice(&[0xFF, 0xC4, (len >> 8) as u8, len as u8]);
    for (i, t) in specs.iter().enumerate() {
        out.push(((i & 1) << 4 | i >> 1) as u8);
        out.extend_from_slice(&t.counts);
        out.extend_from_slice(&t.values);
    }
    out.extend_from_slice(&[0xFF, 0xDA, 0, 6 + 2 * n as u8, n as u8]);
    for c in 0..n as u8 {
        // Id, DC and AC table.
        out.extend_from_slice(&[c + 1, 0x11 * c.min(1)]);
    }
    out.extend_from_slice(&[0, 63, 0]);

    let code: Vec<[(u16, u8); 256]> = specs.iter().map(codes).collect();
    let mut w = BitWriter::new(VecSink { v: &mut out, ff: 0 });
    // The last DC coefficient of each component.
    let mut pred = [0i32; 3];
    let mut block = |c: usize, bx: usize, by: usize| {
        let (p, kind) = (&planes[c], c.min(1));
        // The samples less 128; past the right or bottom edge, the last column or row again.
        let mut b = [0i64; 64];
        for (i, v) in b.iter_mut().enumerate() {
            let (col, row) = ((bx * 8 + i % 8).min(p.w - 1), (by * 8 + i / 8).min(p.h - 1));
            *v = p.px[row * p.w + col] as i64 - 128;
        }
        fdct(&mut b);
        // Coefficient k of the zigzag order: divided by 2^17 times its table value, to the nearest.
        let coef = |k: usize| {
            let z = ZIGZAG[k] as usize;
            let (v, d) = (b[z] as i32, (quant[kind][z] as i32) << 17);
            (v.abs() + d / 2) / d * v.signum()
        };
        // A symbol's code word, then as many low bits of `v` (of `v - 1` when negative) as its size says.
        let mut put = |table: usize, sym: usize, v: i32| {
            let ((word, len), size) = (code[table][sym], sym as u32 & 15);
            w.put((word as u32) << size | ((v - (v < 0) as i32) as u32 & ((1 << size) - 1)), len as u32 + size);
        };
        let size = |v: i32| (32 - v.unsigned_abs().leading_zeros()) as usize;
        // The DC coefficient as its difference from the last one, which then fits 11 bits.
        let dc = coef(0).clamp(-1024, 1023);
        put(2 * kind, size(dc - pred[c]), dc - pred[c]);
        pred[c] = dc;
        let mut run = 0;
        for k in 1..64 {
            let v = coef(k).clamp(-1023, 1023);
            if v == 0 {
                run += 1;
                continue;
            }
            for _ in 0..run / 16 {
                put(2 * kind + 1, 0xF0, 0);
            }
            put(2 * kind + 1, (run % 16) << 4 | size(v), v);
            run = 0;
        }
        if run > 0 {
            put(2 * kind + 1, 0x00, 0);
        }
    };
    for my in 0..y.h.div_ceil(8 * kinds) {
        if deadline.passed() {
            return None;
        }
        for mx in 0..y.w.div_ceil(8 * kinds) {
            for i in 0..kinds * kinds {
                block(0, kinds * mx + i % kinds, kinds * my + i / kinds);
            }
            for c in 1..n {
                block(c, mx, my);
            }
        }
    }
    w.pad();
    out.extend_from_slice(&[0xFF, 0xD9]);
    Some(out)
}

/// Interleaved 8-bit RGB (`w` by `h` pixels) as Y, Cb and Cr planes by JFIF's formulas in 16-bit fixed point,
/// the colour planes at half size each way: each of their samples is the average, to the nearest, over the
/// pixels the picture has in its 2 by 2 square.
pub fn ycc_planes(rgb: &[u8], w: usize, h: usize) -> Option<Vec<Plane>> {
    if w == 0 || h == 0 || w.checked_mul(h)?.checked_mul(3)? != rgb.len() {
        return None;
    }
    // The weights of Y add up to 65536 and those of Cb and of Cr to 0, so every result is within 0..=255.
    let luma = |p: &[u8; 3]| ((19595 * p[0] as u32 + 38470 * p[1] as u32 + 7471 * p[2] as u32 + 32768) >> 16) as u8;
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (mut cb, mut cr) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for cy in 0..ch {
        for cx in 0..cw {
            let (mut b, mut r, mut count) = (0, 0, 0);
            for y in 2 * cy..(2 * cy + 2).min(h) {
                for x in 2 * cx..(2 * cx + 2).min(w) {
                    let p = &rgb[(y * w + x) * 3..];
                    let (pr, pg, pb) = (p[0] as i32, p[1] as i32, p[2] as i32);
                    b += (-11059 * pr - 21709 * pg + 32768 * pb + 8421375) >> 16;
                    r += (32768 * pr - 27439 * pg - 5329 * pb + 8421375) >> 16;
                    count += 1;
                }
            }
            cb.push(((b + count / 2) / count) as u8);
            cr.push(((r + count / 2) / count) as u8);
        }
    }
    let px = rgb.as_chunks::<3>().0.iter().map(luma).collect();
    Some(vec![Plane { w, h, px }, Plane { w: cw, h: ch, px: cb }, Plane { w: cw, h: ch, px: cr }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jpeg::huffman::DecodeTable;
    use crate::jpeg::parse::parse;
    use crate::jpeg::{decode, optimize, Refused};
    use std::time::Duration;

    const NONE: Deadline = Deadline::NONE;
    const GRAY: &[u8] = include_bytes!("../../tests/data/jpeg/gray.jpg");
    const BASE420: &[u8] = include_bytes!("../../tests/data/jpeg/base420.jpg");

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// The sum of the squared differences of two planes' samples.
    fn sse(a: &Plane, b: &Plane) -> u64 {
        assert_eq!((a.w, a.h, a.px.len()), (b.w, b.h, b.px.len()));
        a.px.iter().zip(&b.px).map(|(&x, &y)| (x.abs_diff(y) as u64).pow(2)).sum()
    }

    /// The picture tests/data/jpeg/make.py makes the test files from, as RGB.
    fn picture(w: usize, h: usize) -> Vec<u8> {
        let mut px = Vec::new();
        let mut s = 12345u64;
        for y in 0..h {
            for x in 0..w {
                s = (s * 1103515245 + 12345) & 0x7fff_ffff;
                let n = (s >> 16) as usize % 23;
                let edge = if (x / 9 + y / 7).is_multiple_of(3) { 90 } else { 0 };
                px.push((x * 255 / (w - 1).max(1) + n + edge) as u8);
                px.push((y * 255 / (h - 1).max(1) + 2 * n) as u8);
                px.push(((x + y) * 3 + edge + n) as u8);
            }
        }
        px
    }

    #[test]
    fn quality_scales_the_tables() {
        assert_eq!(quant(&QUANT[0], 50), QUANT[0]);
        assert_eq!(quant(&QUANT[1], 50), QUANT[1]);
        assert_eq!(quant(&QUANT[0], 75)[0], 8);
        assert_eq!(quant(&QUANT[0], 85)[0], 5);
        assert_eq!(quant(&QUANT[0], 100), [1; 64]);
        assert_eq!(quant(&QUANT[0], 200), [1; 64]);
        assert_eq!(quant(&QUANT[1], 1), [255; 64]);
        assert_eq!(quant(&QUANT[1], 0), [255; 64]);
        // The tables cjpeg wrote, in zigzag order: at quality 85 in gray.jpg, at 80 in base420.jpg.
        let zigzag = |base: &[u8; 64], quality: u8| ZIGZAG.map(|z| quant(base, quality)[z as usize]);
        assert_eq!(GRAY[25..89], zigzag(&QUANT[0], 85));
        assert_eq!(BASE420[25..89], zigzag(&QUANT[0], 80));
        assert_eq!(BASE420[94..158], zigzag(&QUANT[1], 80));
    }

    #[test]
    fn the_standard_huffman_tables() {
        for (i, &(counts, values)) in HUFFMAN.iter().enumerate() {
            let ac = i % 2 == 1;
            assert_eq!(counts.iter().map(|&c| c as usize).sum::<usize>(), values.len(), "table {i}");
            let spec = Spec { counts, values: values.to_vec() };
            assert!(DecodeTable::new(&spec, !ac).is_ok(), "table {i}");
            // Every symbol a block can need has a code: the sizes 0 to 11 of a DC difference; the end of a
            // block, 16 zeros, and every run of 0 to 15 zeros before a value of size 1 to 10.
            let code = codes(&spec);
            let needed: Vec<usize> = if ac {
                (0..256).filter(|&s| s == 0x00 || s == 0xF0 || (1..=10).contains(&(s & 15))).collect()
            } else {
                (0..12).collect()
            };
            assert_eq!(needed.len(), if ac { 162 } else { 12 });
            assert_eq!(values.len(), needed.len(), "table {i}");
            assert!(needed.iter().all(|&s| code[s].1 > 0), "table {i}");
            // cjpeg writes these tables when it is not asked for optimal ones.
            let segment = [&[(i % 2 * 16 + i / 2) as u8][..], &counts, values].concat();
            assert!(BASE420.windows(segment.len()).any(|w| w == segment), "table {i}");
        }
    }

    /// A smooth picture with a little noise: the luma, and colour planes of half the size when asked.
    fn synthetic(w: usize, h: usize, colour: bool) -> Vec<Plane> {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut plane = |w: usize, h: usize, level: usize, dx: usize, dy: usize| {
            let px = (0..w * h).map(|i| (level + dx * (i % w) + dy * (i / w) + (rng(&mut s) % 4) as usize) as u8);
            Plane { w, h, px: px.collect() }
        };
        let mut planes = vec![plane(w, h, 20, 2, 2)];
        if colour {
            planes.push(plane(w.div_ceil(2), h.div_ceil(2), 60, 2, 0));
            planes.push(plane(w.div_ceil(2), h.div_ceil(2), 150, 0, 3));
        }
        planes
    }

    #[test]
    fn encoded_pictures_decode_close_to_themselves() {
        for (w, h) in [(64, 48), (37, 21), (1, 1), (16, 8), (17, 9)] {
            for colour in [false, true] {
                let planes = synthetic(w, h, colour);
                let file = encode(&planes, 90, NONE).unwrap_or_else(|| panic!("{w}x{h} {colour}"));
                assert!(file.starts_with(&[0xFF, 0xD8, 0xFF, 0xE0]) && file.ends_with(&[0xFF, 0xD9]));
                assert!(parse(&file).is_ok(), "{w}x{h} {colour}");
                let back = decode(&file, 1 << 20, NONE).unwrap_or_else(|e| panic!("{w}x{h} {colour}: {e}"));
                assert_eq!((back.width, back.height, back.planes.len()), (w, h, planes.len()));
                // Luma at 35 dB or better (a mean squared error of 20.56), the colour planes in their places.
                let luma = sse(&back.planes[0], &planes[0]);
                assert!(luma * 100 <= 2056 * (w * h) as u64, "{w}x{h} {colour}: luma {luma}");
                for i in 1..planes.len() {
                    let e = sse(&back.planes[i], &planes[i]);
                    assert!(e <= 100 * planes[i].px.len() as u64, "{w}x{h} plane {i}: {e}");
                }
                // The Huffman step takes the file: optimal tables, the same samples.
                match optimize(&file) {
                    Ok(o) => assert_eq!(decode(&o, 1 << 20, NONE).as_ref(), Ok(&back)),
                    Err(e) => assert_eq!(e, Refused("no saving")),
                }
            }
        }
        // Every quality gives a file that reads back, noise and flat extremes included.
        let mut s = 7u64;
        let noise = Plane { w: 24, h: 24, px: (0..576).map(|_| rng(&mut s) as u8).collect() };
        let ends = Plane { w: 24, h: 24, px: (0..576).map(|i| if i % 48 < 24 { 0 } else { 255 }).collect() };
        for quality in [0, 1, 25, 50, 75, 100, 255] {
            for p in [&noise, &ends] {
                let grey = encode(std::slice::from_ref(p), quality, NONE).unwrap();
                assert_eq!(decode(&grey, 1 << 20, NONE).map(|b| (b.width, b.height)), Ok((24, 24)));
                let small = Plane { w: 12, h: 12, px: p.px[..144].to_vec() };
                let colour = encode(&[p.clone(), small.clone(), small], quality, NONE).unwrap();
                assert_eq!(decode(&colour, 1 << 20, NONE).map(|b| b.planes.len()), Ok(3));
            }
        }
        // At quality 100 every divisor is 1: only rounding is left, a mean squared error far below 1.
        let exact = decode(&encode(std::slice::from_ref(&noise), 100, NONE).unwrap(), 1 << 20, NONE).unwrap();
        assert!(sse(&exact.planes[0], &noise) <= 576);
    }

    #[test]
    fn other_shapes_are_refused() {
        let plane = |w: usize, h: usize| Plane { w, h, px: vec![128; w * h] };
        assert!(encode(&[plane(9, 7)], 80, NONE).is_some());
        assert!(encode(&[plane(9, 7), plane(5, 4), plane(5, 4)], 80, NONE).is_some());
        assert!(encode(&[], 80, NONE).is_none());
        assert!(encode(&[plane(9, 7), plane(5, 4)], 80, NONE).is_none());
        assert!(encode(&[plane(9, 7), plane(5, 4), plane(5, 4), plane(5, 4)], 80, NONE).is_none());
        assert!(encode(&[plane(0, 7)], 80, NONE).is_none());
        assert!(encode(&[plane(9, 0)], 80, NONE).is_none());
        assert!(encode(&[plane(65536, 1)], 80, NONE).is_none());
        assert!(encode(&[plane(1, 65536)], 80, NONE).is_none());
        assert!(encode(&[Plane { w: 9, h: 7, px: vec![0; 62] }], 80, NONE).is_none());
        // Colour planes of the wrong size, or with the wrong number of samples.
        assert!(encode(&[plane(9, 7), plane(4, 4), plane(5, 4)], 80, NONE).is_none());
        assert!(encode(&[plane(9, 7), plane(5, 4), plane(5, 3)], 80, NONE).is_none());
        assert!(encode(&[plane(9, 7), plane(9, 7), plane(9, 7)], 80, NONE).is_none());
        assert!(encode(&[plane(9, 7), plane(5, 4), Plane { w: 5, h: 4, px: vec![0; 19] }], 80, NONE).is_none());
        // Out of time.
        assert!(encode(&[plane(9, 7)], 80, Deadline::after(Duration::ZERO)).is_none());
    }

    #[test]
    fn rgb_to_planes() {
        let one = |rgb: [u8; 3]| ycc_planes(&rgb, 1, 1).unwrap().iter().map(|p| p.px[0]).collect::<Vec<u8>>();
        assert_eq!(one([255, 255, 255]), [255, 128, 128]);
        assert_eq!(one([0, 0, 0]), [0, 128, 128]);
        assert_eq!(one([255, 0, 0]), [76, 85, 255]);
        assert_eq!(one([0, 255, 0]), [150, 44, 21]);
        assert_eq!(one([0, 0, 255]), [29, 255, 107]);
        // 3 by 3: red, white, black in every row. The colour planes are 2 by 2: the mean of red and white (to
        // the nearest, a half upwards), then black, over two rows and over the one left.
        let p = ycc_planes(&[255, 0, 0, 255, 255, 255, 0, 0, 0].repeat(3), 3, 3).unwrap();
        assert_eq!(p[0], Plane { w: 3, h: 3, px: [76, 255, 0].repeat(3) });
        assert_eq!(p[1], Plane { w: 2, h: 2, px: vec![107, 128, 107, 128] });
        assert_eq!(p[2], Plane { w: 2, h: 2, px: vec![192, 128, 192, 128] });
        // Four different pixels in one square.
        let p = ycc_planes(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255], 2, 2).unwrap();
        assert_eq!(p[1].px, [128]);
        assert_eq!(p[2].px, [128]);
        // The size must be the data's.
        assert!(ycc_planes(&[0; 12], 2, 2).is_some());
        assert!(ycc_planes(&[0; 11], 2, 2).is_none());
        assert!(ycc_planes(&[0; 12], 4, 1).is_some());
        assert!(ycc_planes(&[0; 12], 0, 4).is_none());
        assert!(ycc_planes(&[], 0, 0).is_none());
        assert!(ycc_planes(&[], usize::MAX, 3).is_none());
    }

    /// The files cjpeg made hold the picture make.py gave it, as far as their quantisation lets them: at the
    /// worst every coefficient is off by half its divisor, which bounds the mean squared error of a plane.
    /// A block in the wrong place or a wrong coefficient order gives errors many times these.
    #[test]
    fn libjpeg_files_decode_to_the_picture_they_were_made_from() {
        // gray.jpg is the red of a 61 by 37 picture at quality 85.
        let red = Plane { w: 61, h: 37, px: picture(61, 37).chunks(3).map(|p| p[0]).collect() };
        let grey = decode(GRAY, 1 << 20, NONE).unwrap();
        let e = sse(&grey.planes[0], &red);
        assert!(e <= 150 * 61 * 37, "gray: {e}");
        // base420.jpg is a 75 by 53 picture at quality 80, its colour planes halved as ours are (cjpeg rounds
        // their averages a little differently).
        let want = ycc_planes(&picture(75, 53), 75, 53).unwrap();
        let got = decode(BASE420, 1 << 20, NONE).unwrap();
        for (i, limit) in [250, 520, 520].into_iter().enumerate() {
            let e = sse(&got.planes[i], &want[i]);
            assert!(e <= limit * want[i].px.len() as u64, "base420 plane {i}: {e}");
        }
    }
}
