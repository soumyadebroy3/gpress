//! Stream filters we decode ourselves: Flate (our zlib), LZW, ASCIIHex, ASCII85, RunLength, and the PNG/TIFF
//! predictors. Image codecs (DCT, JPX, JBIG2, CCITT) are never decoded here; their bytes are kept as they are.

use super::object::{Dict, Obj};
use crate::flate::zlib;

#[derive(Debug, Clone, PartialEq)]
pub enum FilterError {
    Unsupported,
    Bad(&'static str),
    TooLarge,
}

/// One stage of a stream's filter chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Stage {
    pub name: Vec<u8>,
    pub parms: Option<Dict>,
}

fn full_name(n: &[u8]) -> Vec<u8> {
    match n {
        b"AHx" => b"ASCIIHexDecode".to_vec(),
        b"A85" => b"ASCII85Decode".to_vec(),
        b"LZW" => b"LZWDecode".to_vec(),
        b"Fl" => b"FlateDecode".to_vec(),
        b"RL" => b"RunLengthDecode".to_vec(),
        b"CCF" => b"CCITTFaxDecode".to_vec(),
        b"DCT" => b"DCTDecode".to_vec(),
        _ => n.to_vec(),
    }
}

/// The filter chain of a stream dictionary, or None if it is malformed (e.g. parameters that don't line up).
/// Indirect references in Filter or DecodeParms are not resolved here: such chains are reported as None.
pub fn chain(d: &Dict) -> Option<Vec<Stage>> {
    let names: Vec<Vec<u8>> = match d.get(b"Filter") {
        None | Some(Obj::Null) => return Some(Vec::new()),
        Some(Obj::Name(n)) => vec![full_name(n)],
        Some(Obj::Array(a)) => {
            let mut v = Vec::new();
            for x in a {
                v.push(full_name(x.as_name()?));
            }
            v
        }
        _ => return None,
    };
    let parms: Vec<Option<Dict>> = match d.get(b"DecodeParms") {
        None | Some(Obj::Null) => vec![None; names.len()],
        Some(Obj::Dict(p)) if names.len() == 1 => vec![Some(p.clone())],
        Some(Obj::Array(a)) if a.len() == names.len() => {
            let mut v = Vec::new();
            for x in a {
                match x {
                    Obj::Null => v.push(None),
                    Obj::Dict(p) => v.push(Some(p.clone())),
                    _ => return None,
                }
            }
            v
        }
        _ => return None,
    };
    Some(names.into_iter().zip(parms).map(|(name, parms)| Stage { name, parms }).collect())
}

fn int_parm(p: &Option<Dict>, key: &[u8], default: i64) -> Option<i64> {
    match p.as_ref().and_then(|d| d.get(key)) {
        None => Some(default),
        Some(Obj::Int(i)) => Some(*i),
        _ => None,
    }
}

/// Is this a filter we can decode losslessly and re-encode?
pub fn is_general(name: &[u8]) -> bool {
    matches!(name, b"FlateDecode" | b"LZWDecode" | b"ASCIIHexDecode" | b"ASCII85Decode" | b"RunLengthDecode")
}

/// Decodes one general stage. For Flate and LZW the predictor is NOT undone: the result is what the
/// compressor saw, so re-encoding it with the same DecodeParms gives the same data back.
pub fn decode_stage(st: &Stage, data: &[u8], limit: usize) -> Result<Vec<u8>, FilterError> {
    match st.name.as_slice() {
        b"FlateDecode" => match zlib::unwrap(data, limit) {
            Ok(u) => Ok(u.data),
            Err(zlib::ZlibError::Inflate(crate::flate::InflateError::TooLarge)) => Err(FilterError::TooLarge),
            Err(_) => Err(FilterError::Bad("flate")),
        },
        b"LZWDecode" => {
            let early = int_parm(&st.parms, b"EarlyChange", 1).ok_or(FilterError::Bad("EarlyChange"))?;
            if early != 0 && early != 1 {
                return Err(FilterError::Bad("EarlyChange"));
            }
            lzw_decode(data, early == 1, limit)
        }
        b"ASCIIHexDecode" => ascii_hex_decode(data),
        b"ASCII85Decode" => ascii85_decode(data),
        b"RunLengthDecode" => run_length_decode(data, limit),
        _ => Err(FilterError::Unsupported),
    }
}

/// Predictor settings of a stage: (predictor, colors, bits, columns).
pub fn predictor(st: &Stage) -> Option<(i64, i64, i64, i64)> {
    Some((
        int_parm(&st.parms, b"Predictor", 1)?,
        int_parm(&st.parms, b"Colors", 1)?,
        int_parm(&st.parms, b"BitsPerComponent", 8)?,
        int_parm(&st.parms, b"Columns", 1)?,
    ))
}

