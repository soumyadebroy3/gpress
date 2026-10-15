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

/// The number of symbols (literals and matches, not the end-of-block code) in the first block of a raw
/// Deflate stream, or None when that block is stored or the final one. Reads only that block and keeps no
/// output; counting stops past `max`, so the count is at most `max + 1`.
pub fn first_block_symbols(input: &[u8], max: usize) -> Result<Option<usize>, InflateError> {
    let mut br = BitReader::new(input);
    let last = br.get(1)?;
    let btype = br.get(2)?;
    if btype == 3 {
        return Err(InflateError::BadBlockType);
    }
    if last == 1 || btype == 0 {
        return Ok(None);
    }
    let dynamic;
    let (ll, d) = if btype == 1 {
        fixed_decoders()
    } else {
        dynamic = dynamic_tables(&mut br)?;
        &dynamic
    };
    // The same checks as `codes`, with the output's length in place of the output.
    let (mut count, mut out_len) = (0usize, 0usize);
    while count <= max {
        let sym = ll.decode(&mut br)?;
        if sym < 256 {
            out_len += 1;
        } else if sym == END_OF_BLOCK {
            break;
        } else {
            let c = sym - 257;
            if c >= 29 {
                return Err(InflateError::BadSymbol);
            }
            let len = LEN_BASE[c] as usize + br.get(LEN_EXTRA[c] as u32)? as usize;
            let dc = d.decode(&mut br)?;
            if dc >= 30 {
                return Err(InflateError::BadSymbol);
            }
            let dist = DIST_BASE[dc] as usize + br.get(DIST_EXTRA[dc] as u32)? as usize;
            if dist > out_len {
                return Err(InflateError::TooFarBack);
            }
            out_len += len;
        }
        count += 1;
    }
    Ok(Some(count))
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
