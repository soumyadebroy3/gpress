//! The values the lossless rewrite decides by, as rules a caller can set.
//!
//! A rule is one value with a name, a default (the value that was fixed in the code before), a lowest and a
//! highest value. With every rule at its default a rewrite gives the bytes it gave when the values were fixed.
//! Any value in a rule's range is safe: a rule only moves which candidates are tried or kept, how much work a
//! stream gets, or how the output is laid out. Whatever the rules say, the output is read back and compared
//! with the input before it is returned, and it is returned only when it is smaller than the input.
//!
//! What stays fixed: the facts of the formats, the bounds on hostile input, the rules kept for other PDF
//! readers (the gate), and the scale of the effort itself (0 to 100, in bands of 20, the fast encoder below
//! 20), which the tables here and the Flate encoder's are both laid out on.
//!
//! `docs/reference.md` says what each rule decides ("Rules of the rewrite"); a test holds that table equal to
//! `RewriteRules::RULES`.

/// The highest value of a rule that is a size in bytes: more than any input (`max_input` is 1 GiB by default).
const ANY: u64 = 1 << 40;

/// The fast encoder's top level, the highest value of a rule that is one of its levels.
const TOP: u64 = crate::flate::fast::LEVELS as u64;

macro_rules! rules {
    ($($name:ident = $default:expr, $lowest:expr, $highest:expr;)*) => {
        /// A rule's place in `RewriteRules::RULES`. A table's cells follow each other and are read by their
        /// place after the first (`RewriteRules::cell`), so only the first is named in the code.
        #[allow(non_camel_case_types, dead_code)]
        #[derive(Clone, Copy)]
        pub(crate) enum Rule {
            $($name,)*
        }

        impl RewriteRules {
            /// (name, default, lowest, highest), in a fixed order.
            pub const RULES: &'static [(&'static str, u64, u64, u64)] =
                &[$((stringify!($name), $default, $lowest, $highest),)*];
        }
    };
}

