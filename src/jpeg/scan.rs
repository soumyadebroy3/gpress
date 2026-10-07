//! Entropy-coded data of a sequential Huffman scan, read as its symbols ("tokens") and written back with
//! other Huffman codes. The tokens are kept exactly: DC difference categories with their extra bits, AC
//! run/size symbols with their extra bits, end-of-block and zero-run symbols, and the restart markers. So the
//! coefficients any decoder gets are the same.

use super::huffman::DecodeTable;
use super::parse::{Fail, Jpeg, Scan};

/// Token slot of a restart marker (Huffman slots are 0-3 for DC tables, 4-7 for AC tables).
pub const RESTART: u8 = 8;

/// One coded symbol with its extra bits, or a restart marker (slot RESTART, `sym` its number 0-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tok {
    pub slot: u8,
    pub sym: u8,
    pub nbits: u8,
    pub bits: u16,
}

/// Reads entropy-coded bits: stuffed 0xFF 0x00 is a 0xFF data byte; reading stops at any other marker.
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    end: usize,
    acc: u64,
    n: u32,
    at_marker: bool,
}

impl<'a> Bits<'a> {
    #[inline]
    fn refill(&mut self) {
        while self.n <= 56 && !self.at_marker {
            if self.pos >= self.end {
                self.at_marker = true;
                break;
            }
            let b = self.d[self.pos];
            if b == 0xFF {
                if self.pos + 1 < self.end && self.d[self.pos + 1] == 0 {
                    self.pos += 2;
                } else {
                    self.at_marker = true;
                    break;
                }
            } else {
                self.pos += 1;
            }
            self.acc = self.acc << 8 | b as u64;
            self.n += 8;
        }
    }

    #[inline]
    fn peek16(&mut self) -> u32 {
        if self.n < 16 {
            self.refill();
        }
        if self.n >= 16 {
            (self.acc >> (self.n - 16)) as u32 & 0xFFFF
        } else {
            (self.acc << (16 - self.n)) as u32 & 0xFFFF
        }
    }

    #[inline]
    fn symbol(&mut self, t: &DecodeTable) -> Result<u8, Fail> {
        let peek = self.peek16();
        let (len, sym) = t.lookup(peek).ok_or("bad Huffman code")?;
        if len > self.n {
            return Err("entropy-coded data ends early");
        }
        self.n -= len;
        Ok(sym)
    }

    #[inline]
    fn get(&mut self, k: u32) -> Result<u16, Fail> {
        if k == 0 {
            return Ok(0);
        }
        if self.n < k {
            self.refill();
            if self.n < k {
                return Err("entropy-coded data ends early");
            }
        }
        self.n -= k;
        Ok((self.acc >> self.n) as u16 & ((1u32 << k) - 1) as u16)
    }

    /// End of a restart interval or of the scan: the rest of the current byte is padding, and no data byte may
    /// follow before the next marker.
    fn end_of_interval(&mut self) -> Result<(), Fail> {
        self.n -= self.n % 8;
        if self.n == 0 {
            self.refill();
        }
        if self.n != 0 {
            return Err("extra entropy-coded data");
        }
        Ok(())
    }

    /// Restart marker `m` (and nothing else) at the current position.
    fn restart_marker(&mut self, m: u8) -> bool {
        if self.pos + 2 <= self.end && self.d[self.pos] == 0xFF && self.d[self.pos + 1] == 0xD0 + m {
            self.pos += 2;
            self.at_marker = false;
            self.acc = 0;
            return true;
        }
        false
    }
}

/// The tokens of one scan, in order.
pub struct Tokens<'a> {
    bits: Bits<'a>,
    tables: [Option<&'a DecodeTable>; 8],
    /// (DC slot, AC slot) of each block of an MCU.
    blocks: Vec<(u8, u8)>,
    mcus: u64,
    restart: u64,
    mcu: u64,
    blk: usize,
    /// Next coefficient of the current block: 0 is DC, 64 means the block is complete.
    k: u32,
    rst: u8,
    done: bool,
}

