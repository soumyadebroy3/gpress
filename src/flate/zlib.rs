//! The zlib wrapper (RFC 1950) that PDF's FlateDecode uses around Deflate data.

use super::inflate::{inflate, inflate_pieces, InflateError};

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

/// The Adler-32 of more data: `adler` is that of the bytes before `data` (1 for none), so the Adler-32 of a
/// stream handed over in pieces is worked out as they come; `adler32(d) == adler32_more(1, d)`.
pub fn adler32_more(adler: u32, data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    // As in `adler32`: the sums are reduced every 5552 bytes, the most they take without overflowing from
    // any start below 2^16.
    const NMAX: usize = 5552;
    let (mut a, mut b) = (adler & 0xffff, adler >> 16);
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

/// `unwrap` handing the data to `sink` a piece at a time (see `inflate_pieces`) instead of returning it
/// whole: the same checks of the header, the Deflate data and the checksum, with the same errors. `sink`
/// returns false to stop (then Ok(None), and nothing after it is checked). Ok: the data's length and the
/// bytes after the Adler-32 trailer (as `Unwrapped::trailing`).
pub fn unwrap_pieces(
    input: &[u8],
    limit: usize,
    mut sink: impl FnMut(&[u8]) -> bool,
) -> Result<Option<(usize, usize)>, ZlibError> {
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
    let mut adler = 1;
    let inflated = inflate_pieces(&input[2..], limit, |p| {
        adler = adler32_more(adler, p);
        sink(p)
    })
    .map_err(ZlibError::Inflate)?;
    let Some((len, consumed)) = inflated else { return Ok(None) };
    let end = 2 + consumed;
    if input.len() < end + 4 {
        return Err(ZlibError::NoChecksum);
    }
    let want = u32::from_be_bytes([input[end], input[end + 1], input[end + 2], input[end + 3]]);
    if want != adler {
        return Err(ZlibError::BadChecksum);
    }
    Ok(Some((len, input.len() - end - 4)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adler_known_value() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
    }

    #[test]
    fn adler_runs_on_over_pieces() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i * 7 + i / 300) as u8).collect();
        let ones = vec![0xffu8; 30_000];
        for d in [&data[..], &ones[..], b""] {
            for cut in [0, 1, 5551, 5552, 5553, d.len() / 2, d.len()] {
                let cut = cut.min(d.len());
                assert_eq!(adler32_more(adler32_more(1, &d[..cut]), &d[cut..]), adler32(d));
            }
        }
    }

    /// `unwrap_pieces` with every piece kept.
    fn in_pieces(input: &[u8], limit: usize) -> Result<Unwrapped, ZlibError> {
        let mut data = Vec::new();
        let (len, trailing) = unwrap_pieces(input, limit, |p| {
            data.extend_from_slice(p);
            true
        })?
        .expect("never stopped");
        assert_eq!(len, data.len());
        Ok(Unwrapped { data, trailing })
    }

    fn same(input: &[u8], limit: usize) {
        match (unwrap(input, limit), in_pieces(input, limit)) {
            (Ok(a), Ok(b)) => assert!(a.data == b.data && a.trailing == b.trailing),
            (Err(a), Err(b)) => assert_eq!(a, b),
            (a, b) => panic!("{:?} against {:?}", a.map(|a| a.data.len()), b.map(|b| b.data.len())),
        }
    }

    #[test]
    fn unwraps_the_same_in_pieces() {
        // Every stream CPython's zlib made in tests/data, with bytes after it, cut short, its checksum
        // changed, and at the limit and one byte short of it.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let list = std::fs::read_to_string(dir.join("vectors.txt")).unwrap();
        for line in list.lines() {
            let z = std::fs::read(dir.join(line.split('\t').next().unwrap())).unwrap();
            let n = unwrap(&z, usize::MAX).unwrap().data.len();
            let mut bad = z.clone();
            *bad.last_mut().unwrap() ^= 1;
            let cases =
                [z.clone(), [&z[..], b"\r\n"].concat(), z[..z.len() - 1].to_vec(), z[..z.len() - 4].to_vec(), bad];
            for c in cases {
                for limit in [usize::MAX, n, n.saturating_sub(1)] {
                    same(&c, limit);
                }
            }
        }
        // Headers: too short, not Deflate, a window too large, a bad check, a preset dictionary.
        for h in [&b"\x78"[..], b"\x79\x9c\x03\x00", b"\x88\x98\x03\x00", b"\x78\x9d\x03\x00", b"\x78\xbb\x03\x00"] {
            same(h, 100);
        }
        // A sink that stops: what it was given is the data's start, and nothing after is checked.
        let z = std::fs::read(dir.join("text-9-def-15.z")).unwrap();
        let mut got = Vec::new();
        let r = unwrap_pieces(&z[..z.len() - 2], usize::MAX, |p| {
            got.extend_from_slice(p);
            false
        });
        assert_eq!(r, Ok(None));
        assert!(!got.is_empty() && unwrap(&z, usize::MAX).unwrap().data.starts_with(&got));
    }
}
