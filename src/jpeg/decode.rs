//! A JPEG decoder for the lossy image step: the samples of every component at the component's own sampling,
//! with no upsampling and no colour conversion. The file is read by `parse` and its coded data by
//! `scan::Tokens`, as the Huffman step reads them. The transform is our own and in integers, so every machine
//! gets the same samples.
//!
//! Only pictures every reader takes the same way are decoded: one component (grey), or three that are YCbCr
//! with the first at full sampling and the other two sampled alike. Everything else is refused.

use super::parse::{parse, Piece};
use super::scan::{Tokens, RESTART};
use super::Refused;
use crate::clock::Deadline;

/// One component's samples, w by h, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

/// A decoded picture: one plane (grey) or three (Y, Cb, Cr), each at its own sampling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub planes: Vec<Plane>,
}

/// Where in a block (row by row) each coefficient of the zigzag order goes.
pub(super) const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

// The transform, written from the symmetry of its cosines: cos((2x + 1) u pi / 16) is the same at x and 7 - x
// for even u and the opposite for odd u, so sums of mirrored samples give the even coefficients and their
// differences the odd ones. C1 to C7 are cos(k pi / 16) * 8192, rounded. One pass over 8 values gives 2^14
// times the standard's one-dimensional result (8192, and the standard halves each dimension); the first pass
// keeps 3 of those bits, so two passes give 2^17 times the two-dimensional result.
const C1: i64 = 8035;
const C2: i64 = 7568;
const C3: i64 = 6811;
const C4: i64 = 5793;
const C5: i64 = 4551;
const C6: i64 = 3135;
const C7: i64 = 1598;

/// The forward transform of the 8 values `step` apart from `at`, shifted right by `shift` with rounding.
fn forward(b: &mut [i64; 64], at: usize, step: usize, shift: u32) {
    let f = |k: usize| b[at + k * step];
    let s = [f(0) + f(7), f(1) + f(6), f(2) + f(5), f(3) + f(4)];
    let d = [f(0) - f(7), f(1) - f(6), f(2) - f(5), f(3) - f(4)];
    let a = [
        C4 * (s[0] + s[1] + s[2] + s[3]),
        C1 * d[0] + C3 * d[1] + C5 * d[2] + C7 * d[3],
        C2 * (s[0] - s[3]) + C6 * (s[1] - s[2]),
        C3 * d[0] - C7 * d[1] - C1 * d[2] - C5 * d[3],
        C4 * (s[0] - s[1] - s[2] + s[3]),
        C5 * d[0] - C1 * d[1] + C7 * d[2] + C3 * d[3],
        C6 * (s[0] - s[3]) - C2 * (s[1] - s[2]),
        C7 * d[0] - C5 * d[1] + C3 * d[2] - C1 * d[3],
    ];
    let half = (1 << shift) >> 1;
    for k in 0..8 {
        b[at + k * step] = (a[k] + half) >> shift;
    }
}

/// The inverse transform of the 8 values `step` apart from `at`, shifted right by `shift` with rounding.
fn inverse(b: &mut [i64; 64], at: usize, step: usize, shift: u32) {
    let a = |k: usize| b[at + k * step];
    let (e0, e1) = (C4 * (a(0) + a(4)), C4 * (a(0) - a(4)));
    let (e2, e3) = (C2 * a(2) + C6 * a(6), C6 * a(2) - C2 * a(6));
    let s = [e0 + e2, e1 + e3, e1 - e3, e0 - e2];
    let d = [
        C1 * a(1) + C3 * a(3) + C5 * a(5) + C7 * a(7),
        C3 * a(1) - C7 * a(3) - C1 * a(5) - C5 * a(7),
        C5 * a(1) - C1 * a(3) + C7 * a(5) + C3 * a(7),
        C7 * a(1) - C5 * a(3) + C3 * a(5) - C1 * a(7),
    ];
    let half = (1 << shift) >> 1;
    for i in 0..4 {
        b[at + i * step] = (s[i] + d[i] + half) >> shift;
        b[at + (7 - i) * step] = (s[i] - d[i] + half) >> shift;
    }
}

/// The forward DCT of a block (samples less 128, row by row), in place: 2^17 times each coefficient, left for
/// the quantiser to divide out. For 8-bit samples every value stays within 2^28.
pub(super) fn fdct(b: &mut [i64; 64]) {
    for i in 0..8 {
        forward(b, 8 * i, 1, 11);
    }
    for i in 0..8 {
        forward(b, i, 8, 0);
    }
}

