//! Bit-level reading and writing in Deflate's order (least significant bit first).

/// Writes bits least-significant-first, as Deflate stores them.
pub struct BitWriter {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        BitWriter { out: Vec::new(), acc: 0, n: 0 }
    }

    pub fn with_capacity(cap: usize) -> Self {
        BitWriter { out: Vec::with_capacity(cap), acc: 0, n: 0 }
    }

    /// Writes the low `count` bits of `value` (count <= 32).
    #[inline]
    pub fn put(&mut self, value: u32, count: u32) {
        debug_assert!(count <= 32);
        debug_assert!(count == 32 || value >> count == 0);
        self.acc |= (value as u64) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Pads with zero bits to the next byte boundary.
    pub fn align(&mut self) {
        if self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc = 0;
            self.n = 0;
        }
    }

    /// Number of bits written so far.
    pub fn bit_len(&self) -> u64 {
        self.out.len() as u64 * 8 + self.n as u64
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}

impl Default for BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads bits least-significant-first. Reading past the end is an error (no silent zero padding),
/// so a truncated stream is always noticed.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // next byte to load into acc
    acc: u64,
    n: u32, // valid bits in acc
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndOfInput;

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, acc: 0, n: 0 }
    }

    #[inline]
    fn refill(&mut self) {
        while self.n <= 56 && self.pos < self.data.len() {
            self.acc |= (self.data[self.pos] as u64) << self.n;
            self.pos += 1;
            self.n += 8;
        }
    }

    /// Makes at least `count` bits available if the input has them.
    #[inline]
    pub fn need(&mut self, count: u32) -> Result<(), EndOfInput> {
        if self.n < count {
            self.refill();
            if self.n < count {
                return Err(EndOfInput);
            }
        }
        Ok(())
    }

    /// Bits available without reading past the end (up to 57).
    #[inline]
    pub fn available(&mut self) -> u32 {
        if self.n <= 56 {
            self.refill();
        }
        self.n
    }

    #[inline]
    pub fn peek(&self, count: u32) -> u32 {
        (self.acc & ((1u64 << count) - 1)) as u32
    }

    #[inline]
    pub fn consume(&mut self, count: u32) {
        debug_assert!(count <= self.n);
        self.acc >>= count;
        self.n -= count;
    }

    #[inline]
    pub fn get(&mut self, count: u32) -> Result<u32, EndOfInput> {
        if count == 0 {
            return Ok(0);
        }
        self.need(count)?;
        let v = self.peek(count);
        self.consume(count);
        Ok(v)
    }

    /// Drops the bits up to the next byte boundary.
    pub fn align(&mut self) {
        let drop = self.n % 8;
        self.consume(drop);
    }

    /// Byte offset just after the last bit consumed (after `align`, exact).
    pub fn byte_pos(&self) -> usize {
        self.pos - (self.n as usize / 8)
    }

    /// For stored blocks: after `align`, reads raw bytes.
    pub fn take_bytes(&mut self, len: usize) -> Result<&'a [u8], EndOfInput> {
        debug_assert!(self.n % 8 == 0);
        let start = self.byte_pos();
        let end = start.checked_add(len).ok_or(EndOfInput)?;
        if end > self.data.len() {
            return Err(EndOfInput);
        }
        self.acc = 0;
        self.n = 0;
        self.pos = end;
        Ok(&self.data[start..end])
    }
}
