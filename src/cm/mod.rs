//! Our context-mixing coder: a binary arithmetic coder driven by many small adaptive models whose
//! predictions are combined by trained mixers. Integer arithmetic only, so encoding and decoding give
//! identical results on every platform (native and WebAssembly).

pub mod model;

// ---------------------------------------------------------------- logistic helpers (12-bit domain)

// Stretched probabilities are ln(p / (1 - p)) times 256, within +-2047; probabilities are 12-bit. The tables
// are worked out in floating point when compiling (so every platform gets the same integers) by the two
// series below; nothing at run time uses floating point.

/// e^x for 0 <= x <= 8, by its Taylor series: every term is positive, so nothing cancels, and 64 terms take
/// it far below f64 precision.
const fn exp(x: f64) -> f64 {
    let (mut sum, mut term, mut n) = (1.0, 1.0, 1.0);
    while n < 64.0 {
        term = term * x / n;
        sum += term;
        n += 1.0;
    }
    sum
}

/// ln(x) for x >= 1: x = m 2^e with m in [1, 2), and ln(m) = 2 atanh(z) with z = (m - 1) / (m + 1) below 1/3,
/// by the series 2 (z + z^3/3 + z^5/5 + ...).
const fn ln(x: f64) -> f64 {
    let (mut m, mut e) = (x, 0.0);
    while m >= 2.0 {
        m /= 2.0;
        e += 1.0;
    }
    let z = (m - 1.0) / (m + 1.0);
    let (mut sum, mut zk, mut k) = (0.0, z, 1.0);
    while k < 60.0 {
        sum += zk / k;
        zk *= z * z;
        k += 2.0;
    }
    e * std::f64::consts::LN_2 + 2.0 * sum
}

/// squash(d) for d in 0..=2047: 4096 / (1 + e^(-x)) = 4096 e^x / (1 + e^x) with x = d / 256, rounded, at most
/// 4095.
const SQUASH_HALF: [u16; 2048] = {
    let mut t = [0u16; 2048];
    let mut d = 0;
    while d < 2048 {
        let e = exp(d as f64 / 256.0);
        let p = (4096.0 * e / (1.0 + e) + 0.5) as u16;
        t[d] = if p > 4095 { 4095 } else { p };
        d += 1;
    }
    t
};

/// The logistic function: the 12-bit probability whose stretched value is `d`, 1 below -2047 and 4095 above
/// 2047. The negative half mirrors the positive one, so squash(-d) + squash(d) == 4096.
pub const fn squash(d: i32) -> i32 {
    if d > 2047 {
        4095
    } else if d < -2047 {
        1
    } else if d >= 0 {
        SQUASH_HALF[d as usize] as i32
    } else {
        4096 - SQUASH_HALF[-d as usize] as i32
    }
}

/// squash(d) for d in -2047..=2047, at index d + 2047.
static SQUASH: [u16; 4095] = {
    let mut t = [0u16; 4095];
    let mut i = 0;
    while i < 4095 {
        t[i] = squash(i as i32 - 2047) as u16;
        i += 1;
    }
    t
};

/// stretch(p) for p in 0..=4095: 256 (ln(p) - ln(4096 - p)) rounded half away from 0 (swapping p and 4096 - p
/// only negates it, exactly), within +-2047.
static STRETCH: [i16; 4096] = {
    let mut t = [-2047i16; 4096];
    let mut p = 1;
    while p < 4096 {
        let x = 256.0 * (ln(p as f64) - ln((4096 - p) as f64));
        let d = if x < 0.0 { x - 0.5 } else { x + 0.5 } as i32;
        t[p] = if d.abs() > 2047 { 2047 * d.signum() } else { d } as i16;
        p += 1;
    }
    t
};

/// The inverse of squash: ln(p / (4096 - p)) times 256, rounded, within +-2047, for a 12-bit probability p
/// (clamped to 0..=4095; 0 gives -2047).
#[inline]
pub fn stretch(p: i32) -> i32 {
    STRETCH[p.clamp(0, 4095) as usize] as i32
}

/// `N` copies of `v` in a boxed array, made on the heap (a large array would not fit on the stack first).
fn boxed<T: Clone, const N: usize>(v: T) -> Box<[T; N]> {
    match vec![v; N].into_boxed_slice().try_into() {
        Ok(b) => b,
        Err(_) => unreachable!("the vector has N items"),
    }
}

