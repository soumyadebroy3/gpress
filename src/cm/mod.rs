//! Our context-mixing coder: a binary arithmetic coder driven by many small adaptive models whose
//! predictions are combined by trained mixers. Integer arithmetic only, so encoding and decoding give
//! identical results on every platform (native and WebAssembly).

pub mod model;

// ---------------------------------------------------------------- logistic helpers (12-bit domain)

const SQUASH_T: [i32; 33] = [
    1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901,
    3975, 4024, 4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094,
];

/// 1 / (1 + e^-x): x is ln-odds scaled by 256 (-2047..2047), result is a 12-bit probability.
#[inline]
pub const fn squash(d: i32) -> i32 {
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 1;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (SQUASH_T[i] * (128 - w) + SQUASH_T[i + 1] * w + 64) >> 7
}

/// `squash` of every value of -2047..2047, worked out when compiling.
static SQUASH: [u16; 4095] = {
    let mut t = [0u16; 4095];
    let mut i = 0;
    while i < 4095 {
        t[i] = squash(i as i32 - 2047) as u16;
        i += 1;
    }
    t
};

/// `stretch` of every 12-bit probability: the inverse of `squash`, worked out when compiling.
static STRETCH: [i16; 4096] = {
    let mut t = [0i16; 4096];
    let mut pi = 0usize;
    let mut x = -2047;
    while x <= 2047 {
        let v = squash(x) as usize;
        while pi <= v {
            t[pi] = x as i16;
            pi += 1;
        }
        x += 1;
    }
    while pi < 4096 {
        t[pi] = 2047;
        pi += 1;
    }
    t
};

/// ln(p / (1 - p)) scaled by 256 (-2047..2047) of a 12-bit probability: the inverse of `squash`.
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

/// Adaptive probability map (secondary estimation): refines a probability within a context (below `N`, a
/// power of two) by interpolating between 33 buckets over the stretched probability.
pub struct Apm<const N: usize> {
    t: Box<[[u16; 33]; N]>,
    row: usize,
    j: usize,
    rate: i32,
}

impl<const N: usize> Apm<N> {
    pub fn new(rate: i32) -> Apm<N> {
        const { assert!(N.is_power_of_two()) };
        let mut r = [0u16; 33];
        for (j, v) in r.iter_mut().enumerate() {
            *v = (squash((j as i32 - 16) * 128) * 16) as u16;
        }
        Apm { t: boxed(r), row: 0, j: 0, rate }
    }
    /// `st` is the stretched probability (-2047..2047).
    #[inline]
    pub fn p(&mut self, st: i32, cx: usize) -> i32 {
        let s = st + 2048;
        // s is 1..4095 and cx below N: the masks change nothing but spare the bounds checks.
        let lo = (s >> 7) as usize & 31;
        let w = s & 127;
        self.row = cx & (N - 1);
        let r = &self.t[self.row];
        let a = r[lo] as i32;
        let b = r[lo + 1] as i32;
        self.j = lo + (w >= 64) as usize;
        ((a * (128 - w) + b * w) >> 11).clamp(1, 4095)
    }
    #[inline]
    pub fn update(&mut self, bit: u32) {
        let target: i32 = if bit == 1 { 65535 } else { 0 };
        // The bucket is at most 32 (see `p`): the minimum changes nothing.
        let v = &mut self.t[self.row & (N - 1)][self.j.min(32)];
        *v = (*v as i32 + ((target - *v as i32) >> self.rate)) as u16;
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
    fn stretch_table_is_the_one_built_at_run_time() {
        // The loop that made the table when the program started, before the table was made when compiling.
        let mut t = [0i16; 4096];
        let mut pi = 0usize;
        for x in -2047..=2047 {
            let v = squash(x) as usize;
            for e in t.iter_mut().take(v + 1).skip(pi) {
                *e = x as i16;
            }
            pi = v + 1;
        }
        for e in t.iter_mut().skip(pi) {
            *e = 2047;
        }
        assert!(t == STRETCH);
    }

    #[test]
    fn squash_table_is_squash() {
        for d in -2047..=2047 {
            assert_eq!(SQUASH[(d + 2047) as usize] as i32, squash(d), "{d}");
        }
    }

    #[test]
    fn squash_stretch_are_inverse_enough() {
        for p in (1..4095).step_by(7) {
            let back = squash(stretch(p));
            assert!((back - p).abs() <= 40, "{p} -> {back}");
        }
    }
}