/// Undoes a PNG (10-15) or TIFF (2, 8-bit samples) predictor.
pub fn unpredict(st: &Stage, data: &[u8]) -> Result<Vec<u8>, FilterError> {
    let (pred, colors, bpc, cols) = predictor(st).ok_or(FilterError::Bad("predictor parameters"))?;
    if pred == 1 {
        return Ok(data.to_vec());
    }
    if !(1..=32).contains(&colors) || ![1, 2, 4, 8, 16].contains(&bpc) || !(1..=1 << 24).contains(&cols) {
        return Err(FilterError::Bad("predictor parameters"));
    }
    let bits_per_pixel = (colors * bpc) as usize;
    // A row longer than the data holds only one (short) row, whose samples come out the same whatever the
    // row length beyond them: so the row is cut to the data, and nothing is sized by the parameters alone.
    let row = (bits_per_pixel * cols as usize).div_ceil(8).min(data.len()).max(1);
    let bpp = bits_per_pixel.div_ceil(8);
    if pred == 2 {
        if bpc != 8 {
            return Err(FilterError::Unsupported);
        }
        let mut out = data.to_vec();
        for r in out.chunks_mut(row) {
            for i in bpp..r.len() {
                r[i] = r[i].wrapping_add(r[i - bpp]);
            }
        }
        return Ok(out);
    }
    if !(10..=15).contains(&pred) {
        return Err(FilterError::Bad("unknown predictor"));
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        if chunk.len() < 2 {
            break;
        }
        let ft = chunk[0];
        let mut cur = chunk[1..].to_vec();
        cur.resize(row, 0);
        for i in 0..row {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            cur[i] = match ft {
                0 => cur[i],
                1 => cur[i].wrapping_add(a),
                2 => cur[i].wrapping_add(b),
                3 => cur[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => {
                    let p = a as i16 + b as i16 - c as i16;
                    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
                    let pr = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    };
                    cur[i].wrapping_add(pr)
                }
                _ => return Err(FilterError::Bad("unknown PNG filter type")),
            };
        }
        let take = (chunk.len() - 1).min(row);
        out.extend_from_slice(&cur[..take]);
        prev = cur;
    }
    Ok(out)
}

pub fn lzw_decode(data: &[u8], early: bool, limit: usize) -> Result<Vec<u8>, FilterError> {
    let mut prefix: Vec<u16> = vec![0; 4096];
    let mut last: Vec<u8> = vec![0; 4096];
    let mut lens: Vec<u16> = vec![0; 4096];
    for i in 0..256 {
        last[i] = i as u8;
        lens[i] = 1;
    }
    let (mut next, mut width) = (258usize, 9u32);
    let mut prev: Option<usize> = None;
    let mut out = Vec::new();
    let (mut acc, mut nbits, mut pos) = (0u32, 0u32, 0usize);
    let mut buf = Vec::new();
    loop {
        while nbits < width && pos < data.len() {
            acc = (acc << 8) | data[pos] as u32;
            pos += 1;
            nbits += 8;
        }
        if nbits < width {
            break; // no end-of-data code: what was decoded is the content (as readers do)
        }
        let code = ((acc >> (nbits - width)) & ((1 << width) - 1)) as usize;
        nbits -= width;
        if code == 256 {
            next = 258;
            width = 9;
            prev = None;
            continue;
        }
        if code == 257 {
            break;
        }
        // The string for `code`.
        buf.clear();
        let first;
        if code < next && (code < 256 || lens[code] > 0) {
            let mut c = code;
            loop {
                buf.push(last[c]);
                if lens[c] == 1 {
                    break;
                }
                c = prefix[c] as usize;
            }
            buf.reverse();
            first = buf[0];
        } else if code == next && prev.is_some() {
            let mut c = prev.unwrap();
            loop {
                buf.push(last[c]);
                if lens[c] == 1 {
                    break;
                }
                c = prefix[c] as usize;
            }
            buf.reverse();
            first = buf[0];
            buf.push(first);
        } else {
            return Err(FilterError::Bad("LZW code out of range"));
        }
        if out.len() + buf.len() > limit {
            return Err(FilterError::TooLarge);
        }
        out.extend_from_slice(&buf);
        if let Some(p) = prev {
            if next < 4096 {
                prefix[next] = p as u16;
                last[next] = first;
                lens[next] = lens[p].saturating_add(1);
                next += 1;
            }
        }
        prev = Some(code);
        let e = early as usize;
        if next + e >= 512 && width < 10 {
            width = 10;
        }
        if next + e >= 1024 && width < 11 {
            width = 11;
        }
        if next + e >= 2048 && width < 12 {
            width = 12;
        }
    }
    Ok(out)
}