// ---------------------------------------------------------------- arithmetic coder

/// Codes one bit with probability `p1` (12-bit, of a 1) — the same interface for encoding and decoding.
pub trait Coder {
    fn code(&mut self, bit: Option<u32>, p1: i32) -> u32;
    /// The same with a 16-bit probability, for decisions that are almost certain (long repeats).
    fn code16(&mut self, bit: Option<u32>, p16: u32) -> u32 {
        self.code(bit, (p16 >> 4) as i32)
    }
    /// A decoder that has read well past the end of its input is decoding a damaged file.
    fn exhausted(&self) -> bool {
        false
    }
}

pub struct Encoder {
    x1: u32,
    x2: u32,
    pub out: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Encoder {
        Encoder { x1: 0, x2: 0xffff_ffff, out: Vec::new() }
    }
    pub fn finish(mut self) -> Vec<u8> {
        // Enough bytes to pin the final interval.
        self.out.extend_from_slice(&self.x1.to_be_bytes());
        self.out
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    #[inline]
    fn code_at(&mut self, bit: u32, xmid: u32) -> u32 {
        if bit == 1 {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.out.push((self.x2 >> 24) as u8);
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 255;
        }
        bit
    }
}

impl Coder for Encoder {
    #[inline]
    fn code16(&mut self, bit: Option<u32>, p16: u32) -> u32 {
        let bit = bit.expect("encoder needs the bit");
        let p = p16.clamp(1, 65535) as u64;
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p) >> 16) as u32;
        self.code_at(bit, xmid)
    }
    #[inline]
    fn code(&mut self, bit: Option<u32>, p1: i32) -> u32 {
        let bit = bit.expect("encoder needs the bit");
        let p = p1.clamp(1, 4095) as u64;
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p) >> 12) as u32;
        if bit == 1 {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.out.push((self.x2 >> 24) as u8);
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 255;
        }
        bit
    }
}

pub struct Decoder<'a> {
    x1: u32,
    x2: u32,
    x: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Decoder<'a> {
        let mut d = Decoder { x1: 0, x2: 0xffff_ffff, x: 0, data, pos: 0 };
        for _ in 0..4 {
            d.x = (d.x << 8) | d.next() as u32;
        }
        d
    }
    #[inline]
    fn next(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }
    pub fn overrun(&self) -> bool {
        self.pos > self.data.len() + 4
    }
}

impl<'a> Decoder<'a> {
    #[inline]
    fn code_at(&mut self, xmid: u32) -> u32 {
        let bit = if self.x <= xmid { 1 } else { 0 };
        if bit == 1 {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 255;
            self.x = (self.x << 8) | self.next() as u32;
        }
        bit
    }
}

impl<'a> Coder for Decoder<'a> {
    fn exhausted(&self) -> bool {
        self.overrun()
    }
    #[inline]
    fn code16(&mut self, _bit: Option<u32>, p16: u32) -> u32 {
        let p = p16.clamp(1, 65535) as u64;
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p) >> 16) as u32;
        self.code_at(xmid)
    }
    #[inline]
    fn code(&mut self, _bit: Option<u32>, p1: i32) -> u32 {
        let p = p1.clamp(1, 4095) as u64;
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p) >> 12) as u32;
        let bit = if self.x <= xmid { 1 } else { 0 };
        if bit == 1 {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 255;
            self.x = (self.x << 8) | self.next() as u32;
        }
        bit
    }
}

// ---------------------------------------------------------------- adaptive pieces

/// Probability with a count-driven learning rate, packed in 32 bits (22-bit probability, 10-bit count).
#[derive(Clone, Copy)]
pub struct Counter(pub u32);

pub const RECIP: [u32; 1024] = {
    let mut t = [0u32; 1024];
    let mut i = 0;
    while i < 1024 {
        t[i] = 65536 / (i as u32 + 2);
        i += 1;
    }
    t
};

