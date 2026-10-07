//! The restore check of `pack`, run next to the encoder: the decoder reads the encoder's bytes while they are
//! written (handed over in chunks through a channel), so the check costs little extra time when a second
//! thread is free. It is the same check as `unpack`: the same `run`, on the same bytes.

use crate::cm::{Coder, Encoder};
use std::sync::mpsc::{Receiver, Sender};

/// Bytes handed to the decoder at a time.
const CHUNK: usize = 1 << 14;

/// The arithmetic encoder, also sending what it has written to a decoder.
pub struct TeeEncoder {
    enc: Encoder,
    tx: Sender<Vec<u8>>,
    sent: usize,
}

impl TeeEncoder {
    pub fn new(tx: Sender<Vec<u8>>) -> TeeEncoder {
        TeeEncoder { enc: Encoder::new(), tx, sent: 0 }
    }

    #[inline]
    fn pass_on(&mut self) {
        if self.enc.out.len() - self.sent >= CHUNK {
            // A decoder that has stopped (a damaged result) no longer listens; the check fails then anyway.
            let _ = self.tx.send(self.enc.out[self.sent..].to_vec());
            self.sent = self.enc.out.len();
        }
    }

    /// The coded bytes; the decoder gets the rest and then the end of the channel.
    pub fn finish(self) -> Vec<u8> {
        let TeeEncoder { enc, tx, sent } = self;
        let out = enc.finish();
        let _ = tx.send(out[sent..].to_vec());
        out
    }
}

impl Coder for TeeEncoder {
    #[inline]
    fn code(&mut self, bit: Option<u32>, p1: i32) -> u32 {
        let b = self.enc.code(bit, p1);
        self.pass_on();
        b
    }
    #[inline]
    fn code16(&mut self, bit: Option<u32>, p16: u32) -> u32 {
        let b = self.enc.code16(bit, p16);
        self.pass_on();
        b
    }
}

/// The arithmetic decoder of `cm::Decoder`, reading its bytes from a channel as they arrive. Past the end of
/// the data it reads zeros and counts them, exactly like `cm::Decoder` on the whole data.
pub struct StreamDecoder {
    x1: u32,
    x2: u32,
    x: u32,
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    at: usize,
    /// Bytes read so far (zeros past the end included), and bytes received.
    pos: usize,
    received: usize,
    closed: bool,
}

impl StreamDecoder {
    pub fn new(rx: Receiver<Vec<u8>>) -> StreamDecoder {
        let mut d = StreamDecoder { x1: 0, x2: 0xffff_ffff, x: 0, rx, buf: Vec::new(), at: 0, pos: 0, received: 0, closed: false };
        for _ in 0..4 {
            d.x = (d.x << 8) | d.next() as u32;
        }
        d
    }

    #[inline]
    fn next(&mut self) -> u8 {
        while self.at >= self.buf.len() && !self.closed {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.received += chunk.len();
                    self.buf = chunk;
                    self.at = 0;
                }
                Err(_) => self.closed = true,
            }
        }
        self.pos += 1;
        if self.at < self.buf.len() {
            self.at += 1;
            self.buf[self.at - 1]
        } else {
            0
        }
    }

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

impl Coder for StreamDecoder {
    /// Read well past the end of everything the encoder sent: only a damaged stream gets here.
    fn exhausted(&self) -> bool {
        self.closed && self.pos > self.received + 4
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
        self.code_at(xmid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cm::Decoder;

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// Bits with 12-bit and 16-bit probabilities, some of them badly predicted.
    fn items(seed: u64, n: usize) -> Vec<(bool, u32, u32)> {
        let mut s = seed;
        (0..n)
            .map(|i| {
                let r = rng(&mut s);
                if i % 3 == 0 {
                    let p = 65535 - (r % 64) as u32;
                    (true, p, ((r >> 24) % 65536 < p as u64) as u32)
                } else {
                    let p = (r % 4095) as u32 + 1;
                    (false, p, ((r >> 20) % 4096 < p as u64 || i % 17 == 0) as u32)
                }
            })
            .collect()
    }

    #[test]
    fn decodes_while_the_encoder_writes_and_agrees_with_the_plain_decoder() {
        for (seed, n) in [(1u64, 0usize), (2, 10), (3, 400_000)] {
            let it = items(seed, n);
            let (tx, rx) = std::sync::mpsc::channel();
            let (bytes, got) = std::thread::scope(|s| {
                let check = s.spawn(|| {
                    let mut d = StreamDecoder::new(rx);
                    let bits: Vec<u32> = it.iter().map(|&(wide, p, _)| if wide { d.code16(None, p) } else { d.code(None, p as i32) }).collect();
                    (bits, d.exhausted())
                });
                let mut e = TeeEncoder::new(tx);
                for &(wide, p, b) in &it {
                    if wide {
                        e.code16(Some(b), p);
                    } else {
                        e.code(Some(b), p as i32);
                    }
                }
                (e.finish(), check.join().unwrap())
            });
            let want: Vec<u32> = it.iter().map(|x| x.2).collect();
            assert_eq!(got.0, want);
            assert!(!got.1);
            // The plain decoder on the same bytes gives the same bits.
            let mut d = Decoder::new(&bytes);
            let plain: Vec<u32> = it.iter().map(|&(wide, p, _)| if wide { d.code16(None, p) } else { d.code(None, p as i32) }).collect();
            assert_eq!(plain, want);
        }
    }

    #[test]
    fn a_cut_stream_ends_in_exhaustion_like_the_plain_decoder() {
        let it = items(9, 50_000);
        let mut e = Encoder::new();
        for &(wide, p, b) in &it {
            if wide {
                e.code16(Some(b), p);
            } else {
                e.code(Some(b), p as i32);
            }
        }
        let bytes = e.finish();
        let cut = &bytes[..bytes.len() / 2];
        let (tx, rx) = std::sync::mpsc::channel();
        for c in cut.chunks(1000) {
            tx.send(c.to_vec()).unwrap();
        }
        drop(tx);
        let mut sd = StreamDecoder::new(rx);
        let mut pd = Decoder::new(cut);
        for &(wide, p, _) in &it {
            let (a, b) = if wide { (sd.code16(None, p), pd.code16(None, p)) } else { (sd.code(None, p as i32), pd.code(None, p as i32)) };
            assert_eq!(a, b);
            assert_eq!(sd.exhausted(), pd.exhausted());
        }
        assert!(sd.exhausted());
    }
}
