//! The Flate encoders' rules: the tuning values of the two encoders (`deflate.rs` with `block.rs`, `matcher.rs`
//! and `parse.rs`; `fast.rs`) as named settings. A rule has a default, which is the value the code had before
//! it was a rule, and a lowest and a highest value. With no rule set the encoders give the bytes they always
//! gave; any value in a rule's range gives a valid stream.
//!
//! What stays fixed: the facts of the format, the seed of the strong encoder's blend (the output must be the
//! same on every run), the effort bands' edges (20, 40, 60, 80, 100: `pdf::rewrite` has tables on the same
//! edges), the number of fast levels, the limits that keep memory bounded, which are the highest values of
//! their rules here, and what is code rather than a number: the run lengths the parse tells apart, the fast
//! encoder's farthest distance (its tables' indexing) and the kinds of symbol it counts to see the data change.

use std::sync::Arc;

/// How many rules there are.
const N: usize = FlateRules::RULES.len();

/// The values of the Flate encoders' rules, one per row of `RULES` (named there without the group's prefix,
/// `flate_`). Cheap to clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlateRules(Arc<[u32; N]>);

/// Every rule at its default. Building it checks that every default is in its range and that every value fits
/// the 32 bits it is kept in.
const DEFAULTS: [u32; N] = {
    let mut v = [0; N];
    let mut i = 0;
    while i < N {
        let (_, default, lowest, highest) = FlateRules::RULES[i];
        assert!(lowest <= default && default <= highest && highest <= u32::MAX as u64);
        v[i] = default as u32;
        i += 1;
    }
    v
};

impl Default for FlateRules {
    fn default() -> Self {
        FlateRules(Arc::new(DEFAULTS))
    }
}

