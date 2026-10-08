//! The code tables of ITU-T T.4 (Tables 1, 2 and 3: run lengths) and the 2-D mode codes of T.4 and T.6.
//! Codes are written here as bit strings, most significant bit first, the way they appear in the
//! recommendations; the lookup tables are built from them once.

use std::sync::OnceLock;

/// White terminating codes, run lengths 0 to 63.
const WHITE_TERM: [&str; 64] = [
    "00110101", "000111", "0111", "1000", "1011", "1100", "1110", "1111", "10011", "10100", "00111", "01000", "001000", "000011",
    "110100", "110101", "101010", "101011", "0100111", "0001100", "0001000", "0010111", "0000011", "0000100", "0101000",
    "0101011", "0010011", "0100100", "0011000", "00000010", "00000011", "00011010", "00011011", "00010010", "00010011",
    "00010100", "00010101", "00010110", "00010111", "00101000", "00101001", "00101010", "00101011", "00101100", "00101101",
    "00000100", "00000101", "00001010", "00001011", "01010010", "01010011", "01010100", "01010101", "00100100", "00100101",
    "01011000", "01011001", "01011010", "01011011", "01001010", "01001011", "00110010", "00110011", "00110100",
];

/// White make-up codes, run lengths 64, 128, ... 1728.
const WHITE_MAKEUP: [&str; 27] = [
    "11011", "10010", "010111", "0110111", "00110110", "00110111", "01100100", "01100101", "01101000", "01100111", "011001100",
    "011001101", "011010010", "011010011", "011010100", "011010101", "011010110", "011010111", "011011000", "011011001",
    "011011010", "011011011", "010011000", "010011001", "010011010", "011000", "010011011",
];

/// Black terminating codes, run lengths 0 to 63.
const BLACK_TERM: [&str; 64] = [
    "0000110111", "010", "11", "10", "011", "0011", "0010", "00011", "000101", "000100", "0000100", "0000101", "0000111",
    "00000100", "00000111", "000011000", "0000010111", "0000011000", "0000001000", "00001100111", "00001101000",
    "00001101100", "00000110111", "00000101000", "00000010111", "00000011000", "000011001010", "000011001011",
    "000011001100", "000011001101", "000001101000", "000001101001", "000001101010", "000001101011", "000011010010",
    "000011010011", "000011010100", "000011010101", "000011010110", "000011010111", "000001101100", "000001101101",
    "000011011010", "000011011011", "000001010100", "000001010101", "000001010110", "000001010111", "000001100100",
    "000001100101", "000001010010", "000001010011", "000000100100", "000000110111", "000000111000", "000000100111",
    "000000101000", "000001011000", "000001011001", "000000101011", "000000101100", "000001011010", "000001100110",
    "000001100111",
];

/// Black make-up codes, run lengths 64, 128, ... 1728.
const BLACK_MAKEUP: [&str; 27] = [
    "0000001111", "000011001000", "000011001001", "000001011011", "000000110011", "000000110100", "000000110101",
    "0000001101100", "0000001101101", "0000001001010", "0000001001011", "0000001001100", "0000001001101", "0000001110010",
    "0000001110011", "0000001110100", "0000001110101", "0000001110110", "0000001110111", "0000001010010", "0000001010011",
    "0000001010100", "0000001010101", "0000001011010", "0000001011011", "0000001100100", "0000001100101",
];

/// Extended make-up codes (both colours), run lengths 1792, 1856, ... 2560.
const EXT_MAKEUP: [&str; 13] = [
    "00000001000", "00000001100", "00000001101", "000000010010", "000000010011", "000000010100", "000000010101",
    "000000010110", "000000010111", "000000011100", "000000011101", "000000011110", "000000011111",
];

/// The end-of-line code: eleven 0 bits and a 1.
pub const EOL_BITS: u32 = 1;
pub const EOL_LEN: u32 = 12;

/// Longest run code, in bits.
pub const MAX_RUN_CODE: u32 = 13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Code {
    pub bits: u32,
    pub len: u32,
}

fn parse(s: &str) -> Code {
    let mut bits = 0;
    for c in s.bytes() {
        bits = bits << 1 | (c == b'1') as u32;
    }
    Code { bits, len: s.len() as u32 }
}

/// 2-D coding modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Pass,
    Horizontal,
    /// Vertical mode: a1 is at b1 plus this offset (-3 to 3).
    Vertical(i32),
    /// The 2-D extension code (uncompressed mode and others): never written, refused when read.
    Extension,
}

pub const PASS: Code = Code { bits: 0b0001, len: 4 };
pub const HORIZONTAL: Code = Code { bits: 0b001, len: 3 };
/// Vertical codes for offsets -3 to 3 (index offset + 3).
pub const VERTICAL: [Code; 7] = [
    Code { bits: 0b0000010, len: 7 },
    Code { bits: 0b000010, len: 6 },
    Code { bits: 0b010, len: 3 },
    Code { bits: 0b1, len: 1 },
    Code { bits: 0b011, len: 3 },
    Code { bits: 0b000011, len: 6 },
    Code { bits: 0b0000011, len: 7 },
];
const EXTENSION: Code = Code { bits: 0b0000001, len: 7 };

/// The codes for both colours, and lookup tables over the next 13 (runs) or 7 (modes) bits.
pub struct Tables {
    /// [colour][run 0..=63]
    pub term: [[Code; 64]; 2],
    /// [colour][k] for run 64 * (k + 1), k = 0..=26
    pub makeup: [[Code; 27]; 2],
    /// run 1792 + 64 * k, k = 0..=12
    pub ext: [Code; 13],
    /// [colour][next 13 bits] -> (code length, run length); length 0 marks bits that start no code.
    pub run_lookup: [Vec<(u8, u16)>; 2],
    /// [next 7 bits] -> (code length, mode); length 0 marks bits that start no mode code.
    pub mode_lookup: Vec<(u8, Mode)>,
}