rules! {
    // The whole file: returned only when it saves at least this many bytes.
    min_file_saving = 1, 1, ANY;

    // A stream: a new encoding must save at least this many bytes of stored data (with the stream's dictionary
    // counted where the fast path chooses an image's predictor).
    min_stream_saving = 1, 1, ANY;
    // Flate in front of an image codec that had no general filter: only when it saves more than this
    // percentage (over the JPEG with its new tables, when there is one).
    flate_over_image_min_saving = 1, 0, 100;
    // A JPEG with new Huffman tables must save at least this many bytes.
    jpeg_min_saving = 1, 1, ANY;
    // Which assignments of code words are tried for a JPEG scan (bits: 1 by symbol value, 2 most frequent
    // first, 4 fewest one bits); the one with the fewest 0xFF bytes is written, the first of equals.
    jpeg_code_orders = 7, 1, 7;
    // The second Flate pass over a JPEG's original bytes, where every stream gets the full search: only when
    // Flate took this percentage off the JPEG with its new tables,
    jpeg_second_pass_flate_gain = 10, 0, 100;
    // and the new tables took this percentage off the original.
    jpeg_second_pass_table_gain = 10, 0, 100;
    // A fax image becomes JBIG2 only when that saves at least this percentage (and at least a byte).
    jbig2_min_saving = 5, 0, 100;

    // Image predictors. An image that decodes to more than this gets no predictor search (a value over the
    // default counts only as far as `max_stream`: the search holds copies of the image; at most 1 GiB: the
    // fast path's estimate counts an image's bytes in 32 bits).
    max_predictor_image = 48 << 20, 0, 1 << 30;
    // An image with fewer bytes of samples gets none either.
    min_predictor_image = 1024, 0, ANY;
    // The candidates ranked by encoding them (bits: 1 no predictor, 2 Sub, 4 Up, 8 Average, 16 Paeth, 32 a
    // type chosen for each row). Ranked in the order none, per row, Up, Sub, Paeth, Average: the first of
    // equals wins.
    predictor_candidates = 55, 0, 63;
    // The candidates the estimate of the fast path compares (the same bits, in the order of the bits).
    fast_predictor_candidates = 63, 0, 63;
    // The fast path gives no predictor to an image with fewer bits per component.
    fast_predictor_min_bits = 8, 1, 16;
    // How the type of each row is chosen: 0 the smallest sum of absolute residuals, 1 the lowest order-0 cost
    // of the row's residuals.
    row_predictor_rule = 0, 0, 1;
    // The rows the candidates are ranked on are taken as this many bands spread over the image.
    predictor_sample_bands = 8, 1, 1024;
    // Bytes of an image the estimate of the fast path looks at.
    fast_predictor_sample = 64 << 10, 0, ANY;
    // Bytes of an image the candidates are ranked on, for the efforts 0 to 19, 20 to 39, 40 to 59, 60 to 79
    // and 80 to 99.
    predictor_sample_0 = 64 << 10, 0, ANY;
    predictor_sample_20 = 128 << 10, 0, ANY;
    predictor_sample_40 = 256 << 10, 0, ANY;
    predictor_sample_60 = 512 << 10, 0, ANY;
    predictor_sample_80 = 1 << 20, 0, ANY;
    // The fast encoder's level the candidates are ranked with on the fast path (0: the strong encoder's quick
    // settings, as from effort 20).
    predictor_rank_level = 7, 0, TOP;
    // Candidate inputs up to this size are ranked side by side, bigger ones one after another. Speed and
    // memory only.
    rank_together = 8 << 20, 0, ANY;

    // The layout of the output. Small objects are packed into object streams (1) or written one by one (0).
    object_streams = 1, 0, 1;
    // The order of the objects in the object streams: 0 by kind (dictionaries by type and subtype, then
    // arrays, then the rest), 1 by object number.
    object_stream_order = 0, 0, 1;
    // The cross-reference stream: 0 the smaller of plain and Up-predicted, 1 always plain, 2 always Up.
    xref_predictor = 0, 0, 2;
    // A string is written: 0 in the shorter of its two forms (a literal on a tie, and a literal is never the
    // longer one), 1 in hexadecimal.
    string_form = 0, 0, 1;

    // The effort. From this effort on there are no budgets: every stream gets the effort's full settings and
    // predictors are ranked on the whole image (101: never).
    full_from = 100, 0, 101;
    // On the fast path, from this effort on predictors are chosen by encoding the candidates, below it from
    // an estimate.
    trial_predictors_from = 10, 0, 20;
    // From this effort on, below 20, the streams that take the most room get the strong encoder's quick
    // settings within a budget (20: never).
    bridge_from = 10, 0, 20;
    // That budget at `bridge_from`, in 256ths of the file's size,
    bridge_start = 16, 0, 1 << 20;
    // and what it is multiplied by for each point of effort above, in percent.
    bridge_growth = 200, 100, 1000;
    // On that bridge, a Flate stream that effort 0's settings cannot shrink is kept as it is (1), or gets the
    // strong encoder all the same (0).
    bridge_test = 1, 0, 1;
    // Below effort 20, a stream other than an image that decodes to more than this gets the faster level of
    // `big_level_`.
    big_stream = 4 << 20, 0, ANY;
    // The fast encoder's level for images at the efforts 0 to 19.
    image_level_0 = 7, 1, TOP;
    image_level_1 = 7, 1, TOP;
    image_level_2 = 7, 1, TOP;
    image_level_3 = 9, 1, TOP;
    image_level_4 = 9, 1, TOP;
    image_level_5 = 9, 1, TOP;
    image_level_6 = 10, 1, TOP;
    image_level_7 = 10, 1, TOP;
    image_level_8 = 11, 1, TOP;
    image_level_9 = 11, 1, TOP;
    image_level_10 = 12, 1, TOP;
    image_level_11 = 12, 1, TOP;
    image_level_12 = 12, 1, TOP;
    image_level_13 = 12, 1, TOP;
    image_level_14 = 12, 1, TOP;
    image_level_15 = 12, 1, TOP;
    image_level_16 = 12, 1, TOP;
    image_level_17 = 12, 1, TOP;
    image_level_18 = 12, 1, TOP;
    image_level_19 = 12, 1, TOP;
    // The fast encoder's highest level for a stream over `big_stream` at the efforts 0 to 19.
    big_level_0 = 9, 1, TOP;
    big_level_1 = 9, 1, TOP;
    big_level_2 = 9, 1, TOP;
    big_level_3 = 9, 1, TOP;
    big_level_4 = 9, 1, TOP;
    big_level_5 = 9, 1, TOP;
    big_level_6 = 10, 1, TOP;
    big_level_7 = 10, 1, TOP;
    big_level_8 = 10, 1, TOP;
    big_level_9 = 10, 1, TOP;
    big_level_10 = 12, 1, TOP;
    big_level_11 = 12, 1, TOP;
    big_level_12 = 12, 1, TOP;
    big_level_13 = 12, 1, TOP;
    big_level_14 = 12, 1, TOP;
    big_level_15 = 12, 1, TOP;
    big_level_16 = 12, 1, TOP;
    big_level_17 = 12, 1, TOP;
    big_level_18 = 12, 1, TOP;
    big_level_19 = 12, 1, TOP;
    // Decoded bytes that may be re-encoded in all below `full_from`: this many per byte of the file at
    // effort 0,
    quick_budget_factor = 16, 0, 1 << 20;
    // twice as many every so many points of effort,
    quick_budget_doubling = 10, 1, 100;
    // and never fewer than this.
    quick_budget_floor = 4 << 20, 0, ANY;
    // Extra encoder work per byte of the file from effort 20, in hundredths of quick-encoding one decoded
    // byte: this much at effort 1,
    work_budget_start = 100, 0, 1 << 20;
    // twice as much every so many points.
    work_budget_doubling = 10, 1, 100;
    // The cost of a decoded byte at the settings of the efforts 20 to 39, 40 to 59, 60 to 79 and 80 to 99, in
    // percent of its cost at the quick settings. They follow the Flate encoder's settings for those efforts.
    work_cost_20 = 170, 100, 100_000;
    work_cost_40 = 200, 100, 100_000;
    work_cost_60 = 310, 100, 100_000;
    work_cost_80 = 390, 100, 100_000;
    // A stream too big for the effort's settings is offered the lower steps of 20 before the quick settings
    // (1), or the quick settings at once (0).
    step_down = 1, 0, 1;
    // The budgets go to the streams that take the most room first (0), or in the order of their object
    // numbers (1).
    budget_order = 0, 0, 1;

    // A stream job checks its own new data only when the stream decodes to at least this many times its
    // stored size (the self-check then takes it by its hash). Speed only.
    job_check_ratio = 2, 1, ANY;
}

