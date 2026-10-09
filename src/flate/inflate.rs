//! Our own Deflate decoder (RFC 1951). It accepts exactly what zlib accepts for complete streams and reports
//! everything else as an error, so the engine never "repairs" a stream silently.

use super::bits::{BitReader, EndOfInput};
use super::tables::*;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InflateError {
    /// The input ended before the final block did.
    Truncated,
    BadBlockType,
    StoredLength,
    /// A code table that zlib would refuse (over-subscribed, incomplete, too many symbols, no end-of-block).
    BadTable(&'static str),
    /// A symbol that cannot occur (literal/length 286-287, distance 30-31, or no code matched).
    BadSymbol,
    /// A distance pointing before the start of the output.
    TooFarBack,
    /// Output grew past the caller's limit.
    TooLarge,
}

impl From<EndOfInput> for InflateError {
    fn from(_: EndOfInput) -> Self {
        InflateError::Truncated
    }
}

#[derive(Debug)]
pub struct Inflated {
    pub data: Vec<u8>,
    /// Input bytes used, up to and including the byte holding the final block's last bit.
    pub consumed: usize,
}

const FAST_BITS: u32 = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Codes,
    Lens,
    Dists,
}

struct Decoder {
    /// Index by the next FAST_BITS input bits: (symbol << 4) | length, or 0 when the code is longer.
    fast: Vec<u32>,
    count: [u16; 16],
    sorted: Vec<u16>,
    empty: bool,
}

impl Decoder {
    fn new(lens: &[u8], kind: Kind) -> Result<Decoder, InflateError> {
        let mut count = [0u16; 16];
        for &l in lens {
            count[l as usize] += 1;
        }
        count[0] = 0;
        let max = (1..16).rev().find(|&b| count[b] != 0).unwrap_or(0);
        let mut fast = vec![0u32; 1 << FAST_BITS];
        if max == 0 {
            return Ok(Decoder { fast, count, sorted: Vec::new(), empty: true });
        }
        let mut left: i32 = 1;
        for b in 1..16 {
            left <<= 1;
            left -= count[b] as i32;
            if left < 0 {
                return Err(InflateError::BadTable("over-subscribed code"));
            }
        }
        if left > 0 && (kind == Kind::Codes || max != 1) {
            return Err(InflateError::BadTable("incomplete code"));
        }
        // Symbols ordered by (length, symbol): the canonical order.
        let mut offs = [0u16; 16];
        for b in 1..15 {
            offs[b + 1] = offs[b] + count[b];
        }
        let total: usize = count.iter().map(|&c| c as usize).sum();
        let mut sorted = vec![0u16; total];
        for (sym, &l) in lens.iter().enumerate() {
            if l != 0 {
                sorted[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        // Fast table for codes up to FAST_BITS long.
        let mut code: u32 = 0;
        let mut idx = 0usize;
        for len in 1..16u32 {
            for _ in 0..count[len as usize] {
                let sym = sorted[idx] as u32;
                idx += 1;
                if len <= FAST_BITS {
                    let rev = (code.reverse_bits() >> (32 - len)) as usize;
                    let step = 1usize << len;
                    let mut k = rev;
                    while k < fast.len() {
                        fast[k] = (sym << 4) | len;
                        k += step;
                    }
                }
                code += 1;
            }
            code <<= 1;
        }
        Ok(Decoder { fast, count, sorted, empty: false })
    }

    #[inline]
    fn decode(&self, br: &mut BitReader) -> Result<usize, InflateError> {
        if self.empty {
            return Err(InflateError::BadSymbol);
        }
        let avail = br.available();
        if avail == 0 {
            return Err(InflateError::Truncated);
        }
        // Near the end fewer than FAST_BITS bits may be left; the missing high bits read as zero, which is
        // harmless because an entry only depends on its code's own `len` low bits, checked against `avail`.
        let e = self.fast[br.peek(FAST_BITS.min(avail)) as usize];
        let len = e & 15;
        if len != 0 {
            if len > avail {
                return Err(InflateError::Truncated);
            }
            br.consume(len);
            return Ok((e >> 4) as usize);
        }
        self.slow(br)
    }

    fn slow(&self, br: &mut BitReader) -> Result<usize, InflateError> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= br.get(1)? as i32;
            let c = self.count[len] as i32;
            if code - first < c {
                return Ok(self.sorted[(index + code - first) as usize] as usize);
            }
            index += c;
            first += c;
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::BadSymbol)
    }
}

fn fixed_decoders() -> &'static (Decoder, Decoder) {
    static FIXED: OnceLock<(Decoder, Decoder)> = OnceLock::new();
    FIXED.get_or_init(|| {
        let ll = fixed_litlen_lengths();
        let d = fixed_dist_lengths();
        (Decoder::new(&ll, Kind::Lens).unwrap(), Decoder::new(&d, Kind::Dists).unwrap())
    })
}