impl Counter {
    pub const fn new() -> Counter {
        Counter(1 << 31)
    }
    #[inline]
    pub fn p12(self) -> i32 {
        (self.0 >> 20) as i32
    }
    #[inline]
    pub fn p16(self) -> u32 {
        (self.0 >> 16).clamp(1, 65535)
    }
    #[inline]
    pub fn update(&mut self, bit: u32, limit: u32) {
        let n = self.0 & 1023;
        let p = (self.0 >> 10) as i64; // 22 bits
        let target = if bit == 1 { (1i64 << 22) - 1 } else { 0 };
        let np = p + (((target - p) * RECIP[n as usize] as i64) >> 16);
        let nn = if n < limit { n + 1 } else { n };
        self.0 = ((np as u32) << 10) | nn;
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps a small context (below `N`, a power of two) to an adaptive probability.
pub struct StateMap<const N: usize> {
    t: [Counter; N],
    limit: u32,
    cx: usize,
}

impl<const N: usize> StateMap<N> {
    pub fn new(limit: u32) -> StateMap<N> {
        const { assert!(N.is_power_of_two()) };
        StateMap { t: [Counter::new(); N], limit, cx: 0 }
    }
    /// Contexts are below N: the mask changes nothing but spares the bounds checks.
    #[inline]
    pub fn p(&mut self, cx: usize) -> i32 {
        self.cx = cx & (N - 1);
        self.t[self.cx].p12()
    }
    #[inline]
    pub fn update(&mut self, bit: u32) {
        let l = self.limit;
        self.t[self.cx & (N - 1)].update(bit, l);
    }
}

/// Mixer lanes: inputs and weights come in blocks of 16; the lanes past the inputs stay 0.
pub const LANES: usize = 16;
/// The mixer's learning rate and weight shift (fixed by the format). The error times the rate fits in 16
/// bits, so the weight update is a 16 x 16-bit product per lane.
const MIXER_RATE: i32 = 6;
const MIXER_SHIFT: i32 = 12;
const _: () = assert!(4095 * MIXER_RATE <= i16::MAX as i32);

/// One weight set: a cache line.
#[derive(Clone, Copy)]
#[repr(align(64))]
struct Row([i32; LANES]);

/// Weighted combination of `N` stretched predictions, one weight set per selector context.
pub struct Mixer<const N: usize> {
    w: Vec<Row>,
    /// The inputs, set before `mix`: stretched probabilities, at most 2047 either way. Lanes N.. stay 0.
    pub x: [i16; LANES],
    row: usize,
    pr: i32,
}

impl<const N: usize> Mixer<N> {
    pub fn new(selectors: usize) -> Mixer<N> {
        const { assert!(N <= LANES) };
        let mut r = [0i32; LANES];
        r[..N].fill((1 << 16) / 4);
        Mixer { w: vec![Row(r); selectors], x: [0; LANES], row: 0, pr: 2048 }
    }
    #[inline]
    pub fn mix(&mut self, sel: usize) -> i32 {
        self.row = sel;
        let w = &self.w[sel].0;
        // A 64-bit sum is exact for any weights; the order of the terms doesn't matter.
        let mut dot = 0i64;
        for j in 0..N {
            dot += self.x[j] as i64 * w[j] as i64;
        }
        let d = (dot >> 16).clamp(-2047, 2047) as i32;
        self.pr = SQUASH[(d + 2047) as usize] as i32;
        self.pr
    }
    #[inline]
    pub fn update(&mut self, bit: u32) {
        // At most 4095 x MIXER_RATE either way, so exact in 16 bits; an input times the error fits in 32 bits.
        let err = ((((bit as i32) << 12) - self.pr) * MIXER_RATE) as i16 as i32;
        let w = &mut self.w[self.row].0;
        // All 16 lanes (the unused ones add 0). The weights wrap as 32-bit numbers, as they always did in the
        // release build (a debug build no longer stops there).
        for j in 0..LANES {
            w[j] = w[j].wrapping_add((self.x[j] as i32 * err + (1 << (MIXER_SHIFT - 1))) >> MIXER_SHIFT);
        }
    }
}

/// Knots of each map: at stretched values -2048, -1920, ..., 2048 (128 apart, half a unit of ln-odds).
const APM_KNOTS: usize = 33;

/// An adaptive probability map (secondary estimation): for each of `N` contexts (a power of two; a context
/// number is taken modulo `N`), a piecewise-linear map from a stretched probability to a 12-bit probability.
/// Each map is 33 knots of 16-bit probability (66 bytes), each starting as squash of its place; `p` interpolates
/// between the two knots around its input, and `update` moves both the same fraction of the way toward the coded
/// bit, so that each knot learns the probability of the inputs within one spacing of it.
pub struct Apm<const N: usize> {
    t: Box<[[u16; APM_KNOTS]; N]>,
    /// A step moves a knot by (end - knot) >> shift: 1/2^shift of the way to the bit's end of the scale (16 or
    /// 65520: 1/4096 or 4095/4096), rounded up from a quarter (`ends` are offset to do it, mirroring each other),
    /// so a knot gets to within a quarter step of the end (rounding from a half stops it up to half a step short:
    /// measured, slightly larger archives).
    shift: u32,
    ends: [i32; 2],
    /// Where the last `p` looked: the knot below its input, counted over all the contexts' knots.
    at: usize,
}

impl<const N: usize> Apm<N> {
    /// `rate`: each step is 1/2^(rate - 1) of the way, as two knots learn from every bit (with the model's rate
    /// of 7, steps of 1/64 gave the smallest archives over the benchmark sample: 1/128 gave larger ones at both
    /// efforts measured, 1/32 smaller ones at the lower effort but larger ones at the higher).
    pub fn new(rate: i32) -> Apm<N> {
        const { assert!(N.is_power_of_two()) };
        let knots = std::array::from_fn(|j| (squash(j as i32 * 128 - 2048) * 16) as u16);
        let shift = (rate - 1).clamp(2, 15) as u32;
        let quarter = 1 << (shift - 2);
        Apm { t: boxed(knots), shift, ends: [16 + quarter - 1, 65520 + 3 * quarter], at: 0 }
    }
    /// The knots stay within 16..=65520, so the result is within 1..=4095.
    #[inline]
    pub fn p(&mut self, st: i32, cx: usize) -> i32 {
        let x = st.clamp(-2047, 2047) + 2048;
        let (at, w) = ((cx & (N - 1)) * APM_KNOTS + (x >> 7) as usize, x & 127);
        self.at = at;
        let k = self.t.as_flattened();
        let p16 = (k[at] as i32 * (128 - w) + k[at + 1] as i32 * w) >> 7;
        (p16 + 8) >> 4
    }
    /// A step never passes the bit's end, so the knots stay within 16..=65520.
    #[inline]
    pub fn update(&mut self, bit: u32) {
        let end = self.ends[bit as usize & 1];
        let k = &mut self.t.as_flattened_mut()[self.at..self.at + 2];
        k[0] = (k[0] as i32 + ((end - k[0] as i32) >> self.shift)) as u16;
        k[1] = (k[1] as i32 + ((end - k[1] as i32) >> self.shift)) as u16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coder_round_trip_with_skewed_probabilities() {
        let mut s = 12345u64;
        let mut bits = Vec::new();
        let mut probs = Vec::new();
        for _ in 0..200_000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let p = (s % 4095) as i32 + 1;
            let bit = if (s >> 20) % 4096 < p as u64 { 1 } else { 0 };
            bits.push(bit);
            probs.push(p);
        }
        let mut e = Encoder::new();
        for (b, p) in bits.iter().zip(&probs) {
            e.code(Some(*b), *p);
        }
        let out = e.finish();
        let mut d = Decoder::new(&out);
        for (b, p) in bits.iter().zip(&probs) {
            assert_eq!(d.code(None, *p), *b);
        }
    }

    #[test]
    fn coder_round_trip_with_16_bit_probabilities() {
        let mut s = 987654321u64;
        let mut items = Vec::new();
        for i in 0..300_000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            if i % 3 == 0 {
                let p = (s % 4095) as i32 + 1;
                items.push((false, p as u32, ((s >> 20) % 4096 < p as u64) as u32));
            } else {
                let p = 65535 - (s % 64) as u32;
                items.push((true, p, ((s >> 24) % 65536 < p as u64) as u32));
            }
        }
        let mut e = Encoder::new();
        for &(wide, p, b) in &items {
            if wide {
                e.code16(Some(b), p);
            } else {
                e.code(Some(b), p as i32);
            }
        }
        let out = e.finish();
        let mut d = Decoder::new(&out);
        for &(wide, p, b) in &items {
            let got = if wide { d.code16(None, p) } else { d.code(None, p as i32) };
            assert_eq!(got, b);
        }
    }

    #[test]
    fn squash_stretch_are_inverse_enough() {
        for p in (1..4095).step_by(7) {
            let back = squash(stretch(p));
            assert!((back - p).abs() <= 40, "{p} -> {back}");
        }
    }

    #[test]
    fn squash_is_the_logistic_function() {
        for d in -2047..=2047 {
            let f = (4096.0 / (1.0 + (-d as f64 / 256.0).exp())).clamp(1.0, 4095.0);
            assert!((squash(d) as f64 - f).abs() <= 1.0, "{d}: {} for {f}", squash(d));
            assert!(squash(d) >= squash(d - 1), "{d}");
            assert_eq!(SQUASH[(d + 2047) as usize] as i32, squash(d), "{d}");
        }
        for d in 1..=2047 {
            assert_eq!(squash(-d) + squash(d), 4096, "{d}");
        }
        assert_eq!(squash(0), 2048);
        for d in [-2048, -2049, -100_000, i32::MIN] {
            assert_eq!(squash(d), 1, "{d}");
        }
        for d in [2048, 2049, 100_000, i32::MAX] {
            assert_eq!(squash(d), 4095, "{d}");
        }
        // Usable when compiling.
        const _: () = assert!(squash(0) == 2048 && squash(2047) == 4095);
    }

    #[test]
    fn stretch_is_the_ln_odds() {
        for p in 0..=4095 {
            let f = (256.0 * (p as f64 / (4096 - p) as f64).ln()).round().clamp(-2047.0, 2047.0);
            assert!((stretch(p) as f64 - f).abs() <= 1.0, "{p}: {} for {f}", stretch(p));
            assert!(p == 0 || stretch(p) >= stretch(p - 1), "{p}");
        }
        assert_eq!(stretch(2048), 0);
        for p in [-1, -4096, i32::MIN] {
            assert_eq!(stretch(p), stretch(0), "{p}");
        }
        for p in [4096, 5000, i32::MAX] {
            assert_eq!(stretch(p), stretch(4095), "{p}");
        }
        for p in 1..=4095 {
            let back = squash(stretch(p));
            assert!((back - p).abs() <= 2, "{p} -> {back}");
        }
    }

    #[test]
    fn apm_starts_as_squash() {
        let mut a = Apm::<4>::new(7);
        for st in -2047..=2047 {
            let p = a.p(st, st as usize);
            // Linear between knots half a unit of ln-odds apart: at most 13 away where the curve bends most.
            assert!((p - squash(st)).abs() <= 13, "{st}: {p} for {}", squash(st));
        }
        for j in -16..=16 {
            assert!((a.p(j * 128, 0) - squash(j * 128)).abs() <= 1, "{j}");
        }
    }

    #[test]
    fn apm_moves_toward_the_bits() {
        let mut a = Apm::<16>::new(7);
        let p0 = a.p(300, 5);
        for _ in 0..50 {
            a.p(300, 5);
            a.update(1);
        }
        let p1 = a.p(300, 5);
        assert!(p1 > p0 + 100, "{p0} -> {p1}");
        // The other contexts are untouched.
        assert_eq!(a.p(300, 6), p0);
        for _ in 0..200 {
            a.p(300, 5);
            a.update(0);
        }
        let p2 = a.p(300, 5);
        assert!(p2 < p0 - 100, "{p1} -> {p2}");
        // A context number is reduced modulo N.
        assert_eq!(a.p(300, 5 + 16), p2);
    }

    #[test]
    fn apm_stays_in_range() {
        let mut a = Apm::<64>::new(7);
        let mut s = 99u64;
        let sts = [-2047, -2000, -1000, -1, 0, 1, 777, 2000, 2047];
        for round in 0..3u32 {
            for i in 0..300_000u32 {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let st = if i % 2 == 0 { sts[(s % 9) as usize] } else { (s >> 8) as i32 % 2048 };
                let cx = (s >> 32) as usize;
                let p = a.p(st, cx);
                assert!((1..=4095).contains(&p), "{st} {cx}: {p}");
                // Runs of 0s, runs of 1s, then alternating bits.
                let bit = match round {
                    0 => 0,
                    1 => 1,
                    _ => i & 1,
                };
                a.update(bit);
            }
            for cx in 0..64 {
                for st in -2047..=2047 {
                    assert!((1..=4095).contains(&a.p(st, cx)), "{st} {cx}");
                }
                // Every map has learned the runs.
                let p = a.p(0, cx);
                assert!(round != 0 || p < 50, "{cx}: {p}");
                assert!(round != 1 || p > 4045, "{cx}: {p}");
            }
        }
    }
}
