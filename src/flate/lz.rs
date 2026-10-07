//! LZ77 symbols and their statistics.

use super::tables::*;

/// One Deflate symbol: a literal (`dist == 0`, `litlen` = byte) or a match (`litlen` = length 3..=258, `dist` 1..=32768).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sym {
    pub litlen: u16,
    pub dist: u16,
}

impl Sym {
    #[inline]
    pub fn lit(b: u8) -> Sym {
        Sym { litlen: b as u16, dist: 0 }
    }
    #[inline]
    pub fn mat(len: usize, dist: usize) -> Sym {
        debug_assert!((MIN_MATCH..=MAX_MATCH).contains(&len) && (1..=WINDOW).contains(&dist));
        Sym { litlen: len as u16, dist: dist as u16 }
    }
    /// Bytes this symbol produces.
    #[inline]
    pub fn span(&self) -> usize {
        if self.dist == 0 {
            1
        } else {
            self.litlen as usize
        }
    }
    /// Literal/length alphabet symbol.
    #[inline]
    pub fn ll_sym(&self) -> usize {
        if self.dist == 0 {
            self.litlen as usize
        } else {
            257 + LEN_CODE[self.litlen as usize] as usize
        }
    }
    /// Distance alphabet symbol (only for matches).
    #[inline]
    pub fn d_sym(&self) -> usize {
        dist_code(self.dist as usize)
    }
}

/// Symbol counts of a block; the end-of-block symbol is counted once.
#[derive(Clone)]
pub struct Hist {
    pub ll: [u32; LITLEN_SYMS],
    pub d: [u32; DIST_SYMS],
}

impl Hist {
    pub fn empty() -> Hist {
        let mut h = Hist { ll: [0; LITLEN_SYMS], d: [0; DIST_SYMS] };
        h.ll[END_OF_BLOCK] = 1;
        h
    }

    pub fn of(syms: &[Sym]) -> Hist {
        let mut h = Hist::empty();
        for s in syms {
            h.add(s);
        }
        h
    }

    #[inline]
    pub fn add(&mut self, s: &Sym) {
        self.ll[s.ll_sym()] += 1;
        if s.dist != 0 {
            self.d[s.d_sym()] += 1;
        }
    }

    /// `self - other + end-of-block`, for prefix sums.
    pub fn diff(a: &Hist, b: &Hist) -> Hist {
        let mut h = Hist::empty();
        for i in 0..LITLEN_SYMS {
            if i != END_OF_BLOCK {
                h.ll[i] = a.ll[i] - b.ll[i];
            }
        }
        for i in 0..DIST_SYMS {
            h.d[i] = a.d[i] - b.d[i];
        }
        h
    }
}

/// Extra bits that follow each literal/length symbol.
pub fn ll_extra(sym: usize) -> u32 {
    if sym >= 257 {
        LEN_EXTRA[sym - 257] as u32
    } else {
        0
    }
}

pub fn d_extra(sym: usize) -> u32 {
    DIST_EXTRA[sym] as u32
}