/// Inflates a raw Deflate stream. `limit` caps the output size (protection against decompression bombs).
pub fn inflate(input: &[u8], limit: usize) -> Result<Inflated, InflateError> {
    let mut br = BitReader::new(input);
    let mut out: Vec<u8> = Vec::with_capacity((input.len() * 4).min(limit).min(1 << 26));
    loop {
        let last = br.get(1)?;
        let btype = br.get(2)?;
        match btype {
            0 => {
                br.align();
                let len = br.get(16)? as usize;
                let nlen = br.get(16)? as usize;
                if len != (!nlen & 0xffff) {
                    return Err(InflateError::StoredLength);
                }
                let bytes = br.take_bytes(len)?;
                if out.len() + len > limit {
                    return Err(InflateError::TooLarge);
                }
                out.extend_from_slice(bytes);
            }
            1 => {
                let (ll, d) = fixed_decoders();
                codes(&mut br, &mut out, ll, d, limit)?;
            }
            2 => {
                let (ll, d) = dynamic_tables(&mut br)?;
                codes(&mut br, &mut out, &ll, &d, limit)?;
            }
            _ => return Err(InflateError::BadBlockType),
        }
        if last == 1 {
            break;
        }
    }
    Ok(Inflated { data: out, consumed: br.byte_pos() })
}

fn dynamic_tables(br: &mut BitReader) -> Result<(Decoder, Decoder), InflateError> {
    let hlit = br.get(5)? as usize + 257;
    let hdist = br.get(5)? as usize + 1;
    let hclen = br.get(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(InflateError::BadTable("too many length or distance symbols"));
    }
    let mut cl = [0u8; 19];
    for &i in CL_ORDER.iter().take(hclen) {
        cl[i] = br.get(3)? as u8;
    }
    let cld = Decoder::new(&cl, Kind::Codes)?;
    let mut lens = [0u8; 316];
    let mut i = 0;
    while i < hlit + hdist {
        let sym = cld.decode(br)?;
        match sym {
            0..=15 => {
                lens[i] = sym as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return Err(InflateError::BadTable("repeat with no previous length"));
                }
                let prev = lens[i - 1];
                let n = 3 + br.get(2)? as usize;
                if i + n > hlit + hdist {
                    return Err(InflateError::BadTable("too many lengths"));
                }
                for _ in 0..n {
                    lens[i] = prev;
                    i += 1;
                }
            }
            17 | 18 => {
                let n = if sym == 17 { 3 + br.get(3)? as usize } else { 11 + br.get(7)? as usize };
                if i + n > hlit + hdist {
                    return Err(InflateError::BadTable("too many lengths"));
                }
                i += n; // already zero
            }
            _ => return Err(InflateError::BadSymbol),
        }
    }
    if lens[256] == 0 {
        return Err(InflateError::BadTable("missing end-of-block code"));
    }
    let ll = Decoder::new(&lens[..hlit], Kind::Lens)?;
    let d = Decoder::new(&lens[hlit..hlit + hdist], Kind::Dists)?;
    Ok((ll, d))
}