impl Tables {
    fn build() -> Tables {
        let mut term = [[Code { bits: 0, len: 0 }; 64]; 2];
        let mut makeup = [[Code { bits: 0, len: 0 }; 27]; 2];
        for i in 0..64 {
            term[0][i] = parse(WHITE_TERM[i]);
            term[1][i] = parse(BLACK_TERM[i]);
        }
        for i in 0..27 {
            makeup[0][i] = parse(WHITE_MAKEUP[i]);
            makeup[1][i] = parse(BLACK_MAKEUP[i]);
        }
        let mut ext = [Code { bits: 0, len: 0 }; 13];
        for i in 0..13 {
            ext[i] = parse(EXT_MAKEUP[i]);
        }
        let mut run_lookup = [vec![(0u8, 0u16); 1 << MAX_RUN_CODE], vec![(0u8, 0u16); 1 << MAX_RUN_CODE]];
        for colour in 0..2 {
            let mut all: Vec<(Code, u16)> = Vec::new();
            for i in 0..64 {
                all.push((term[colour][i], i as u16));
            }
            for i in 0..27 {
                all.push((makeup[colour][i], 64 * (i as u16 + 1)));
            }
            for i in 0..13 {
                all.push((ext[i], 1792 + 64 * i as u16));
            }
            for (c, run) in all {
                let shift = MAX_RUN_CODE - c.len;
                for low in 0..(1u32 << shift) {
                    let idx = (c.bits << shift | low) as usize;
                    // The codes are prefix-free: no two codes may claim the same entry.
                    assert_eq!(run_lookup[colour][idx].0, 0, "overlapping run codes");
                    run_lookup[colour][idx] = (c.len as u8, run);
                }
            }
        }
        let mut mode_lookup = vec![(0u8, Mode::Pass); 128];
        let mut modes: Vec<(Code, Mode)> = vec![(PASS, Mode::Pass), (HORIZONTAL, Mode::Horizontal), (EXTENSION, Mode::Extension)];
        for (i, c) in VERTICAL.iter().enumerate() {
            modes.push((*c, Mode::Vertical(i as i32 - 3)));
        }
        for (c, m) in modes {
            let shift = 7 - c.len;
            for low in 0..(1u32 << shift) {
                let idx = (c.bits << shift | low) as usize;
                assert_eq!(mode_lookup[idx].0, 0, "overlapping mode codes");
                mode_lookup[idx] = (c.len as u8, m);
            }
        }
        Tables { term, makeup, ext, run_lookup, mode_lookup }
    }
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(Tables::build)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kraft sum of a code set, in units of 2^-13.
    fn kraft(codes: &[Code]) -> u32 {
        codes.iter().map(|c| 1u32 << (MAX_RUN_CODE - c.len)).sum()
    }

    #[test]
    fn run_codes_are_prefix_free_and_leave_room_only_for_eol() {
        let t = tables();
        for colour in 0..2 {
            let mut all: Vec<Code> = t.term[colour].to_vec();
            all.extend_from_slice(&t.makeup[colour]);
            all.extend_from_slice(&t.ext);
            // Building the lookup table already asserted that no two codes overlap. What is left over is
            // exactly the space of bit strings starting with seven 0 bits that are no extended make-up code
            // (EOL lives there): white uses 2^13 - 64 entries... the exact count is fixed by the tables.
            let used = kraft(&all);
            let free = t.run_lookup[colour].iter().filter(|e| e.0 == 0).count() as u32;
            assert_eq!(used + free, 1 << MAX_RUN_CODE);
            // No run code starts with eight 0 bits, so an EOL (eleven 0 bits) can never be read as one.
            for c in &all {
                assert!(c.len >= 1 && c.len <= MAX_RUN_CODE);
                let leading = (c.bits << (32 - c.len)).leading_zeros().min(c.len);
                assert!(leading <= 7, "{c:?}");
            }
            for idx in 0..(1usize << 5) {
                assert_eq!(t.run_lookup[colour][idx].0, 0, "a code starts with eight 0 bits");
            }
        }
        // Known values from T.4.
        assert_eq!(t.term[0][0], Code { bits: 0b00110101, len: 8 });
        assert_eq!(t.term[1][0], Code { bits: 0b0000110111, len: 10 });
        assert_eq!(t.makeup[0][26], Code { bits: 0b010011011, len: 9 }); // white 1728
        assert_eq!(t.makeup[1][26], Code { bits: 0b0000001100101, len: 13 }); // black 1728
        assert_eq!(t.ext[12], Code { bits: 0b000000011111, len: 12 }); // 2560
    }

    #[test]
    fn mode_codes_are_prefix_free() {
        let t = tables();
        // Only 0000000 (the start of an EOL) is left without a mode code.
        let free: Vec<usize> = (0..128).filter(|&i| t.mode_lookup[i].0 == 0).collect();
        assert_eq!(free, vec![0]);
        assert_eq!(t.mode_lookup[0b1000000], (1, Mode::Vertical(0)));
        assert_eq!(t.mode_lookup[0b0001000], (4, Mode::Pass));
        assert_eq!(t.mode_lookup[0b0010000], (3, Mode::Horizontal));
        assert_eq!(t.mode_lookup[0b0000010], (7, Mode::Vertical(-3)));
        assert_eq!(t.mode_lookup[0b0000011], (7, Mode::Vertical(3)));
    }
}