impl<'a> Tokens<'a> {
    pub fn new(j: &'a Jpeg<'a>, s: &Scan) -> Result<Tokens<'a>, Fail> {
        let f = &j.frame;
        let (blocks, mcus): (Vec<(u8, u8)>, u64) = if s.comps.len() == 1 {
            // Non-interleaved: the component's own blocks, one per MCU.
            let (ci, td, ta) = s.comps[0];
            let c = f.comps[ci];
            let w = (f.width as u64 * c.h as u64).div_ceil(8 * f.hmax as u64);
            let h = (f.height as u64 * c.v as u64).div_ceil(8 * f.vmax as u64);
            (vec![(td, 4 + ta)], w * h)
        } else {
            let mut b = Vec::new();
            for &(ci, td, ta) in &s.comps {
                let c = f.comps[ci];
                for _ in 0..c.h as usize * c.v as usize {
                    b.push((td, 4 + ta));
                }
            }
            let w = (f.width as u64).div_ceil(8 * f.hmax as u64);
            let h = (f.height as u64).div_ceil(8 * f.vmax as u64);
            (b, w * h)
        };
        let mut tables = [None; 8];
        for (slot, t) in s.tables.iter().enumerate() {
            if let Some(i) = t {
                tables[slot] = Some(j.tables.get(*i).ok_or("bad table index")?);
            }
        }
        for &(dc, ac) in &blocks {
            if tables[dc as usize].is_none() || tables[ac as usize].is_none() {
                return Err("scan uses an undefined Huffman table");
            }
        }
        let bits = Bits { d: j.data, pos: s.data.start, end: s.data.end, acc: 0, n: 0, at_marker: false };
        Ok(Tokens { bits, tables, blocks, mcus, restart: s.restart as u64, mcu: 0, blk: 0, k: 0, rst: 0, done: false })
    }

    /// The next token; None after the last one (when the data has been checked to end exactly there).
    pub fn next(&mut self) -> Result<Option<Tok>, Fail> {
        if self.done {
            return Ok(None);
        }
        if self.k >= 64 {
            self.k = 0;
            self.blk += 1;
            if self.blk == self.blocks.len() {
                self.blk = 0;
                self.mcu += 1;
                if self.mcu == self.mcus {
                    self.done = true;
                    self.bits.end_of_interval()?;
                    // Some encoders end the last interval with a restart marker too (the next one in turn);
                    // it is kept, as a last token.
                    let m = self.rst;
                    if self.restart > 0 && self.bits.pos + 2 == self.bits.end && self.bits.restart_marker(m) {
                        return Ok(Some(Tok { slot: RESTART, sym: m, nbits: 0, bits: 0 }));
                    }
                    if self.bits.pos != self.bits.end {
                        return Err("marker inside the scan's data");
                    }
                    return Ok(None);
                }
                if self.restart > 0 && self.mcu % self.restart == 0 {
                    let m = self.rst;
                    self.rst = (m + 1) & 7;
                    self.bits.end_of_interval()?;
                    if !self.bits.restart_marker(m) {
                        return Err("restart marker missing or out of order");
                    }
                    return Ok(Some(Tok { slot: RESTART, sym: m, nbits: 0, bits: 0 }));
                }
            }
        }
        let (dc, ac) = self.blocks[self.blk];
        if self.k == 0 {
            let t = self.tables[dc as usize].ok_or("no table")?;
            let s = self.bits.symbol(t)?;
            if s > 11 {
                return Err("DC difference too large for 8-bit samples");
            }
            let bits = self.bits.get(s as u32)?;
            self.k = 1;
            return Ok(Some(Tok { slot: dc, sym: s, nbits: s, bits }));
        }
        let t = self.tables[ac as usize].ok_or("no table")?;
        let rs = self.bits.symbol(t)?;
        let (r, s) = ((rs >> 4) as u32, (rs & 15) as u32);
        if s == 0 {
            match r {
                0 => self.k = 64, // end of block
                15 => {
                    // Sixteen zeros; ending the block exactly is fine, running past it is not.
                    if self.k + 16 > 64 {
                        return Err("zero run past the end of a block");
                    }
                    self.k += 16;
                }
                _ => return Err("unusual AC symbol"),
            }
            return Ok(Some(Tok { slot: ac, sym: rs, nbits: 0, bits: 0 }));
        }
        if s > 10 {
            return Err("AC coefficient too large for 8-bit samples");
        }
        if self.k + r > 63 {
            return Err("coefficient past the end of a block");
        }
        let bits = self.bits.get(s)?;
        self.k += r + 1;
        Ok(Some(Tok { slot: ac, sym: rs, nbits: s as u8, bits }))
    }
}

/// Writes bits most significant first, with a 0x00 after every 0xFF byte.
pub struct BitWriter<'a> {
    pub out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
}

impl<'a> BitWriter<'a> {
    pub fn new(out: &'a mut Vec<u8>) -> Self {
        BitWriter { out, acc: 0, n: 0 }
    }

    #[inline]
    pub fn put(&mut self, v: u32, len: u32) {
        self.acc = self.acc << len | v as u64;
        self.n += len;
        while self.n >= 8 {
            self.n -= 8;
            let b = (self.acc >> self.n) as u8;
            self.out.push(b);
            if b == 0xFF {
                self.out.push(0);
            }
        }
    }

    /// Fills the last byte with one bits, as the standard asks.
    pub fn pad(&mut self) {
        if self.n > 0 {
            let k = 8 - self.n;
            self.put((1 << k) - 1, k);
        }
        self.acc = 0;
    }
}