fn codes(br: &mut BitReader, out: &mut Vec<u8>, ll: &Decoder, d: &Decoder, limit: usize) -> Result<(), InflateError> {
    loop {
        let sym = ll.decode(br)?;
        if sym < 256 {
            if out.len() >= limit {
                return Err(InflateError::TooLarge);
            }
            out.push(sym as u8);
        } else if sym == END_OF_BLOCK {
            return Ok(());
        } else {
            let c = sym - 257;
            if c >= 29 {
                return Err(InflateError::BadSymbol);
            }
            let len = LEN_BASE[c] as usize + br.get(LEN_EXTRA[c] as u32)? as usize;
            let dc = d.decode(br)?;
            if dc >= 30 {
                return Err(InflateError::BadSymbol);
            }
            let dist = DIST_BASE[dc] as usize + br.get(DIST_EXTRA[dc] as u32)? as usize;
            if dist > out.len() {
                return Err(InflateError::TooFarBack);
            }
            if out.len() + len > limit {
                return Err(InflateError::TooLarge);
            }
            let start = out.len() - dist;
            if dist >= len {
                out.extend_from_within(start..start + len);
            } else {
                for k in 0..len {
                    let b = out[start + k];
                    out.push(b);
                }
            }
        }
    }
}

/// The output `inflate_pieces` keeps behind what it hands over: as far back as a distance reaches.
const WINDOW: usize = 32 * 1024;