const COUNT: usize = RewriteRules::RULES.len();

/// The rules of the rewrite, each at its default until `set`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewriteRules([u64; COUNT]);

impl Default for RewriteRules {
    fn default() -> RewriteRules {
        RewriteRules(std::array::from_fn(|i| RewriteRules::RULES[i].1))
    }
}

impl RewriteRules {
    /// Sets the rule `name`. False, and nothing changes: no such rule, or a value outside its lowest and
    /// highest. Nothing is clamped silently.
    pub fn set(&mut self, name: &str, value: u64) -> bool {
        match RewriteRules::RULES.iter().position(|r| r.0 == name) {
            Some(i) if (RewriteRules::RULES[i].2..=RewriteRules::RULES[i].3).contains(&value) => {
                self.0[i] = value;
                true
            }
            _ => false,
        }
    }

    /// The value of the rule `name`; None: no such rule.
    pub fn get(&self, name: &str) -> Option<u64> {
        RewriteRules::RULES.iter().position(|r| r.0 == name).map(|i| self.0[i])
    }

    /// A rule's value.
    pub(crate) fn at(&self, rule: Rule) -> u64 {
        self.0[rule as usize]
    }

    /// A rule's value as a size (one that this machine cannot hold counts as the largest it can).
    pub(crate) fn size(&self, rule: Rule) -> usize {
        usize::try_from(self.at(rule)).unwrap_or(usize::MAX)
    }

    /// Whether an on/off rule is on.
    pub(crate) fn on(&self, rule: Rule) -> bool {
        self.at(rule) != 0
    }

    /// Cell `i` of the table whose first cell is `first` (the caller keeps `i` inside the table).
    pub(crate) fn cell(&self, first: Rule, i: usize) -> u64 {
        self.0[first as usize + i]
    }

    /// The default of a rule.
    pub(crate) fn default_of(rule: Rule) -> u64 {
        RewriteRules::RULES[rule as usize].1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_takes_any_value_in_its_range_and_no_other() {
        let unset = RewriteRules::default();
        let mut names = std::collections::HashSet::new();
        for (i, &(name, default, lowest, highest)) in RewriteRules::RULES.iter().enumerate() {
            assert!(names.insert(name), "{name} is there twice");
            assert!(lowest <= default && default <= highest, "{name}");
            assert_eq!(unset.get(name), Some(default), "{name}");
            // The default set by hand is the same as unset.
            let mut r = RewriteRules::default();
            assert!(r.set(name, default) && r == unset, "{name}");
            for v in [lowest, highest] {
                assert!(r.set(name, v), "{name} = {v}");
                assert_eq!((r.get(name), r.0[i]), (Some(v), v), "{name}");
            }
            // Outside the range nothing changes.
            let before = r.clone();
            assert!(!r.set(name, highest + 1) && (lowest == 0 || !r.set(name, lowest - 1)), "{name}");
            assert_eq!(r, before, "{name}");
        }
        let mut r = RewriteRules::default();
        assert!(!r.set("no_such_rule", 1) && !r.set("", 0) && r == unset);
        assert_eq!(r.get("no_such_rule"), None);
    }

    #[test]
    fn the_tables_have_their_cells_in_a_row() {
        let r = RewriteRules::default();
        let name = |first: Rule, i: usize| RewriteRules::RULES[first as usize + i].0;
        for e in 0..20 {
            assert_eq!(name(Rule::image_level_0, e), format!("image_level_{e}"));
            assert_eq!(name(Rule::big_level_0, e), format!("big_level_{e}"));
        }
        for band in 0..5 {
            assert_eq!(name(Rule::predictor_sample_0, band), format!("predictor_sample_{}", band * 20));
        }
        for band in 0..4 {
            assert_eq!(name(Rule::work_cost_20, band), format!("work_cost_{}", 20 + band * 20));
        }
        assert_eq!(r.cell(Rule::image_level_0, 19), 12);
        assert_eq!(r.at(Rule::job_check_ratio), 2);
        assert_eq!(RewriteRules::default_of(Rule::max_predictor_image), 48 << 20);
        assert_eq!(r.size(Rule::big_stream), 4 << 20);
    }
}
