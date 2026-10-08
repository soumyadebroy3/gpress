//! The zlib wrapper (RFC 1950) that PDF's FlateDecode uses around Deflate data.

use super::inflate::{inflate, InflateError};

pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    const NMAX: usize = 5552;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(NMAX) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// Wraps raw Deflate data: header 78 DA (32K window, "maximum compression") and the Adler-32 of `original`.
pub fn wrap(raw: &[u8], original: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 6);
    out.extend_from_slice(&[0x78, 0xDA]);
    out.extend_from_slice(raw);
    out.extend_from_slice(&adler32(original).to_be_bytes());
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZlibError {
    Header(&'static str),
    /// A preset dictionary is needed; such streams are left exactly as they are.
    NeedsDictionary,
    Inflate(InflateError),
    /// The Adler-32 trailer is missing.
    NoChecksum,
    /// The Adler-32 trailer doesn't match the data.
    BadChecksum,
}

#[derive(Debug)]
pub struct Unwrapped {
    pub data: Vec<u8>,
    /// Bytes after the Adler-32 trailer (readers ignore them).
    pub trailing: usize,
}

/// Decodes a zlib stream strictly: the header, the Deflate data to its final block, and a matching checksum.
pub fn unwrap(input: &[u8], limit: usize) -> Result<Unwrapped, ZlibError> {
    if input.len() < 2 {
        return Err(ZlibError::Header("too short"));
    }
    let (cmf, flg) = (input[0], input[1]);
    if cmf & 0x0f != 8 {
        return Err(ZlibError::Header("not Deflate"));
    }
    if cmf >> 4 > 7 {
        return Err(ZlibError::Header("window too large"));
    }
    if !((cmf as u16) << 8 | flg as u16).is_multiple_of(31) {
        return Err(ZlibError::Header("bad header check"));
    }
    if flg & 0x20 != 0 {
        return Err(ZlibError::NeedsDictionary);
    }
    let inf = inflate(&input[2..], limit).map_err(ZlibError::Inflate)?;
    let end = 2 + inf.consumed;
    if input.len() < end + 4 {
        return Err(ZlibError::NoChecksum);
    }
    let want = u32::from_be_bytes([input[end], input[end + 1], input[end + 2], input[end + 3]]);
    if want != adler32(&inf.data) {
        return Err(ZlibError::BadChecksum);
    }
    Ok(Unwrapped { trailing: input.len() - end - 4, data: inf.data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adler_known_value() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
    }
}
