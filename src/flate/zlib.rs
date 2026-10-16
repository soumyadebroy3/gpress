//! The zlib wrapper (RFC 1950) that PDF's FlateDecode uses around Deflate data.

use super::inflate::{inflate, inflate_len, inflate_pieces, InflateError};

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
    header(input)?;
    let inf = inflate(&input[2..], limit).map_err(ZlibError::Inflate)?;
    let end = checksum_at(input, 2 + inf.consumed, adler32(&inf.data))?;
    Ok(Unwrapped { trailing: input.len() - end, data: inf.data })
}

/// The length of the data `unwrap` gives, with the same checks and the same errors, but at most 128 KiB of
/// the data held at any time (see `inflate_len`).
pub fn unwrapped_len(input: &[u8], limit: usize) -> Result<usize, ZlibError> {
    header(input)?;
    let (len, adler, consumed) = inflate_len(&input[2..], limit).map_err(ZlibError::Inflate)?;
    checksum_at(input, 2 + consumed, adler)?;
    Ok(len)
}

/// The two header bytes: Deflate, a window zlib accepts, the header check, no preset dictionary.
fn header(input: &[u8]) -> Result<(), ZlibError> {
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
    Ok(())
}

/// The Adler-32 trailer at `end` must be `adler`; gives where the trailer ends.
fn checksum_at(input: &[u8], end: usize, adler: u32) -> Result<usize, ZlibError> {
    if input.len() < end + 4 {
        return Err(ZlibError::NoChecksum);
    }
    let want = u32::from_be_bytes([input[end], input[end + 1], input[end + 2], input[end + 3]]);
    if want != adler {
        return Err(ZlibError::BadChecksum);
    }
    Ok(end + 4)
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
    header(input)?;
    let mut adler = 1;
    let inflated = inflate_pieces(&input[2..], limit, |p| {
        adler = adler32_more(adler, p);
        sink(p)
    })
    .map_err(ZlibError::Inflate)?;
    let Some((len, consumed)) = inflated else { return Ok(None) };
    let end = checksum_at(input, 2 + consumed, adler)?;
    Ok(Some((len, input.len() - end)))
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

#[cfg(test)]
mod counted {
    use super::*;
    use crate::flate::{deflate_zlib, Options};

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// Streams with matches across the counter's window cuts, stored blocks (random bytes) and fixed codes.
    fn streams() -> Vec<Vec<u8>> {
        let mut s = 11u64;
        let block: Vec<u8> = (0..30_000).map(|_| rng(&mut s) as u8).collect();
        let mut text = Vec::new();
        for k in 0..20_000 {
            text.extend_from_slice(format!("{} {} l ", k % 113, k % 7).as_bytes());
        }
        let noise: Vec<u8> = (0..150_000).map(|_| rng(&mut s) as u8).collect();
        let datas = [Vec::new(), b"a".to_vec(), b"abcabcabc".to_vec(), block.repeat(9), text, noise, vec![7u8; 300_000]];
        datas.iter().map(|d| deflate_zlib(d, &Options::quick())).collect()
    }

    fn same(z: &[u8], limit: usize) {
        let full = unwrap(z, limit).map(|u| u.data.len());
        assert_eq!(unwrapped_len(z, limit), full, "{} bytes, limit {limit}", z.len());
    }

    #[test]
    fn the_counted_length_is_the_decoded_length_or_the_same_error() {
        let mut s = 5u64;
        for z in streams() {
            let n = unwrap(&z, usize::MAX).unwrap().data.len();
            assert_eq!(unwrapped_len(&z, usize::MAX), Ok(n));
            // Over the limit by one byte, exactly at it, and far under it.
            for limit in [n, n.saturating_sub(1), n / 2, 0] {
                same(&z, limit);
            }
            // Cut short, a header byte or a checksum byte changed, and bits flipped anywhere.
            for cut in [0, 1, 2, z.len() / 3, z.len() - 5, z.len() - 1] {
                same(&z[..cut], usize::MAX);
            }
            for at in [0, 1, z.len() - 1] {
                let mut bad = z.clone();
                bad[at] ^= 0x10;
                same(&bad, usize::MAX);
            }
            for _ in 0..40 {
                let mut bad = z.clone();
                let at = 2 + rng(&mut s) as usize % (z.len() - 2);
                bad[at] ^= 1 << (rng(&mut s) % 8);
                same(&bad, usize::MAX);
                same(&bad, n);
            }
        }
    }
}