pub fn ascii_hex_decode(data: &[u8]) -> Result<Vec<u8>, FilterError> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut hi: Option<u8> = None;
    for &c in data {
        if c == b'>' {
            break;
        }
        if super::object::is_white(c) {
            continue;
        }
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return Err(FilterError::Bad("bad hex digit")),
        };
        match hi {
            None => hi = Some(v),
            Some(h) => {
                out.push(h << 4 | v);
                hi = None;
            }
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    Ok(out)
}

pub fn ascii85_decode(data: &[u8]) -> Result<Vec<u8>, FilterError> {
    let mut out = Vec::with_capacity(data.len() * 4 / 5);
    let mut group = [0u8; 5];
    let mut n = 0;
    let mut i = 0;
    // An optional "<~" start marker.
    if data.starts_with(b"<~") {
        i = 2;
    }
    while i < data.len() {
        let c = data[i];
        i += 1;
        if super::object::is_white(c) {
            continue;
        }
        if c == b'~' {
            if data.get(i) != Some(&b'>') {
                return Err(FilterError::Bad("bad ASCII85 end"));
            }
            break;
        }
        if c == b'z' {
            if n != 0 {
                return Err(FilterError::Bad("z inside an ASCII85 group"));
            }
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&c) {
            return Err(FilterError::Bad("bad ASCII85 character"));
        }
        group[n] = c - b'!';
        n += 1;
        if n == 5 {
            let mut v: u64 = 0;
            for &g in &group {
                v = v * 85 + g as u64;
            }
            if v > u32::MAX as u64 {
                return Err(FilterError::Bad("ASCII85 group overflow"));
            }
            out.extend_from_slice(&(v as u32).to_be_bytes());
            n = 0;
        }
    }
    if n == 1 {
        return Err(FilterError::Bad("lone ASCII85 character"));
    }
    if n > 1 {
        for k in n..5 {
            group[k] = 84;
        }
        let mut v: u64 = 0;
        for &g in &group {
            v = v * 85 + g as u64;
        }
        if v > u32::MAX as u64 {
            return Err(FilterError::Bad("ASCII85 group overflow"));
        }
        out.extend_from_slice(&(v as u32).to_be_bytes()[..n - 1]);
    }
    Ok(out)
}

pub fn run_length_decode(data: &[u8], limit: usize) -> Result<Vec<u8>, FilterError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let l = data[i] as usize;
        i += 1;
        if l == 128 {
            break;
        }
        if l < 128 {
            let end = i + l + 1;
            if end > data.len() {
                return Err(FilterError::Bad("RunLength literal past the end"));
            }
            out.extend_from_slice(&data[i..end]);
            i = end;
        } else {
            let b = *data.get(i).ok_or(FilterError::Bad("RunLength repeat past the end"))?;
            i += 1;
            out.extend(std::iter::repeat_n(b, 257 - l));
        }
        if out.len() > limit {
            return Err(FilterError::TooLarge);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii85_known() {
        assert_eq!(ascii85_decode(b"87cURD]i,\"Ebo80~>").unwrap(), b"Hello World!".to_vec());
        assert_eq!(ascii85_decode(b"z~>").unwrap(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn hex_and_runlength() {
        assert_eq!(ascii_hex_decode(b"48 65 6C6c 6F7>").unwrap(), b"Hellop".to_vec());
        assert_eq!(run_length_decode(&[2, b'a', b'b', b'c', 254, b'x', 128], 100).unwrap(), b"abcxxx".to_vec());
    }

    #[test]
    fn lzw_spec_example() {
        // PDF reference, LZWDecode example: 45 45 45 45 45 65 45 45 45 66 encodes to 80 0B 60 50 22 0C 0C 85 01.
        let enc = [0x80u8, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        let dec = lzw_decode(&enc, true, 1000).unwrap();
        assert_eq!(dec, vec![45, 45, 45, 45, 45, 65, 45, 45, 45, 66]);
    }

    #[test]
    fn png_up_predictor() {
        let st = Stage {
            name: b"FlateDecode".to_vec(),
            parms: Some(Dict(vec![(b"Predictor".to_vec(), Obj::Int(12)), (b"Columns".to_vec(), Obj::Int(3))])),
        };
        let data = [2u8, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(unpredict(&st, &data).unwrap(), vec![1, 2, 3, 2, 3, 4]);
    }
}