/// Inflates like `inflate`, but hands the output to `sink` a piece at a time instead of returning it whole: a
/// piece is what one Deflate block makes. Only the window (the last 32 KiB, as far back as a distance
/// reaches; up to 64 KiB, so that small blocks don't each move it) and one block are held at a time. zlib
/// never makes a block of more than 2^(mem_level + 6) - 1 symbols, so a block it made gives at most about
/// 8.4 MB; a block made otherwise can be as large as `limit`. The same checks and errors as `inflate`, at the
/// same places; the pieces before an error have been handed over. `sink` returns false to stop (then
/// Ok(None)). Ok: the output's length and the input used (as `Inflated::consumed`).
pub fn inflate_pieces(
    input: &[u8],
    limit: usize,
    mut sink: impl FnMut(&[u8]) -> bool,
) -> Result<Option<(usize, usize)>, InflateError> {
    let mut br = BitReader::new(input);
    // The window and the block being made; the `gone` bytes before them were handed over and dropped.
    // `codes` is given the limit less those, so its size checks are `inflate`'s. Once anything is dropped it
    // holds 32 KiB at least, so its test for a distance before the start of the output is `inflate`'s too.
    let mut out: Vec<u8> = Vec::with_capacity((input.len() * 4).min(limit).min(4 * WINDOW));
    let mut gone = 0usize;
    loop {
        let last = br.get(1)?;
        let btype = br.get(2)?;
        let start = out.len();
        match btype {
            0 => {
                br.align();
                let len = br.get(16)? as usize;
                let nlen = br.get(16)? as usize;
                if len != (!nlen & 0xffff) {
                    return Err(InflateError::StoredLength);
                }
                let bytes = br.take_bytes(len)?;
                if len > limit - gone - out.len() {
                    return Err(InflateError::TooLarge);
                }
                out.extend_from_slice(bytes);
            }
            1 => {
                let (ll, d) = fixed_decoders();
                codes(&mut br, &mut out, ll, d, limit - gone)?;
            }
            2 => {
                let (ll, d) = dynamic_tables(&mut br)?;
                codes(&mut br, &mut out, &ll, &d, limit - gone)?;
            }
            _ => return Err(InflateError::BadBlockType),
        }
        if out.len() > start && !sink(&out[start..]) {
            return Ok(None);
        }
        if out.len() >= 2 * WINDOW {
            let drop = out.len() - WINDOW;
            out.drain(..drop);
            gone += drop;
        }
        if last == 1 {
            break;
        }
    }
    Ok(Some((gone + out.len(), br.byte_pos())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flate::bits::BitWriter;
    use crate::flate::zlibexact::{zlib_stream, Params};

    /// `inflate_pieces` with every piece kept: the output, the input used and the largest piece.
    fn in_pieces(input: &[u8], limit: usize) -> Result<(Vec<u8>, usize, usize), InflateError> {
        let (mut out, mut largest) = (Vec::new(), 0);
        let (len, used) = inflate_pieces(input, limit, |p| {
            largest = largest.max(p.len());
            out.extend_from_slice(p);
            true
        })?
        .expect("never stopped");
        assert_eq!(len, out.len());
        Ok((out, used, largest))
    }

    /// Checks that both ways give the same bytes and input used, or the same error; returns the first.
    fn same(input: &[u8], limit: usize) -> Result<Inflated, InflateError> {
        let whole = inflate(input, limit);
        match (&whole, in_pieces(input, limit)) {
            (Ok(w), Ok((p, used, _))) => assert!(w.data == p && w.consumed == used, "{} {}", w.data.len(), p.len()),
            (Err(a), Err(b)) => assert_eq!(*a, b),
            (w, p) => panic!("{:?} against {:?}", w.as_ref().map(|w| w.data.len()), p.map(|p| p.0.len())),
        }
        whole
    }

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// Deflate data made by zlib's algorithm (the zlib wrapper taken off) from text, runs, noise (stored
    /// blocks) and repeats as far back as zlib reaches, with small and large blocks.
    fn streams() -> Vec<(Params, Vec<u8>, Vec<u8>)> {
        let mut s = 5u64;
        let noise: Vec<u8> = (0..40_000).map(|_| rng(&mut s) as u8).collect();
        let text: Vec<u8> =
            (0..6000u32).flat_map(|i| format!("{} {} Td (x{}) Tj\n", i % 97, i % 13, i).into_bytes()).collect();
        let far: Vec<u8> = noise[..32_000].repeat(5);
        let data = [Vec::new(), vec![7], vec![0; 300_000], text, noise, far];
        let mut out = Vec::new();
        for d in data {
            for (level, mem_level) in [(1, 8), (6, 1), (6, 8), (9, 9), (4, 2)] {
                let p = Params { level, window_bits: 15, mem_level, filtered: false };
                let z = zlib_stream(&d, p);
                out.push((p, z[2..z.len() - 4].to_vec(), d.clone()));
            }
        }
        out
    }

    #[test]
    fn pieces_are_the_whole_output_cut_at_block_ends() {
        for (p, raw, d) in streams() {
            let w = same(&raw, usize::MAX).unwrap();
            assert_eq!(w.data, d);
            // A block of zlib's has at most 2^(mem_level + 6) - 1 symbols of at most 258 bytes each.
            let (_, _, largest) = in_pieces(&raw, usize::MAX).unwrap();
            assert!(largest <= ((1 << (p.mem_level + 6)) - 1) * 258, "{p:?}: {largest}");
            // Exactly at the limit, and one byte short of it.
            same(&raw, d.len()).unwrap();
            if !d.is_empty() {
                assert_eq!(same(&raw, d.len() - 1).unwrap_err(), InflateError::TooLarge);
                same(&raw, d.len() / 2).unwrap_err();
            }
            // Cut short anywhere.
            for cut in (0..raw.len()).step_by(raw.len() / 7 + 1) {
                same(&raw[..cut], usize::MAX).unwrap_err();
            }
        }
    }

    #[test]
    fn damaged_streams_give_the_same_errors() {
        let mut s = 11u64;
        for (_, raw, _) in streams().into_iter().filter(|(_, raw, _)| raw.len() > 8) {
            for _ in 0..40 {
                let mut bad = raw.clone();
                let i = (rng(&mut s) % raw.len() as u64) as usize;
                bad[i] ^= 1 << (rng(&mut s) % 8);
                let _ = same(&bad, 1 << 24);
            }
        }
    }

    /// Writes a Huffman code (Deflate stores them from their first bit).
    fn code(w: &mut BitWriter, c: u32, len: u32) {
        w.put(c.reverse_bits() >> (32 - len), len);
    }

    /// A literal or a length symbol with the fixed codes.
    fn fixed(w: &mut BitWriter, sym: u32) {
        match sym {
            0..=143 => code(w, 0x30 + sym, 8),
            144..=255 => code(w, 0x190 + sym - 144, 9),
            256..=279 => code(w, sym - 256, 7),
            _ => code(w, 0xc0 + sym - 280, 8),
        }
    }

    /// Stored blocks holding `data` (none final).
    fn stored(w: &mut BitWriter, data: &[u8]) {
        for part in data.chunks(65535) {
            w.put(0, 3);
            w.align();
            w.put(part.len() as u32, 16);
            w.put(!part.len() as u32 & 0xffff, 16);
            for &b in part {
                w.put(b as u32, 8);
            }
        }
    }

    #[test]
    fn crafted_streams_give_the_same_results() {
        let mut s = 3u64;
        let noise: Vec<u8> = (0..70_000).map(|_| rng(&mut s) as u8).collect();
        // Stored blocks, then a fixed block whose match reaches back exactly 32 KiB (distance code 29 with
        // all 13 extra bits set), after the window has moved: restored the same both ways.
        let mut w = BitWriter::new();
        stored(&mut w, &noise);
        w.put(0b011, 3);
        fixed(&mut w, 285);
        code(&mut w, 29, 5);
        w.put(8191, 13);
        fixed(&mut w, 256);
        let far = w.finish();
        let out = same(&far, usize::MAX).unwrap().data;
        assert_eq!(&out[70_000..], &noise[70_000 - 32_768..70_000 - 32_768 + 258]);
        same(&far, 70_000 + 257).unwrap_err();
        // A literal, then a match two bytes back: too far.
        let mut w = BitWriter::new();
        w.put(0b011, 3);
        fixed(&mut w, b'a' as u32);
        fixed(&mut w, 257);
        code(&mut w, 1, 5);
        fixed(&mut w, 256);
        assert_eq!(same(&w.finish(), 100).unwrap_err(), InflateError::TooFarBack);
        // Length symbol 286, which can't occur.
        let mut w = BitWriter::new();
        w.put(0b011, 3);
        fixed(&mut w, 286);
        assert_eq!(same(&w.finish(), 100).unwrap_err(), InflateError::BadSymbol);
        // Block type 3; a stored length whose complement is wrong; empty stored blocks.
        let mut w = BitWriter::new();
        w.put(0b111, 3);
        assert_eq!(same(&w.finish(), 100).unwrap_err(), InflateError::BadBlockType);
        let mut w = BitWriter::new();
        w.put(1, 3);
        w.align();
        w.put(5, 16);
        w.put(5, 16);
        assert_eq!(same(&w.finish(), 100).unwrap_err(), InflateError::StoredLength);
        let mut w = BitWriter::new();
        stored(&mut w, &[]);
        w.put(1, 3);
        w.align();
        w.put(0, 16);
        w.put(0xffff, 16);
        assert!(same(&w.finish(), 0).unwrap().data.is_empty());
    }

    #[test]
    fn a_sink_can_stop_it() {
        let (_, raw, d) = streams().swap_remove(13);
        let mut got = Vec::new();
        let r = inflate_pieces(&raw, usize::MAX, |p| {
            got.extend_from_slice(p);
            got.len() < 10_000
        });
        assert_eq!(r, Ok(None));
        assert!(got.len() >= 10_000 && d.starts_with(&got));
    }
}
