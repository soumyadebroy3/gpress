//! Entropy-coded data of a sequential Huffman scan, read as its symbols ("tokens") and written back with
//! other Huffman codes. The tokens are kept exactly: DC difference categories with their extra bits, AC
//! run/size symbols with their extra bits, end-of-block and zero-run symbols, and the restart markers. So the
//! coefficients any decoder gets are the same.

use super::huffman::DecodeTable;
use super::parse::{Fail, Jpeg, Scan};

/// Token slot of a restart marker (Huffman slots are 0-3 for DC tables, 4-7 for AC tables).
pub const RESTART: u8 = 8;

/// Most tokens of one block: the DC token and at most 63 AC tokens (each moves on at least one coefficient,
/// or ends the block).
pub const BLOCK_TOKENS: usize = 64;

/// Most blocks decoded in one call of [`Tokens::next`].
pub const BATCH_BLOCKS: usize = 64;

/// Room for the tokens of one call of [`Tokens::next`].
pub type Buffer = [Tok; BATCH_BLOCKS * BLOCK_TOKENS];

/// One coded symbol with its extra bits, or a restart marker (slot RESTART, symbol its number 0-7), in one
/// word: slot (4 bits), symbol (8), number of extra bits (4), extra bits (16).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tok(u32);

impl Tok {
    #[inline]
    pub fn new(slot: u8, sym: u8, nbits: u8, bits: u16) -> Tok {
        Tok((slot as u32) << 28 | (sym as u32) << 20 | (nbits as u32) << 16 | bits as u32)
    }
    #[inline]
    pub fn slot(self) -> u8 {
        (self.0 >> 28) as u8
    }
    #[inline]
    pub fn sym(self) -> u8 {
        (self.0 >> 20) as u8
    }
    #[inline]
    pub fn nbits(self) -> u8 {
        (self.0 >> 16 & 15) as u8
    }
    #[inline]
    pub fn bits(self) -> u16 {
        self.0 as u16
    }
}

/// True if one of the 8 bytes of `w` is 0xFF.
#[inline]
fn has_ff(w: u64) -> bool {
    const LOW: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    (!w).wrapping_sub(LOW) & w & HIGH != 0
}

/// Reads entropy-coded bits: stuffed 0xFF 0x00 is a 0xFF data byte; reading stops at any other marker.
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    end: usize,
    /// The next `n` bits are the low bits of `acc`, most significant first.
    acc: u64,
    n: u32,
    at_marker: bool,
}