impl FlateRules {
    /// (name without the prefix, default, lowest, highest), in a fixed order.
    pub const RULES: &'static [(&'static str, u64, u64, u64)] = &[
        // The strong encoder's settings (`deflate::Options`), one row per effort band and one for the quick
        // settings; `deflate::Options::row` reads them by position. Per row: pricing rounds and exact rounds
        // (0: none; at most twice the strongest row's), how many earlier positions the match finder compares
        // (0: no matches; 32768: the whole window), the bytes optimised together (at least 4096, so that the
        // match finder's tables are not made anew every few bytes; at most the 1 MiB it always was, which
        // bounds the memory of a thread), block splitting (0 or 1), farther-match alternatives (0 or 1), the
        // size up to which a segment is also started a second way (0: never), the length from which a match
        // is taken at once (3 to 258; 259: never) and block polishing (0 or 1).
        ("quick_rounds", 2, 0, 16),
        ("quick_exact_rounds", 1, 0, 8),
        ("quick_depth", 128, 0, 32768),
        ("quick_segment", 1 << 20, 4096, 1 << 20),
        ("quick_split", 1, 0, 1),
        ("quick_alternatives", 0, 0, 1),
        ("quick_second_start", 0, 0, 1 << 20),
        ("quick_nice", 64, 3, 259),
        ("quick_polish", 0, 0, 1),
        ("e20_rounds", 2, 0, 16),
        ("e20_exact_rounds", 1, 0, 8),
        ("e20_depth", 1024, 0, 32768),
        ("e20_segment", 1 << 20, 4096, 1 << 20),
        ("e20_split", 1, 0, 1),
        ("e20_alternatives", 0, 0, 1),
        ("e20_second_start", 0, 0, 1 << 20),
        ("e20_nice", 259, 3, 259),
        ("e20_polish", 0, 0, 1),
        ("e40_rounds", 3, 0, 16),
        ("e40_exact_rounds", 1, 0, 8),
        ("e40_depth", 1024, 0, 32768),
        ("e40_segment", 1 << 20, 4096, 1 << 20),
        ("e40_split", 1, 0, 1),
        ("e40_alternatives", 0, 0, 1),
        ("e40_second_start", 0, 0, 1 << 20),
        ("e40_nice", 259, 3, 259),
        ("e40_polish", 0, 0, 1),
        ("e60_rounds", 4, 0, 16),
        ("e60_exact_rounds", 2, 0, 8),
        ("e60_depth", 2048, 0, 32768),
        ("e60_segment", 1 << 20, 4096, 1 << 20),
        ("e60_split", 1, 0, 1),
        ("e60_alternatives", 1, 0, 1),
        ("e60_second_start", 0, 0, 1 << 20),
        ("e60_nice", 259, 3, 259),
        ("e60_polish", 1, 0, 1),
        ("e80_rounds", 6, 0, 16),
        ("e80_exact_rounds", 3, 0, 8),
        ("e80_depth", 4096, 0, 32768),
        ("e80_segment", 1 << 20, 4096, 1 << 20),
        ("e80_split", 1, 0, 1),
        ("e80_alternatives", 1, 0, 1),
        ("e80_second_start", 0, 0, 1 << 20),
        ("e80_nice", 259, 3, 259),
        ("e80_polish", 1, 0, 1),
        ("e100_rounds", 8, 0, 16),
        ("e100_exact_rounds", 4, 0, 8),
        ("e100_depth", 8192, 0, 32768),
        ("e100_segment", 1 << 20, 4096, 1 << 20),
        ("e100_split", 1, 0, 1),
        ("e100_alternatives", 1, 0, 1),
        ("e100_second_start", 1 << 18, 0, 1 << 20),
        ("e100_nice", 259, 3, 259),
        ("e100_polish", 1, 0, 1),
        // The fast encoder's level for efforts 0 to 19.
        ("fast_level", 12, 1, 12),
        // The fast encoder's twelve levels; `fast::Level::of` reads them by position. Per level: how it
        // chooses (0 the longest match at once, 1 looking one position ahead, 2 looking two ahead, 3 the
        // cheapest path), how many earlier positions it compares (at least 1; at most four times the deepest
        // level's), from what length a match is taken at once, the parses per block of the cheapest path
        // (0 and 1 are both one parse; at most four times the top level's) and whether hash chains also
        // look for 3-byte matches (0 or 1).
        ("level1_mode", 0, 0, 3),
        ("level1_depth", 2, 1, 400),
        ("level1_nice", 16, 3, 258),
        ("level1_passes", 0, 0, 8),
        ("level1_short", 0, 0, 1),
        ("level2_mode", 1, 0, 3),
        ("level2_depth", 3, 1, 400),
        ("level2_nice", 16, 3, 258),
        ("level2_passes", 0, 0, 8),
        ("level2_short", 0, 0, 1),
        ("level3_mode", 1, 0, 3),
        ("level3_depth", 6, 1, 400),
        ("level3_nice", 16, 3, 258),
        ("level3_passes", 0, 0, 8),
        ("level3_short", 1, 0, 1),
        ("level4_mode", 1, 0, 3),
        ("level4_depth", 12, 1, 400),
        ("level4_nice", 32, 3, 258),
        ("level4_passes", 0, 0, 8),
        ("level4_short", 1, 0, 1),
        ("level5_mode", 1, 0, 3),
        ("level5_depth", 24, 1, 400),
        ("level5_nice", 48, 3, 258),
        ("level5_passes", 0, 0, 8),
        ("level5_short", 1, 0, 1),
        ("level6_mode", 1, 0, 3),
        ("level6_depth", 64, 1, 400),
        ("level6_nice", 96, 3, 258),
        ("level6_passes", 0, 0, 8),
        ("level6_short", 1, 0, 1),
        ("level7_mode", 2, 0, 3),
        ("level7_depth", 100, 1, 400),
        ("level7_nice", 258, 3, 258),
        ("level7_passes", 0, 0, 8),
        ("level7_short", 1, 0, 1),
        ("level8_mode", 3, 0, 3),
        ("level8_depth", 6, 1, 400),
        ("level8_nice", 24, 3, 258),
        ("level8_passes", 1, 0, 8),
        ("level8_short", 1, 0, 1),
        ("level9_mode", 3, 0, 3),
        ("level9_depth", 12, 1, 400),
        ("level9_nice", 32, 3, 258),
        ("level9_passes", 1, 0, 8),
        ("level9_short", 1, 0, 1),
        ("level10_mode", 3, 0, 3),
        ("level10_depth", 12, 1, 400),
        ("level10_nice", 32, 3, 258),
        ("level10_passes", 2, 0, 8),
        ("level10_short", 1, 0, 1),
        ("level11_mode", 3, 0, 3),
        ("level11_depth", 24, 1, 400),
        ("level11_nice", 64, 3, 258),
        ("level11_passes", 2, 0, 8),
        ("level11_short", 1, 0, 1),
        ("level12_mode", 3, 0, 3),
        ("level12_depth", 35, 1, 400),
        ("level12_nice", 100, 3, 258),
        ("level12_passes", 2, 0, 8),
        ("level12_short", 1, 0, 1),
        // The strong encoder (`deflate.rs`). A block below this many bytes is priced with a real code search
        // when parses are compared, a larger one with an estimate (0: always the estimate; 1 MiB and one byte:
        // every block, since a segment is at most 1 MiB).
        ("exact_price_below", 65536, 0, (1 << 20) + 1),
        // Both encoders: the block type written when two cost the same. 0: fixed, then dynamic, then stored;
        // 1: dynamic before fixed; 2: stored before both.
        ("block_tie", 0, 0, 2),
        // Polishing: its passes (0: none; at most four times the default), how much larger the joined
        // estimate of two blocks may be for a merge to be tried (a percentage of their bits, and at most so
        // many bits), the smallest block tried for a split (symbols), the step of the search for a cut (the
        // block's symbols divided by this, and at least so many: at least 1, or the search would not move),
        // and the bits a cut must be estimated to save.
        ("polish_passes", 2, 0, 8),
        ("polish_merge_percent", 2, 0, 100),
        ("polish_merge_bits", 400, 0, 1 << 20),
        ("polish_min_block", 64, 0, 1 << 20),
        ("polish_step_div", 64, 1, 256),
        ("polish_step_min", 4, 1, 1 << 16),
        ("polish_min_gain", 6, 0, 1 << 20),
        // Pricing rounds stop after this many rounds in a row without a gain (at least 1).
        ("stuck_rounds", 4, 1, 16),
        // After a round without a gain the new counts are mixed with the old ones divided by this, and for
        // half the literals and lengths (chosen with a fixed seed) with the new ones divided by this.
        ("blend_old_div", 2, 1, 1024),
        ("blend_noise_div", 8, 1, 1024),
        // Exact rounds: which of the three tries of a round are made (a set of bits: 1 the first, 2 the
        // second, 4 the third, which also offers the farther matches), and the bits each try charges, over the
        // longest code, for a symbol that has no code.
        ("exact_tries", 7, 1, 7),
        ("exact_try1_missing", 1, 0, 16),
        ("exact_try2_missing", 4, 0, 16),
        ("exact_try3_missing", 1, 0, 16),
        // Block splitting: the fewest symbols of a segment that is split at all (at least 2), the distance
        // between candidate boundaries (the segment's symbols divided by this, and at least so many: at least
        // 1), the most candidates one block may span, the bits an uncompressed block is priced at over 8 a
        // byte, how far each chosen boundary is fine-tuned (in candidate distances either side; 0: not at
        // all) and in what steps (the candidate distance divided by this).
        ("split_min_symbols", 64, 2, 1 << 20),
        ("split_step_div", 160, 1, 640),
        ("split_step_min", 32, 1, 1 << 16),
        ("split_span", 96, 1, 192),
        ("split_stored_extra_bits", 40, 0, 4096),
        ("split_tune_reach", 1, 0, 2),
        ("split_tune_div", 8, 1, 16),
        // The match finder (`matcher.rs`): its search trees are 2 to this power (at least 256 trees; at most
        // the 65,536 it always had, which bound its memory).
        ("hash_bits", 16, 8, 16),
        // With farther-match alternatives: how many of a position's matches the parse weighs (at most 32, the
        // room the parse has for them), and how many matches a position may have before no more alternatives
        // are kept (0: none are; at most the 24 it always was, each being three bytes per position).
        ("alt_candidates", 32, 1, 32),
        ("alt_keep", 24, 0, 24),
        // Deep inside a run of one byte the parse takes the longest match without pricing (1) or prices every
        // position (0).
        ("run_shortcut", 1, 0, 1),
        // The prices the first pass over a segment starts from, in bits: a literal byte below 144 and from
        // 144, a match length up to 114 and from 115, a distance. The defaults are the fixed code's lengths;
        // a code is at most 15 bits long.
        ("first_literal_bits", 8, 1, 15),
        ("first_high_literal_bits", 9, 1, 15),
        ("first_length_bits", 7, 1, 15),
        ("first_long_length_bits", 8, 1, 15),
        ("first_distance_bits", 5, 1, 15),
        // In the pricing rounds a symbol the last parse never used costs the bits of one used once, plus this.
        ("unused_symbol_bits", 1, 0, 16),
        // Block codes (`block.rs`). Count smoothing evens out runs of at least this many symbols (287: never,
        // an alphabet has at most 286).
        ("smooth_min_run", 4, 1, 287),
        // Which of the six code variants are tried for a block that is written, and for one that is only
        // priced (sets of bits, 1 for the first variant to 32 for the sixth, tried in that order). The
        // variants: plain counts, counts smoothed by a quarter, by a half, codes of at most 12 bits, of at
        // most 10 bits, counts smoothed fully.
        ("code_variants", 63, 1, 63),
        ("price_code_variants", 3, 1, 63),
        // The search for the cheapest block header, for a block that is written and for one that is only
        // priced: which of its eight starting strategies are used (sets of bits; the first search tries
        // them from the lowest bit up, the second from the highest down), and the rounds from each start
        // (at least 1; at most four times the default of the first search).
        ("header_starts", 255, 1, 255),
        ("header_rounds", 4, 1, 16),
        ("price_header_starts", 192, 1, 255),
        ("price_header_rounds", 2, 1, 16),
        // The bits a header symbol costs in that search while the header does not use it (a header code is
        // at most 7 bits long, so the default is one more).
        ("header_unused_bits", 8, 1, 15),
        // The fast encoder (`fast.rs`). The input is cut into segments of this many bytes, each encoded on
        // its own (at least 65,536; at most the 1 MiB it always was), and each segment's match finder is
        // primed with this much input before it (more than the 32 KiB window is no use).
        ("fast_segment", 1 << 20, 65536, 1 << 20),
        ("fast_dict", 32768, 0, 32768),
        // Where its blocks end: the symbols gathered before each check (at least 64, so that the check stays
        // rare), the fewest bytes of a block ended on a change of statistics, the most bytes of a block of
        // the levels that do not search for the cheapest path (at least 1000; at most the 300,000 it always
        // was, which bound the symbols held), how different the latest symbols must be (in 32nds of the
        // largest difference there can be, 64), the symbols below which a block needs a larger difference,
        // and the bytes of a block that each lower the difference needed (at least 1).
        ("fast_batch", 512, 64, 65536),
        ("fast_min_block", 2000, 0, 1 << 20),
        ("fast_max_block", 300_000, 1000, 300_000),
        ("fast_end_threshold", 10, 0, 64),
        ("fast_young_block", 8192, 0, 1 << 20),
        ("fast_age_step", 2048, 1, 1 << 20),
        // Its hash tables are at most 2 to these powers (4-byte and 3-byte hashes; at least 256 entries, at
        // most what they always were).
        ("fast_hash4_bits", 16, 8, 16),
        ("fast_hash3_bits", 15, 8, 15),
        // Lazy matching: the units a later match gains per byte it is longer and per halving of the distance,
        // and the gain over which the match one position ahead, and two ahead, replaces the current one.
        ("fast_gain_per_byte", 4, 0, 64),
        ("fast_gain_per_halving", 1, 0, 64),
        ("fast_lazy_gain", 1, 0, 1024),
        ("fast_lazy2_gain", 4, 0, 1024),
        // The cheapest-path levels: how far back the first pass still counts a 3-byte match, the candidate
        // matches kept per position (their count is kept in a byte), the positions of one block (at least
        // 4096; at most the 262,144 it always was, which bound the memory of a segment), the bits charged
        // for a literal, a length and a distance the statistics do not have, the weight of the block
        // before in a block's first prices (percent of the block's own) and how many of the last distances
        // are tried again at every position (0 to 2).
        ("fast_far3_distance", 1024, 0, 32768),
        ("fast_max_candidates", 64, 1, 255),
        ("fast_max_opt_block", 1 << 18, 4096, 1 << 18),
        ("fast_unused_literal_bits", 11, 1, 30),
        ("fast_unused_length_bits", 13, 1, 30),
        ("fast_unused_distance_bits", 10, 1, 30),
        ("fast_blend_percent", 100, 0, 400),
        ("fast_repeat_distances", 2, 0, 2),
        // Speed and memory only; these never change the output. The largest input whose output a store
        // remembers (at most 1 MiB, the bound it always had), the segments the strong encoder plans as one
        // group (at most the 8 it always was, which bound the plans held) and the segments the fast encoder
        // hands to the threads at once (at most the 16 it always was, which bound the finished output held
        // before it is written).
        ("remember_up_to", 256 << 10, 0, 1 << 20),
        ("segments_at_once", 8, 1, 8),
        ("fast_group", 16, 1, 16),
    ];

    /// The row of a rule in `RULES`.
    const fn find(name: &str) -> Option<usize> {
        let name = name.as_bytes();
        let mut i = 0;
        while i < N {
            let row = Self::RULES[i].0.as_bytes();
            let mut k = 0;
            while k < row.len() && k < name.len() && row[k] == name[k] {
                k += 1;
            }
            if k == row.len() && k == name.len() {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// The row of a rule the code names (see `rule!`): worked out when the crate is built, where a name that
    /// is not in `RULES` stops the build.
    pub(crate) const fn at(name: &str) -> usize {
        match Self::find(name) {
            Some(i) => i,
            None => panic!("no such Flate rule"),
        }
    }

    /// The value in row `i`.
    #[inline(always)]
    pub(crate) fn v(&self, i: usize) -> usize {
        self.0[i] as usize
    }

    /// A hash of all the values (for the key of remembered outputs).
    pub(crate) fn hash(&self) -> u64 {
        crate::par::hash64(self.0.map(u32::to_le_bytes).as_flattened())
    }

    /// Sets a rule. false: no such rule, or a value outside lowest..=highest; nothing is changed then, and
    /// nothing is ever clamped silently.
    pub fn set(&mut self, name: &str, value: u64) -> bool {
        let Some(i) = Self::find(name) else { return false };
        let (_, _, lowest, highest) = Self::RULES[i];
        if value < lowest || value > highest {
            return false;
        }
        Arc::make_mut(&mut self.0)[i] = value as u32;
        true
    }

    /// The value of a rule, or None when there is no such rule.
    pub fn get(&self, name: &str) -> Option<u64> {
        Self::find(name).map(|i| self.0[i] as u64)
    }
}

/// The value of a rule as a `usize`: `rule!(rules, "polish_passes")`. The name is looked up when the crate is
/// built, so reading a rule costs one load.
macro_rules! rule {
    ($rules:expr, $name:literal) => {{
        const AT: usize = $crate::flate::rules::FlateRules::at($name);
        $rules.v(AT)
    }};
}
pub(crate) use rule;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flate::deflate::Shared;
    use crate::flate::{deflate_zlib, zlib, Options};

    /// The rows of the strong encoder's table and the columns of the two tables, as their rules are named.
    const BANDS: [&str; 6] = ["quick", "e20", "e40", "e60", "e80", "e100"];
    const BAND_COLUMNS: [&str; 9] =
        ["rounds", "exact_rounds", "depth", "segment", "split", "alternatives", "second_start", "nice", "polish"];
    const LEVEL_COLUMNS: [&str; 5] = ["mode", "depth", "nice", "passes", "short"];
    /// The rules that change how the work is shared out, never the output.
    const FOR_SPEED: [&str; 3] = ["remember_up_to", "segments_at_once", "fast_group"];

    fn with(sets: &[(&str, u64)]) -> FlateRules {
        let mut r = FlateRules::default();
        for &(name, value) in sets {
            assert!(r.set(name, value), "{name} = {value}");
        }
        r
    }

    fn xorshift(s: &mut u32) -> u32 {
        *s ^= *s << 13;
        *s ^= *s >> 17;
        *s ^= *s << 5;
        *s
    }

    /// Text, numbers in columns, a run, noise, stretches of what came before, bytes of a few values, numbers
    /// again and steps, `n` bytes in all: work for every part of both encoders.
    fn sample(n: usize) -> Vec<u8> {
        let mut s = 0x2545_F491;
        let mut out = Vec::with_capacity(n + 400);
        while out.len() < n {
            let x = xorshift(&mut s);
            match out.len() * 8 / n {
                0 => out.extend_from_slice(
                    format!("BT /F{} 12 Tf {} {} Td (line {}) Tj ET\n", x % 3, x % 600, x % 97 * 8, x % 50).as_bytes(),
                ),
                1 | 6 => out.extend_from_slice(format!("{:05} {:05} n \n", x % 1000 * 7, x % 13).as_bytes()),
                2 => out.extend_from_slice(&[if x.is_multiple_of(300) { 255 } else { 0 }; 40]),
                3 => out.extend_from_slice(&x.to_le_bytes()),
                4 => {
                    let from = x as usize % out.len();
                    out.extend_from_within(from..(from + 100 + (x >> 20) as usize % 300).min(out.len()));
                }
                5 => out.push((x % 7) as u8 * 37),
                _ => out.extend_from_slice(&[(out.len() / 64) as u8; 16]),
            }
        }
        out.truncate(n);
        out
    }

    /// Whether a rule belongs to the fast encoder alone.
    fn of_fast(name: &str) -> bool {
        name.starts_with("fast_") || name.starts_with("level")
    }

    /// 180 bytes that came before in two parts, their first 120 and their last 80: where the first part's match
    /// ends is a matter of the prices of the lengths on both sides of 115.
    fn overlap() -> Vec<u8> {
        let mut s = 7;
        let mut noise = |n: usize| (0..n).map(|_| (xorshift(&mut s) >> 9) as u8).collect::<Vec<u8>>();
        let x = noise(180);
        [&x[..120], &noise(40)[..], &x[100..], &noise(40)[..], &x[..]].concat()
    }

    /// Where a rule shows in the output: an input, and the values other rules need for it.
    fn scene(name: &str) -> (Vec<u8>, Vec<(&'static str, u64)>) {
        let row = FlateRules::find(name).unwrap();
        let earlier = |back: usize| FlateRules::RULES[row - back].0;
        if of_fast(name) && name.ends_with("_passes") {
            // A level's passes count only in its optimal mode...
            return (sample(150_000), vec![(earlier(3), 3)]);
        }
        if of_fast(name) && name.ends_with("_short") {
            // ...and its 3-byte matches are a choice only outside it.
            return (sample(150_000), vec![(earlier(4), 1)]);
        }
        match name {
            // The bytes before a segment serve the segments after the first.
            "fast_dict" => (sample(150_000), vec![("fast_segment", 65536)]),
            // 30 literals of 9 bits: as large stored as under the fixed codes.
            "block_tie" => ((0..30).map(|k| 144 + 3 * k).collect(), vec![]),
            "first_long_length_bits" => (overlap(), vec![]),
            _ if of_fast(name) => (sample(150_000), vec![]),
            _ => (sample(24_000), vec![]),
        }
    }

    /// The settings a rule is at work in, by its name: a band's rules in that band, a level's at that level, the
    /// fast encoder's others at a level of each kind (greedy, lazy, lazy twice, optimal with one pass and with
    /// two), the strong encoder's others in every band but the first and at the quick settings.
    fn settings(name: &str, r: &FlateRules) -> Vec<Options> {
        let fast = |level: u8| Options { fast: level, ..Options::quick_with(r) };
        if let Some(k) = BANDS.iter().position(|b| name.strip_prefix(b).is_some_and(|rest| rest.starts_with('_'))) {
            return vec![if k == 0 { Options::quick_with(r) } else { Options::effort_with(20 * k as u8, r) }];
        }
        if let Some(level) = name.strip_prefix("level").and_then(|rest| rest.split('_').next()?.parse().ok()) {
            return vec![fast(level)];
        }
        if name == "fast_level" {
            return vec![Options::effort_with(0, r)];
        }
        if of_fast(name) {
            return [1, 4, 7, 9, 12].map(fast).to_vec();
        }
        let mut v = vec![Options::quick_with(r)];
        v.extend([100, 40, 60, 80].map(|effort| Options::effort_with(effort, r)));
        if name == "block_tie" {
            v.push(fast(12));
        }
        v
    }

    fn decodes_to(z: &[u8], data: &[u8]) -> bool {
        zlib::unwrap(z, data.len() + 1).is_ok_and(|u| u.data == data)
    }

    #[test]
    fn the_tables_are_where_the_code_reads_them() {
        let names: Vec<&str> = FlateRules::RULES.iter().map(|r| r.0).collect();
        for (i, name) in names.iter().enumerate() {
            assert!(!names[..i].contains(name), "{name} twice");
            assert_eq!(FlateRules::find(name), Some(i));
        }
        // `deflate::Options::row` and `fast::Level::of` read their tables by position.
        let bands = FlateRules::at("quick_rounds");
        for (k, band) in BANDS.iter().enumerate() {
            for (c, column) in BAND_COLUMNS.iter().enumerate() {
                assert_eq!(names[bands + BAND_COLUMNS.len() * k + c], format!("{band}_{column}"));
            }
        }
        let levels = FlateRules::at("level1_mode");
        for level in 1..=crate::flate::fast::LEVELS as usize {
            for (c, column) in LEVEL_COLUMNS.iter().enumerate() {
                assert_eq!(names[levels + LEVEL_COLUMNS.len() * (level - 1) + c], format!("level{level}_{column}"));
            }
        }
    }

    #[test]
    fn set_takes_a_value_in_range_and_nothing_else() {
        let unset = FlateRules::default();
        let mut by_hand = unset.clone();
        for &(name, default, lowest, highest) in FlateRules::RULES {
            let mut r = unset.clone();
            for outside in [lowest.wrapping_sub(1), highest + 1, u64::MAX] {
                assert!(!r.set(name, outside), "{name} = {outside}");
            }
            assert!(r == unset && r.get(name) == Some(default), "{name}: a refused value changes nothing");
            for inside in [lowest, highest] {
                assert!(r.set(name, inside) && r.get(name) == Some(inside), "{name} = {inside}");
                assert_eq!(r == unset, inside == default, "{name} = {inside}");
                assert_eq!(r.hash() == unset.hash(), inside == default, "{name} = {inside}");
            }
            assert!(by_hand.set(name, default));
        }
        // Every default set by hand is the same as nothing set; a copy keeps its own values.
        assert!(by_hand == unset && by_hand.hash() == unset.hash());
        assert_eq!(unset.get("hash_bits"), Some(16));
        // No such rule (a name carries no prefix here).
        let mut r = unset.clone();
        assert!(!r.set("flate_hash_bits", 12) && !r.set("", 1) && r.get("flate_hash_bits").is_none() && r == unset);
    }

    #[test]
    fn the_defaults_set_by_hand_give_the_same_bytes() {
        let mut by_hand = FlateRules::default();
        for &(name, default, ..) in FlateRules::RULES {
            assert!(by_hand.set(name, default));
        }
        let data = sample(24_000);
        for effort in [0, 20, 40, 60, 80, 100] {
            let opt = Options::effort_with(effort, &by_hand);
            assert!(opt.same_output(&Options::effort(effort)), "effort {effort}");
            assert!(deflate_zlib(&data, &opt) == deflate_zlib(&data, &Options::effort(effort)), "effort {effort}");
        }
        assert!(deflate_zlib(&data, &Options::quick_with(&by_hand)) == deflate_zlib(&data, &Options::quick()));
        // The settings are remembered by their rules too: another value of a rule that no field of `Options`
        // carries, another output, also from one store.
        let other = with(&[("header_starts", 1)]);
        assert!(!Options::effort_with(40, &other).same_output(&Options::effort(40)));
        let shared = Shared::new(1);
        let outputs = [FlateRules::default(), other].map(|r| {
            let fresh = deflate_zlib(&data, &Options::effort_with(40, &r));
            let opt = Options { shared: Some(shared.clone()), ..Options::effort_with(40, &r) };
            assert!(deflate_zlib(&data, &opt) == fresh);
            fresh
        });
        assert!(outputs[0] != outputs[1]);
    }

    #[test]
    fn the_lowest_and_the_highest_of_every_rule_give_a_valid_stream() {
        let tiny: [&[u8]; 4] = [b"", b"a", b"abcabcabcabc", &[0; 300]];
        for &(name, _, lowest, highest) in FlateRules::RULES {
            let (sample, around) = scene(name);
            for value in [lowest, highest] {
                for opt in settings(name, &with(&[&around[..], &[(name, value)]].concat())) {
                    for data in tiny.into_iter().chain([&sample[..]]) {
                        let z = deflate_zlib(data, &opt);
                        assert!(decodes_to(&z, data), "{name} = {value}, {} bytes, fast {}", data.len(), opt.fast);
                    }
                }
            }
        }
    }

    #[test]
    fn every_rule_changes_the_output_except_the_three_for_speed() {
        let mut silent = Vec::new();
        for &(name, default, lowest, highest) in FlateRules::RULES {
            if FOR_SPEED.contains(&name) {
                continue;
            }
            let (data, around) = scene(name);
            let plain = settings(name, &with(&around));
            let others: Vec<Vec<Options>> =
                [lowest, highest, default / 2, default * 2, default + 1, default.saturating_sub(1)]
                    .into_iter()
                    .filter(|&v| v != default && (lowest..=highest).contains(&v))
                    .map(|v| settings(name, &with(&[&around[..], &[(name, v)]].concat())))
                    .collect();
            let shows = |data: &[u8]| {
                let plain: Vec<Vec<u8>> = plain.iter().map(|opt| deflate_zlib(data, opt)).collect();
                others
                    .iter()
                    .any(|other| other.iter().zip(&plain).any(|(opt, plain)| deflate_zlib(data, opt) != *plain))
            };
            // Some rules win on some inputs only (polishing at the quick settings, the second start at effort 100):
            // more inputs for those.
            if ![data, sample(12_000), sample(48_000), sample(96_000)].iter().any(|data| shows(data)) {
                silent.push(name);
            }
        }
        assert!(silent.is_empty(), "no value tried changes the output: {silent:?}");
    }

    #[test]
    fn the_rules_for_speed_and_the_threads_change_no_output() {
        // Many segments for both encoders, so that groups of them and several threads take part.
        let many = [("fast_segment", 65536), ("e40_segment", 8192)];
        let data = sample(150_000);
        let efforts = [0, 40];
        let want = efforts.map(|e| deflate_zlib(&data, &Options::effort_with(e, &with(&many))));
        for name in FOR_SPEED {
            let (_, _, lowest, highest) = FlateRules::RULES[FlateRules::at(name)];
            for value in [lowest, highest, (lowest + highest) / 2] {
                let r = with(&[many[0], many[1], (name, value)]);
                for threads in [1, 2, 3] {
                    for (e, want) in efforts.iter().zip(&want) {
                        let opt = Options { shared: Some(Shared::new(threads)), ..Options::effort_with(*e, &r) };
                        assert!(deflate_zlib(&data, &opt) == *want, "{name} = {value}, {threads} threads, effort {e}");
                    }
                }
            }
        }
        // An input is remembered up to `remember_up_to` bytes: beyond it the same input is encoded again, to the
        // same bytes.
        let small = sample(24_000);
        for (remember, hits) in
            [(DEFAULTS[FlateRules::at("remember_up_to")] as u64, 1), (24_000, 1), (23_999, 0), (0, 0)]
        {
            let shared = Shared::new(1);
            let r = with(&[("remember_up_to", remember)]);
            let opt = Options { shared: Some(shared.clone()), ..Options::effort_with(40, &r) };
            let first = deflate_zlib(&small, &opt);
            assert!(first == deflate_zlib(&small, &Options::effort(40)) && deflate_zlib(&small, &opt) == first);
            assert_eq!(shared.stats().hits, hits, "remember_up_to = {remember}");
        }
    }

    #[test]
    fn any_values_together_give_a_valid_stream_whatever_the_threads() {
        let data = sample(24_000);
        // One store of outputs for every set of values: what it gives back must be what these values make.
        let shared = Shared::new(3);
        let mut s = 0x9E37_79B9;
        for k in 0..24u8 {
            let mut r = FlateRules::default();
            for &(name, _, lowest, highest) in FlateRules::RULES {
                // All at their lowest, all at their highest, then half of them anywhere in their ranges.
                let x = xorshift(&mut s) as u64;
                let value = match k {
                    0 => lowest,
                    1 => highest,
                    _ if x & 1 == 0 => continue,
                    _ => lowest + (x >> 1) % (highest - lowest + 1),
                };
                assert!(r.set(name, value));
            }
            let band = Options::effort_with(20 * (1 + k % 5), &r);
            let level = Options { fast: 1 + k % 12, ..Options::quick_with(&r) };
            for opt in [Options::quick_with(&r), Options::effort_with(0, &r), band, level] {
                let z = deflate_zlib(&data, &opt);
                assert!(decodes_to(&z, &data), "set {k}, fast {}, {} rounds", opt.fast, opt.iterations);
                let threaded = Options { shared: Some(shared.clone()), ..opt };
                assert!(deflate_zlib(&data, &threaded) == z, "set {k}: three threads");
            }
        }
    }
}
