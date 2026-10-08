//! Our context-mixing coder: a binary arithmetic coder driven by many small adaptive models whose
//! predictions are combined by trained mixers. Integer arithmetic only, so encoding and decoding give
//! identical results on every platform (native and WebAssembly).

pub mod model;

// ---------------------------------------------------------------- logistic helpers (12-bit domain)

const SQUASH_T: [i32; 33] = [
    1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901, 3975, 4024,
    4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094,
];

/// 1 / (1 + e^-x): x is ln-odds scaled by 256 (-2047..2047), result is a 12-bit probability.
#[inline]
pub fn squash(d: i32) -> i32 {
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

pub struct Tables {
    stretch: Box<[i16; 4096]>,
}

impl Tables {
    pub fn new() -> Tables {
        // stretch = inverse of squash.
        let mut stretch = Box::new([0i16; 4096]);
        let mut pi = 0usize;
        for x in -2047..=2047 {
            let v = squash(x) as usize;
            for j in pi..=v {
                stretch[j] = x as i16;
            }
            pi = v + 1;
        }
        for j in pi..4096 {
            stretch[j] = 2047;
        }
        Tables { stretch }
    }
    #[inline]
    pub fn stretch(&self, p: i32) -> i32 {
        self.stretch[p.clamp(0, 4095) as usize] as i32
    }
}

impl Default for Tables {
    fn default() -> Self {
        Self::new()
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

/// Maps a small context to an adaptive probability.
pub struct StateMap {
    t: Vec<Counter>,
    limit: u32,
    cx: usize,
}

impl StateMap {
    pub fn new(n: usize, limit: u32) -> StateMap {
        StateMap { t: vec![Counter::new(); n], limit, cx: 0 }
    }
    #[inline]
    pub fn p(&mut self, cx: usize) -> i32 {
        self.cx = cx;
        self.t[cx].p12()
    }
    #[inline]
    pub fn update(&mut self, bit: u32) {
        let l = self.limit;
        self.t[self.cx].update(bit, l);
    }
}

/// Weighted combination of stretched predictions, one weight set per selector context.
pub struct Mixer {
    pub shift: i32,
    n: usize,
    w: Vec<i32>,
    x: Vec<i32>,
    nx: usize,
    sel: usize,
    pr: i32,
    rate: i32,
}

impl Mixer {
    pub fn new(inputs: usize, selectors: usize, rate: i32) -> Mixer {
        // Inputs are stretched probabilities (at most 2047 either way) and the error at most 4095 x rate, so an
        // input times the error fits in 32 bits (see `update`).
        assert!((1..=256).contains(&rate));
        Mixer { shift: 10, n: inputs, w: vec![(1 << 16) / 4; inputs * selectors], x: vec![0; inputs], nx: 0, sel: 0, pr: 2048, rate }
    }
    #[inline]
    pub fn add(&mut self, st: i32) {
        debug_assert!(self.nx < self.n && st.abs() <= 2047);
        self.x[self.nx] = st;
        self.nx += 1;
    }
    /// Several inputs at once: the same as adding them one by one.
    #[inline]
    pub fn add_all(&mut self, st: &[i32]) {
        debug_assert!(st.iter().all(|x| x.abs() <= 2047));
        self.x[self.nx..self.nx + st.len()].copy_from_slice(st);
        self.nx += st.len();
    }
    #[inline]
    pub fn mix(&mut self, sel: usize) -> i32 {
        let n = self.n;
        // Inputs not given count as 0.
        if self.nx < n {
            self.x[self.nx..n].fill(0);
        }
        self.nx = n;
        self.sel = sel * n;
        let w = &self.w[self.sel..self.sel + n];
        // Integer sums: the order of the terms doesn't matter.
        let dot: i64 = self.x[..n].iter().zip(w).map(|(&x, &w)| x as i64 * w as i64).sum();
        let d = (dot >> 16).clamp(-2047, 2047) as i32;
        self.pr = squash(d);
        self.pr
    }
    #[inline]
    pub fn update(&mut self, bit: u32) {
        let err = (((bit as i32) << 12) - self.pr) * self.rate;
        let n = self.n;
        let sh = self.shift;
        // |x * err| <= 2047 * 4095 * 256 < 2^31: 32-bit arithmetic gives exactly the 64-bit result.
        for (w, &x) in self.w[self.sel..self.sel + n].iter_mut().zip(&self.x[..n]) {
            *w += (x * err + (1 << (sh - 1))) >> sh;
        }
        self.nx = 0;
    }
}

/// Adaptive probability map (secondary estimation): refines a probability within a context by
/// interpolating between 33 buckets over the stretched probability.
pub struct Apm {
    t: Vec<u16>,
    idx: usize,
    rate: i32,
}

impl Apm {
    pub fn new(contexts: usize, rate: i32) -> Apm {
        let mut t = vec![0u16; contexts * 33];
        for c in 0..contexts {
            for j in 0..33 {
                t[c * 33 + j] = (squash((j as i32 - 16) * 128) * 16) as u16;
            }
        }
        Apm { t, idx: 0, rate }
    }
    #[inline]
    pub fn p(&mut self, pr: i32, cx: usize, tables: &Tables) -> i32 {
        let s = tables.stretch(pr) + 2048;
        let lo = (s >> 7) as usize;
        let w = s & 127;
        self.idx = cx * 33 + lo;
        let a = self.t[self.idx] as i32;
        let b = self.t[(self.idx + 1).min(self.t.len() - 1)] as i32;
        if w >= 64 {
            self.idx += 1;
        }
        ((a * (128 - w) + b * w) >> 11).clamp(1, 4095)
    }
    #[inline]
    pub fn update(&mut self, bit: u32) {
        let target: i32 = if bit == 1 { 65535 } else { 0 };
        let v = self.t[self.idx] as i32;
        self.t[self.idx] = (v + ((target - v) >> self.rate)) as u16;
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
        let t = Tables::new();
        for p in (1..4095).step_by(7) {
            let back = squash(t.stretch(p));
            assert!((back - p).abs() <= 40, "{p} -> {back}");
        }
    }
}