impl<'a> Bits<'a> {
    #[inline]
    fn refill(&mut self) {
        // Eight bytes without a 0xFF need no checks: take as many as fit.
        if !self.at_marker && self.n <= 56 && self.pos + 8 <= self.end {
            let mut b = [0u8; 8];
            b.copy_from_slice(&self.d[self.pos..self.pos + 8]);
            let w = u64::from_be_bytes(b);
            if !has_ff(w) {
                let take = (64 - self.n) / 8; // 1 to 8 bytes
                self.acc = if take == 8 { w } else { self.acc << (8 * take) | w >> (64 - 8 * take) };
                self.n += 8 * take;
                self.pos += take as usize;
                return;
            }
        }
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

/// The tokens of one scan, in order, some blocks (or a restart marker) at a time.
pub struct Tokens<'a> {
    bits: Bits<'a>,
    /// (DC table, AC table, DC slot, AC slot) of each block of an MCU.
    blocks: Vec<(&'a DecodeTable, &'a DecodeTable, u8, u8)>,
    mcus: u64,
    restart: u64,
    mcu: u64,
    /// Next block of the current MCU.
    blk: usize,
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
        // Every block takes at least 2 bits (a DC code and an end-of-block code, 1 bit each at the least): a
        // frame claiming more blocks than the data can hold is refused before any work.
        if mcus * blocks.len() as u64 > 4 * s.data.len() as u64 {
            return Err("more blocks than the data can hold");
        }
        let table = |slot: u8| -> Result<&'a DecodeTable, Fail> {
            let i = s.tables[slot as usize].ok_or("scan uses an undefined Huffman table")?;
            j.tables.get(i).ok_or("bad table index")
        };
        let blocks =
            blocks.iter().map(|&(dc, ac)| Ok((table(dc)?, table(ac)?, dc, ac))).collect::<Result<Vec<_>, Fail>>()?;
        let bits = Bits { d: j.data, pos: s.data.start, end: s.data.end, acc: 0, n: 0, at_marker: false };
        Ok(Tokens { bits, blocks, mcus, restart: s.restart as u64, mcu: 0, blk: 0, rst: 0, done: false })
    }

    /// A buffer for [`Tokens::next`].
    pub fn buffer() -> Box<Buffer> {
        Box::new([Tok::default(); BATCH_BLOCKS * BLOCK_TOKENS])
    }

    /// The tokens of the next blocks (at most BATCH_BLOCKS, never past the end of a restart interval), or the
    /// token of a restart marker, into the start of `out`: how many there are; 0 after the last token, when the
    /// data has been checked to end exactly there. Two scans with the same frame and scan header come in the
    /// same batches.
    pub fn next(&mut self, out: &mut Buffer) -> Result<usize, Fail> {
        if self.done {
            return Ok(0);
        }
        if self.blk == self.blocks.len() {
            self.blk = 0;
            self.mcu += 1;
            if self.mcu == self.mcus {
                self.done = true;
                self.bits.end_of_interval()?;
                // Some encoders end the last interval with a restart marker too (the next one in turn); it is
                // kept, as a last token.
                let m = self.rst;
                if self.restart > 0 && self.bits.pos + 2 == self.bits.end && self.bits.restart_marker(m) {
                    out[0] = Tok::new(RESTART, m, 0, 0);
                    return Ok(1);
                }
                if self.bits.pos != self.bits.end {
                    return Err("marker inside the scan's data");
                }
                return Ok(0);
            }
            if self.restart > 0 && self.mcu.is_multiple_of(self.restart) {
                let m = self.rst;
                self.rst = (m + 1) & 7;
                self.bits.end_of_interval()?;
                if !self.bits.restart_marker(m) {
                    return Err("restart marker missing or out of order");
                }
                out[0] = Tok::new(RESTART, m, 0, 0);
                return Ok(1);
            }
        }
        let (mut count, mut n) = (0, 0);
        loop {
            let (tdc, tac, dc, ac) = self.blocks[self.blk];
            n = self.block(tdc, tac, dc, ac, out, n)?;
            self.blk += 1;
            count += 1;
            if self.blk == self.blocks.len() {
                // The end of the scan or of an interval is for the next call.
                let next = self.mcu + 1;
                if count >= BATCH_BLOCKS || next == self.mcus || (self.restart > 0 && next.is_multiple_of(self.restart))
                {
                    return Ok(n);
                }
                self.blk = 0;
                self.mcu = next;
            } else if count >= BATCH_BLOCKS {
                return Ok(n);
            }
        }
    }

    /// One block into `out` from position `n`: the DC token, then AC tokens up to the end-of-block symbol or the
    /// 64th coefficient. Returns the position after it (at most 64 more).
    #[inline]
    fn block(
        &mut self,
        tdc: &DecodeTable,
        tac: &DecodeTable,
        dc: u8,
        ac: u8,
        out: &mut Buffer,
        mut n: usize,
    ) -> Result<usize, Fail> {
        let s = self.bits.symbol(tdc)?;
        if s > 11 {
            return Err("DC difference too large for 8-bit samples");
        }
        let bits = self.bits.get(s as u32)?;
        out[n] = Tok::new(dc, s, s, bits);
        n += 1;
        // Next coefficient; each token moves it on by at least one, or ends the block.
        let mut k = 1u32;
        while k < 64 {
            let rs = self.bits.symbol(tac)?;
            let (r, s) = ((rs >> 4) as u32, (rs & 15) as u32);
            if s == 0 {
                match r {
                    0 => {
                        out[n] = Tok::new(ac, rs, 0, 0);
                        return Ok(n + 1); // end of block
                    }
                    15 => {
                        // Sixteen zeros; ending the block exactly is fine, running past it is not.
                        if k + 16 > 64 {
                            return Err("zero run past the end of a block");
                        }
                        k += 16;
                        out[n] = Tok::new(ac, rs, 0, 0);
                        n += 1;
                        continue;
                    }
                    _ => return Err("unusual AC symbol"),
                }
            }
            if s > 10 {
                return Err("AC coefficient too large for 8-bit samples");
            }
            if k + r > 63 {
                return Err("coefficient past the end of a block");
            }
            let bits = self.bits.get(s)?;
            out[n] = Tok::new(ac, rs, s as u8, bits);
            n += 1;
            k += r + 1;
        }
        Ok(n)
    }
}

/// Where the bytes of a [`BitWriter`] go.
pub trait Sink {
    fn byte(&mut self, b: u8);
    fn marker(&mut self, m: u8);
}

/// Counts the 0xFF bytes written (each needs a stuffed 0x00), writing nothing.
#[derive(Default)]
pub struct CountFF(pub u64);

impl Sink for CountFF {
    #[inline]
    fn byte(&mut self, b: u8) {
        self.0 += (b == 0xFF) as u64;
    }
    fn marker(&mut self, _: u8) {}
}

/// Writes bits most significant first, with a 0x00 after every 0xFF byte.
pub struct BitWriter<S: Sink> {
    pub out: S,
    acc: u64,
    n: u32,
}

impl<S: Sink> BitWriter<S> {
    pub fn new(out: S) -> Self {
        BitWriter { out, acc: 0, n: 0 }
    }

    /// `len` (at most 32) bits of `v`.
    #[inline]
    pub fn put(&mut self, v: u32, len: u32) {
        self.acc = self.acc << len | v as u64;
        self.n += len;
        while self.n >= 8 {
            self.n -= 8;
            let b = (self.acc >> self.n) as u8;
            self.out.byte(b);
            if b == 0xFF {
                self.out.byte(0);
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

    /// Restart marker `m`, after padding.
    pub fn restart(&mut self, m: u8) {
        self.pad();
        self.out.marker(0xD0 + m);
    }
}