/// The inverse DCT of a block (coefficients within 16 bits, row by row), in place: the samples less 128.
pub(super) fn idct(b: &mut [i64; 64]) {
    for i in 0..8 {
        inverse(b, 8 * i, 1, 11);
    }
    for i in 0..8 {
        inverse(b, i, 8, 17);
    }
}

/// One block of an MCU: its component and quantisation table (row by row), how many blocks of the component
/// an MCU has across and down, and which of them this one is.
struct Block {
    c: usize,
    q: [u16; 64],
    h: u64,
    v: u64,
    x: u64,
    y: u64,
}

/// The tables of a DQT segment's body into `quant`. None if the segment is not as `parse` checked it.
fn read_quant(mut b: &[u8], quant: &mut [Option<[u16; 64]>; 4]) -> Option<()> {
    while let Some((&pt, rest)) = b.split_first() {
        // Bytes per value.
        let size = match pt >> 4 {
            0 => 1,
            1 => 2,
            _ => return None,
        };
        let (values, after) = rest.split_at_checked(64 * size)?;
        let mut q = [0u16; 64];
        for (v, &z) in values.chunks_exact(size).zip(&ZIGZAG) {
            q[z as usize] = v.iter().fold(0, |a, &x| a << 8 | x as u16);
        }
        *quant.get_mut((pt & 15) as usize)? = Some(q);
        b = after;
    }
    Some(())
}

/// Dequantises a block, transforms it and puts its samples at block (`x`, `y`) of `p`. None if that is outside.
fn put_block(p: &mut Plane, x: u64, y: u64, coef: &[i32; 64], q: &[u16; 64]) -> Option<()> {
    if x >= (p.w / 8) as u64 || y >= (p.h / 8) as u64 {
        return None;
    }
    let mut b = [0i64; 64];
    for i in 0..64 {
        b[i] = (coef[i] as i64 * q[i] as i64).clamp(-32768, 32767);
    }
    idct(&mut b);
    let at = y as usize * 8 * p.w + x as usize * 8;
    for (r, row) in b.chunks_exact(8).enumerate() {
        for (o, &v) in p.px.get_mut(at + r * p.w..)?.iter_mut().zip(row) {
            *o = (v + 128).clamp(0, 255) as u8;
        }
    }
    Some(())
}

