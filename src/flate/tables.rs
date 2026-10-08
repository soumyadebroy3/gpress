//! Deflate's fixed tables (RFC 1951, section 3.2.5).

pub const MIN_MATCH: usize = 3;
pub const MAX_MATCH: usize = 258;
pub const WINDOW: usize = 32768;
pub const END_OF_BLOCK: usize = 256;
/// Literal/length alphabet size used by real data (286 and 287 never appear).
pub const LITLEN_SYMS: usize = 286;
pub const DIST_SYMS: usize = 30;

pub const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
pub const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
pub const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
pub const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Order in which code-length code lengths are sent.
pub const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// Length (3..=258) to length code index (0..=28); symbol = 257 + index.
pub static LEN_CODE: [u8; 259] = build_len_code();

const fn build_len_code() -> [u8; 259] {
    let mut t = [0u8; 259];
    let mut code = 0;
    while code < 29 {
        let base = LEN_BASE[code] as usize;
        let span = 1usize << LEN_EXTRA[code];
        let mut l = base;
        while l < base + span && l <= 258 {
            t[l] = code as u8;
            l += 1;
        }
        code += 1;
    }
    // 258 has its own code (285), never 284 with all extra bits set.
    t[258] = 28;
    t
}

/// Distance (1..=32768) to distance code (0..=29).
#[inline]
pub fn dist_code(d: usize) -> usize {
    debug_assert!((1..=WINDOW).contains(&d));
    if d <= 4 {
        d - 1
    } else {
        let x = (d - 1) as u32;
        let k = 31 - x.leading_zeros(); // floor(log2(x)) >= 2
        (2 * k + ((x >> (k - 1)) & 1)) as usize
    }
}

/// Code lengths of the fixed Huffman codes.
pub fn fixed_litlen_lengths() -> [u8; 288] {
    let mut l = [0u8; 288];
    for (i, v) in l.iter_mut().enumerate() {
        *v = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    l
}

pub fn fixed_dist_lengths() -> [u8; 32] {
    [5u8; 32]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_codes_cover_every_length() {
        for l in 3..=258usize {
            let c = LEN_CODE[l] as usize;
            let base = LEN_BASE[c] as usize;
            assert!(l >= base, "length {l}");
            assert!(l - base < (1 << LEN_EXTRA[c]) || (c == 28 && l == 258), "length {l}");
        }
        assert_eq!(LEN_CODE[258], 28);
        assert_eq!(LEN_CODE[257], 27);
    }

    #[test]
    fn distance_codes_cover_every_distance() {
        for d in 1..=WINDOW {
            let c = dist_code(d);
            let base = DIST_BASE[c] as usize;
            assert!(d >= base && d - base < (1 << DIST_EXTRA[c]), "distance {d} code {c}");
        }
    }
}