/// The samples of a baseline or extended sequential JPEG that is grey or YCbCr. Refused, with the reason: what
/// `parse` refuses, other colour layouts, a picture whose planes (padded to whole MCUs) hold more than
/// `max_samples` samples together, and one not done by `deadline`.
pub fn decode(d: &[u8], max_samples: usize, deadline: Deadline) -> Result<Picture, Refused> {
    const BAD: Refused = Refused("bad scan");
    let j = parse(d).map_err(Refused)?;
    let f = &j.frame;
    let n = f.comps.len();
    if n != 1 && n != 3 {
        return Err(Refused("not grey or YCbCr"));
    }
    if f.comps[0].h as u32 != f.hmax || f.comps[0].v as u32 != f.vmax {
        return Err(Refused("first component not at full sampling"));
    }
    if n == 3 {
        let (a, b) = (f.comps[1], f.comps[2]);
        if (a.h, a.v) != (b.h, b.v) {
            return Err(Refused("colour components sampled differently"));
        }
        // Readers differ on these: some take the ids for the colours, some don't look.
        if [f.comps[0].id, a.id, b.id] == *b"RGB" {
            return Err(Refused("components named R, G, B"));
        }
    }
    // Every plane padded to whole MCUs, as an interleaved scan fills it.
    let (width, height) = (f.width as usize, f.height as usize);
    let (mx, my) = (width.div_ceil(8 * f.hmax as usize), height.div_ceil(8 * f.vmax as usize));
    let mut planes: Vec<Plane> =
        f.comps.iter().map(|c| Plane { w: mx * c.h as usize * 8, h: my * c.v as usize * 8, px: Vec::new() }).collect();
    let total = planes.iter().try_fold(0u64, |t, p| t.checked_add((p.w as u64).checked_mul(p.h as u64)?));
    if total.is_none_or(|t| t > max_samples as u64) {
        return Err(Refused("too large"));
    }
    for p in &mut planes {
        p.px = vec![0; p.w * p.h];
    }
    let mut quant = [None; 4];
    for piece in &j.pieces {
        match piece {
            Piece::Keep(r) => {
                let s = d.get(r.clone()).unwrap_or(&[]);
                // A DQT segment: its tables come after the marker and the length.
                if let Some(body) = s.strip_prefix(&[0xFF, 0xDB]).and_then(|b| b.get(2..)) {
                    read_quant(body, &mut quant).ok_or(Refused("bad quantisation table"))?;
                }
                // Adobe's segment says what three components are: 1 is YCbCr, 0 is RGB.
                let adobe = s.starts_with(&[0xFF, 0xEE]) && s.get(4..9) == Some(&b"Adobe"[..]);
                if n == 3 && adobe && s.get(15).is_some_and(|&transform| transform != 1) {
                    return Err(Refused("Adobe segment says not YCbCr"));
                }
            }
            Piece::Tables => {}
            Piece::Scan(i) => {
                let s = j.scans.get(*i).ok_or(BAD)?;
                // The blocks of one MCU, in the order they are coded, and the MCUs in a row. A scan of one
                // component is not interleaved: its MCU is one block, in rows of the component's own blocks.
                let single = s.comps.len() == 1;
                let mut across = mx as u64;
                let mut mcu = Vec::new();
                for &(ci, _, _) in &s.comps {
                    let c = f.comps.get(ci).ok_or(BAD)?;
                    let q = quant.get(c.tq as usize).copied().flatten();
                    let q = q.ok_or(Refused("scan uses an undefined quantisation table"))?;
                    let (h, v) = if single { (1, 1) } else { (c.h as u64, c.v as u64) };
                    if single {
                        across = (f.width as u64 * c.h as u64).div_ceil(8 * f.hmax as u64);
                    }
                    for y in 0..v {
                        for x in 0..h {
                            mcu.push(Block { c: ci, q, h, v, x, y });
                        }
                    }
                }
                let per = mcu.len() as u64;
                if per == 0 || across == 0 {
                    return Err(BAD);
                }
                let mut t = Tokens::new(&j, s).map_err(Refused)?;
                let mut buf = Tokens::buffer();
                // The last DC coefficient of each component.
                let mut pred = [0i32; 3];
                let mut coef = [0i32; 64];
                // Blocks done, and the next coefficient of the block being read (64: none is).
                let (mut done, mut k) = (0u64, 64usize);
                loop {
                    let got = t.next(&mut buf).map_err(Refused)?;
                    if got == 0 {
                        break;
                    }
                    for tok in &buf[..got] {
                        let (sym, size, bits) = (tok.sym(), tok.nbits() as u32, tok.bits() as i32);
                        // The value the extra bits stand for: the lower half of the numbers of a size are
                        // the negative ones.
                        let v = if size > 0 && bits < 1 << (size - 1) { bits - (1 << size) + 1 } else { bits };
                        let b = &mcu[(done % per) as usize];
                        if tok.slot() == RESTART {
                            pred = [0; 3];
                        } else if tok.slot() < 4 {
                            // A DC token starts a block. Once per row of MCUs, the clock.
                            if done.is_multiple_of(per * across) && deadline.passed() {
                                return Err(Refused("out of time"));
                            }
                            // Kept within 16 bits: damaged data can add up any number of differences.
                            let p = pred.get_mut(b.c).ok_or(BAD)?;
                            *p = (*p + v).clamp(-32768, 32767);
                            coef = [0; 64];
                            coef[0] = *p;
                            k = 1;
                        } else if k < 64 {
                            match sym {
                                0x00 => k = 64,
                                0xF0 => k += 16,
                                _ => {
                                    k += (sym >> 4) as usize;
                                    if let Some(&z) = ZIGZAG.get(k) {
                                        coef[z as usize] = v;
                                    }
                                    k += 1;
                                }
                            }
                            if k >= 64 {
                                let (m, p) = (done / per, planes.get_mut(b.c).ok_or(BAD)?);
                                put_block(p, m % across * b.h + b.x, m / across * b.v + b.y, &coef, &b.q).ok_or(BAD)?;
                                done += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    // The planes without their padding, moved up in place.
    for (p, c) in planes.iter_mut().zip(&f.comps) {
        let w = (width * c.h as usize).div_ceil(f.hmax as usize);
        let h = (height * c.v as usize).div_ceil(f.vmax as usize);
        for y in 1..h {
            p.px.copy_within(y * p.w..y * p.w + w, y * w);
        }
        p.px.truncate(w * h);
        (p.w, p.h) = (w, h);
    }
    Ok(Picture { width, height, planes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const NONE: Deadline = Deadline::NONE;

    /// A file of tests/data/jpeg, its name in one or more parts.
    macro_rules! data {
        ($($name:literal),+) => {
            &include_bytes!(concat!("../../tests/data/jpeg/", $($name),+))[..]
        };
    }

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// The transform as the standard defines it, in floating point.
    fn reference(b: &[f64; 64], inverse: bool) -> [f64; 64] {
        let c = |u: usize| if u == 0 { 0.5f64.sqrt() } else { 1.0 };
        let cos = |x: usize, u: usize| ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI / 16.0).cos();
        let mut out = [0.0; 64];
        // Sample (x, y) and coefficient (u, v).
        for (xy, uv) in (0..64 * 64).map(|i| (i / 64, i % 64)) {
            let k = c(uv % 8) * c(uv / 8) * cos(xy % 8, uv % 8) * cos(xy / 8, uv / 8) / 4.0;
            if inverse {
                out[xy] += k * b[uv];
            } else {
                out[uv] += k * b[xy];
            }
        }
        out
    }

    #[test]
    fn transforms_match_their_definition() {
        // Flat blocks at both ends of the range, the sharpest pattern, then noise: over the whole range, and
        // of a few levels around one level.
        let sharpest = std::array::from_fn(|i| if (i + i / 8).is_multiple_of(2) { 127 } else { -128 });
        let mut blocks: Vec<[i64; 64]> = vec![[-128; 64], [127; 64], sharpest];
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        for round in 0..300u64 {
            let (base, spread) = if round < 100 { (0, 256) } else { (rng(&mut s) % 250, 1 + round % 6) };
            blocks.push(std::array::from_fn(|_| (base + rng(&mut s) % spread) as i64 - 128));
        }
        for px in blocks {
            let want = reference(&px.map(|v| v as f64), false);
            let mut f = px;
            fdct(&mut f);
            for i in 0..64 {
                assert!((f[i] as f64 / 131072.0 - want[i]).abs() < 1.0, "forward {i}: {} {}", f[i], want[i]);
            }
            let mut c = f.map(|v| (v + (1 << 16)) >> 17);
            let want = reference(&c.map(|v| v as f64), true);
            idct(&mut c);
            for i in 0..64 {
                assert!((c[i] as f64 - want[i]).abs() < 1.0, "inverse {i}: {} {}", c[i], want[i]);
            }
            // Forward then inverse gives the samples back. The coefficients are kept to an eighth here, so
            // that their rounding (which alone can move a sample by more than 1) stays out of it.
            let mut back = f.map(|v| (v + (1 << 13)) >> 14);
            idct(&mut back);
            for i in 0..64 {
                assert!((((back[i] + 4) >> 3) - px[i]).abs() <= 1, "round trip {i}: {} {}", back[i], px[i]);
            }
        }
        // The largest coefficients a file can ask for.
        let mixed = std::array::from_fn(|i| if i.is_multiple_of(2) { 32767 } else { -32768 });
        for mut b in [[32767i64; 64], [-32768; 64], mixed] {
            idct(&mut b);
        }
    }

    #[test]
    fn test_files_decode_as_their_jpegtran_copies() {
        macro_rules! case {
            ($name:literal, $size:expr, $colour:expr) => {
                ($name, data!($name, ".jpg"), data!($name, ".ref.jpg"), $size, $colour)
            };
        }
        // The file, jpegtran's copy of it, the picture's size and its colour planes' (none: grey).
        let cases = [
            case!("gray", (61, 37), None),
            case!("base420", (75, 53), Some((38, 27))),
            case!("base444", (75, 53), Some((75, 53))),
            case!("base422", (75, 53), Some((38, 53))),
            case!("base440", (75, 53), Some((75, 27))),
            case!("restart1", (75, 53), Some((38, 27))),
            case!("restart5", (75, 53), Some((38, 27))),
            case!("sof1", (75, 53), Some((38, 27))),
            case!("edge17x9", (17, 9), Some((17, 9))),
            case!("multiscan", (75, 53), Some((38, 27))),
            case!("multiscan2", (75, 53), Some((38, 27))),
            case!("tiny", (1, 1), Some((1, 1))),
            case!("optimized", (75, 53), Some((38, 27))),
        ];
        for (name, a, b, size, colour) in cases {
            let p = decode(a, 1 << 20, NONE).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((p.width, p.height), size, "{name}");
            let sizes: Vec<(usize, usize)> = p.planes.iter().map(|q| (q.w, q.h)).collect();
            assert_eq!(sizes, colour.map_or(vec![size], |c| vec![size, c, c]), "{name}");
            assert!(p.planes.iter().all(|q| q.px.len() == q.w * q.h), "{name}");
            assert_eq!(decode(b, 1 << 20, NONE).as_ref(), Ok(&p), "{name}: jpegtran's copy");
        }
        // One picture coded six ways (restart intervals, scans of one and of two components, other tables): the
        // same coefficients, so the same samples.
        let first = decode(data!("base420.jpg"), 1 << 20, NONE);
        assert!(first.is_ok());
        for other in [
            data!("restart1.jpg"),
            data!("restart5.jpg"),
            data!("multiscan.jpg"),
            data!("multiscan2.jpg"),
            data!("optimized.jpg"),
        ] {
            assert_eq!(decode(other, 1 << 20, NONE), first);
        }
    }

    fn find(h: &[u8], n: &[u8]) -> usize {
        h.windows(n.len()).position(|w| w == n).expect("pattern")
    }

    #[test]
    fn only_grey_and_ycbcr() {
        assert_eq!(decode(data!("cmyk.jpg"), 1 << 20, NONE), Err(Refused("not grey or YCbCr")));
        assert_eq!(decode(data!("progressive.jpg"), 1 << 20, NONE), Err(Refused("progressive")));
        assert_eq!(decode(data!("arithmetic.jpg"), 1 << 20, NONE), Err(Refused("arithmetic coding")));
        // An Adobe segment right after the start marker: transform 1 is YCbCr, anything else is not.
        let with_adobe = |a: &[u8], transform: u8| {
            [&a[..2], &[0xFF, 0xEE, 0, 14], b"Adobe", &[0, 100, 0, 0, 0, 0, transform], &a[2..]].concat()
        };
        let a = data!("base444.jpg");
        let plain = decode(a, 1 << 20, NONE);
        assert!(plain.is_ok());
        assert_eq!(decode(&with_adobe(a, 1), 1 << 20, NONE), plain);
        for transform in [0, 2] {
            assert_eq!(decode(&with_adobe(a, transform), 1 << 20, NONE), Err(Refused("Adobe segment says not YCbCr")));
        }
        // A grey picture is grey whatever the segment says.
        let g = data!("gray.jpg");
        assert_eq!(decode(&with_adobe(g, 0), 1 << 20, NONE), decode(g, 1 << 20, NONE));
        // Components named R, G, B (in the frame header and the scan header).
        let a = data!("base420.jpg");
        let (sof, sos) = (find(a, &[0xFF, 0xC0]), find(a, &[0xFF, 0xDA]));
        let mut b = a.to_vec();
        for (i, id) in b"RGB".iter().enumerate() {
            b[sof + 10 + 3 * i] = *id;
            b[sos + 5 + 2 * i] = *id;
        }
        assert_eq!(decode(&b, 1 << 20, NONE), Err(Refused("components named R, G, B")));
        // Luma at half the sampling of a colour component, and colour components sampled differently.
        let patched = |at: &[(usize, u8)]| {
            let mut b = a.to_vec();
            at.iter().for_each(|&(i, v)| b[sof + i] = v);
            b
        };
        let low = patched(&[(11, 0x11), (14, 0x22)]);
        assert_eq!(decode(&low, 1 << 20, NONE), Err(Refused("first component not at full sampling")));
        let uneven = patched(&[(14, 0x21)]);
        assert_eq!(decode(&uneven, 1 << 20, NONE), Err(Refused("colour components sampled differently")));
    }

    #[test]
    fn limits() {
        // 5 by 4 MCUs: 80 by 64 samples of luma and two planes of 40 by 32.
        let a = data!("base420.jpg");
        assert!(decode(a, 7680, NONE).is_ok());
        assert_eq!(decode(a, 7679, NONE), Err(Refused("too large")));
        assert_eq!(decode(a, 0, NONE), Err(Refused("too large")));
        assert_eq!(decode(a, 1 << 20, Deadline::after(Duration::ZERO)), Err(Refused("out of time")));
    }

    #[test]
    fn damaged_files_never_panic() {
        for a in
            [data!("tiny.jpg"), data!("gray.jpg"), data!("multiscan.jpg"), data!("restart5.jpg"), data!("sof1.jpg")]
        {
            for n in 0..a.len() {
                assert!(decode(&a[..n], 1 << 16, NONE).is_err(), "cut at {n}");
            }
            let mut b = a.to_vec();
            for i in 0..a.len() {
                for flip in [0xFF, 0x01] {
                    b[i] = a[i] ^ flip;
                    let _ = decode(&b, 1 << 16, NONE);
                }
                b[i] = a[i];
            }
        }
    }
}
