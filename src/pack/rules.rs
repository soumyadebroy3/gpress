//! The archive's tuning values as named rules (`Options::archive_rules`): each has a default, which is what
//! the archive did when these were constants, and a lowest and a highest value. Any value in its range is safe:
//! every archive is restored and compared before it is returned, and memory and time stay bounded.
//!
//! The first `SHARED` rules are the ones `expand` must share with `compress`. An archive made with all of
//! them at their defaults is version 9, as before. With any of them changed it is version 10, whose header
//! lists the changed values (the profile); `expand` takes them from there, never from its caller. The other
//! rules are choices of the packing side only: the archive stores their outcome, so they never change the
//! version.

/// One line per rule: what it decides, then `name = default, lowest, highest;`. The rows of the shared rules
/// are part of the version-10 format (a profile names a rule by its row): a new shared rule goes after the
/// last shared one, and none is ever moved or removed.
macro_rules! rules {
    ($( $(#[$doc:meta])* $name:ident = $def:expr, $lo:expr, $hi:expr; )*) => {
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy)]
        enum Row { $($name),* }
        const RULES: &[(&str, u64, u64, u64)] = &[$((stringify!($name), $def, $lo, $hi)),*];
        impl ArchiveRules {
            $( $(#[$doc])* #[inline] pub fn $name(&self) -> u64 { self.v[Row::$name as usize] } )*
        }
    };
}

rules! {
    // ---- Shared with `expand` (the profile of a version-10 archive). The general model:
    /// How strongly the mixer changes its weights after each bit. Highest 8: the error times the rate must fit
    /// in 16 bits. Lowest 1: at 0 the mixer would never learn.
    mixer_rate = 6, 1, 8;
    /// The scale of each weight change (a shift; larger is slower): an input (at most 2047) times the error
    /// (at most 4095 times the rate, 8 at most) is below 2^26, shifted right by this, in 1/65536 of a whole
    /// weight. Lowest 4: one bit can then move a weight by up to 64 whole; highest 20: by at most 64/65536,
    /// where the mixer barely learns.
    mixer_shift = 12, 4, 20;
    /// The weight every prediction starts with, in 1/65536: from 0 (the prediction is not used) to 65536 (a
    /// whole weight).
    mixer_start = 16384, 0, 65536;
    /// The constant input the mixer can weigh. Lowest 0: none; highest 2047, the largest input a prediction
    /// gives.
    mixer_bias = 256, 0, 2047;
    /// The resolution of each final correction map: 2^bits + 1 points (33 at 5). Lowest 3 (9 points), the
    /// coarsest map the code builds (it shifts the map's input right by at most 2 bits); highest 5, what a map
    /// has room for (more would cost every archive memory and time).
    apm_bits = 5, 3, 5;
    /// From what part of a step a map's point moves, in quarters of a step. Lowest 1 and highest 4 (a whole
    /// step): only within them does a point never pass the end of the scale.
    apm_round = 1, 1, 4;
    /// How fast the first final correction (by kind and partial byte) learns: steps of 1/2^(rate - 1). Lowest
    /// 3, a step of 1/4, the largest that `apm_round` (quarters of a step) works with; highest 16, a step of
    /// 1/32768, the smallest that still moves a 16-bit point.
    apm1_rate = 7, 3, 16;
    /// The same for the second final correction (by the last byte and partial byte), with the same range.
    apm2_rate = 7, 3, 16;
    /// The share of the first correction in the final probability, in sixteenths. Lowest 0: not used; with
    /// `blend_apm2` at most the whole (8 each); the mixer's own result gets the rest.
    blend_apm1 = 4, 0, 8;
    /// The share of the second correction in the final probability, in sixteenths, as `blend_apm1`.
    blend_apm2 = 8, 0, 8;
    /// The highest count a context's bit history keeps. Lowest 1: at 0 no bit is counted, and a 0 bit sends a
    /// context back to state 0, "nothing seen"; highest 7, what a 3-bit count holds (a state holds two of them).
    state_count = 7, 1, 7;
    /// The opposite count is halved when it is above this: 0 (always) to 7 (never).
    state_discount = 2, 0, 7;
    /// How long the per-state probabilities keep slowing down. Lowest 0: every update moves half the way;
    /// highest 1023, the most the 10-bit count holds.
    state_limit = 1023, 0, 1023;
    /// The shortest repeat the long-match model follows, in bytes. 1 to 8: the model keeps the last 8 bytes.
    match_min = 5, 1, 8;
    /// How far back a new match is checked, in bytes. Lowest 1, the byte before; highest 256, so that the work
    /// per byte stays bounded.
    match_check = 64, 1, 256;
    /// The match table has 2^bits positions of 4 bytes. Lowest 8: 256 positions, as many as one byte has
    /// values; highest 22 (16 MiB), the size of the fax model's largest tables.
    match_bits = 20, 8, 22;
    /// The match length up to which every length has its own probability. Lowest 0: one for all lengths;
    /// highest 63: the table has 128.
    match_cap = 63, 0, 63;
    /// From this match length the mixer uses its weights for long matches. Lowest 1: every match is long;
    /// highest 2^31 (2 GiB, the largest part): as good as none is.
    match_long = 24, 1, 1 << 31;
    /// The count cap of the match probabilities. Lowest 0: every update moves half the way; highest 1023, the
    /// most the 10-bit count holds.
    match_limit = 1023, 0, 1023;
    /// A match at least this long codes the next byte with one decision instead of eight. Lowest 1: only a
    /// running match has a byte to offer. At the highest it never happens within a part.
    repeat_min = 128, 1, 1 << 31;
    /// The count cap of the long-repeat decision, with the range of `match_limit`.
    repeat_limit = 1023, 0, 1023;
    /// The number of expected bytes that came true up to which every count has its own probability. Lowest 0:
    /// one for all counts; highest 15: the table has 128.
    expect_cap = 15, 0, 15;
    /// The count cap of the expected-text probabilities, with the range of `match_limit`.
    expect_limit = 1023, 0, 1023;
    /// The model's ten tables have at least 2^bits buckets of 16 bytes each (2.5 MiB in all at 14). Lowest 8:
    /// 256 buckets, as many as one byte has values; highest 20, as `table_bits_max`.
    table_bits_min = 14, 8, 20;
    /// ... and at most 2^bits buckets (160 MiB in all at 20, the highest: the bound it always had). Lowest 8,
    /// as `table_bits_min`. When the least is above the most, the least counts.
    table_bits_max = 20, 8, 20;
    /// Between the two, a table grows until it has the file's length / 2^shift buckets. Lowest 0: a bucket for
    /// every byte; highest 16: one for every 64 KiB, so that from the default least (14) the tables grow only
    /// for files over 1 GiB, the default `max_len`.
    table_shift = 3, 0, 16;
    /// How often the two built-in texts run through the model before a file's first byte; 0: not at all.
    /// Highest 8: each round costs about 2 ms a model (expanding a 4 KB file at effort 100: 30 ms with none,
    /// 34 ms with 2, 45 ms with 8).
    prime_rounds = 2, 0, 8;
    /// Bytes of the original's SHA-256 an archive stores and checks. Lowest 16, what it always was (fewer
    /// would weaken "never a wrong file"); highest 32, all of it.
    hash_bytes = 16, 16, 32;
    /// How far before a stream stored without a filter the mark of a Type 1 font is looked for, in bytes; 0:
    /// such a font is coded as raw data. Highest 65536, as far back as a stream's dictionary is looked for.
    font_window = 4096, 0, 65536;
    // The fax image model:
    /// The two hashed tables of the fax model have at least 2^bits counters of 4 bytes. Lowest 8, as the
    /// general model's tables; highest 22, as `fax_bits_max`.
    fax_bits_min = 12, 8, 22;
    /// ... and at most 2^bits (16 MiB each at 22, the highest: the bound it always had). Lowest 8, as
    /// `fax_bits_min`. When the least is above the most, the least counts.
    fax_bits_max = 22, 8, 22;
    /// Between the two, 2^bits is the smallest power of two above the file's length, times 2^this. Lowest 0:
    /// that power alone; highest 16: then a file of 64 bytes, the least a fax stream is coded from by default
    /// (`fax_min_bytes`), already gets 2^23, more than the most.
    fax_bits_add = 4, 0, 16;
    /// The count cap of the small counters for the original encoder's layout choices, with the range of
    /// `match_limit`.
    fax_small_limit = 255, 0, 1023;
    /// The learning rate of the fax model's two mixers (a shift; larger is slower): a stretched input (at
    /// most 2840) times the error (at most 65535) is below 2^28, shifted right by this, in 1/65536 of a whole
    /// weight. Lowest 4: one pixel can then move a weight by up to 177.5 whole (weights stay within 256 either
    /// way); highest 24: by at most 11/65536.
    fax_mixer_shift = 15, 4, 24;
    /// The weight every prediction of the fax model starts with, in 1/65536, with the range of `mixer_start`.
    fax_mixer_start = 19660, 0, 65536;
    /// The resolution of the fax model's two correction maps: 2^bits + 1 points. Lowest 3 (9 points), as
    /// `apm_bits`; highest 6 (65 points: 8.125 MiB for the larger map, 1.016 MiB for the smaller).
    fax_apm_bits = 5, 3, 6;
    /// The learning rate of those maps (a shift). Lowest 1: a point moves half the way; highest 15: 1/32768 of
    /// it, the smallest step that still moves a 16-bit point.
    fax_apm_shift = 6, 1, 15;
    /// The count cap of the fax model's direct tables, its white-area counters and its same-row counters,
    /// with the range of `match_limit`.
    fax_limit = 1023, 0, 1023;
    /// The count cap of the fax model's hashed tables, with the range of `match_limit`.
    fax_hashed_limit = 1023, 0, 1023;
    /// The share of the first correction map in a pixel's final probability, in sixteenths (as `blend_apm1`).
    fax_blend_apm1 = 4, 0, 8;
    /// The share of the second correction map, in sixteenths (as `blend_apm1`).
    fax_blend_apm2 = 8, 0, 8;
    // Tables and switches of the general model:
    /// Differences between the pixels of a photo-like image are told apart in 8 classes: a difference's class
    /// is the number of these seven limits it is above (0, 1, 2, 4, 8, 16, 32). Each from 0 to 255, the sizes
    /// a difference between two bytes can have; no order is asked for.
    image_step_1 = 0, 0, 255;
    /// As `image_step_1`.
    image_step_2 = 1, 0, 255;
    /// As `image_step_1`.
    image_step_3 = 2, 0, 255;
    /// As `image_step_1`.
    image_step_4 = 4, 0, 255;
    /// As `image_step_1`.
    image_step_5 = 8, 0, 255;
    /// As `image_step_1`.
    image_step_6 = 16, 0, 255;
    /// As `image_step_1`.
    image_step_7 = 32, 0, 255;
    /// What counts as a word: 0 letters and digits with the letters lower-cased, 1 letters and digits as
    /// they are, 2 letters only, lower-cased.
    word_bytes = 0, 0, 2;
    /// The expected-text model (the next object header, cross-reference rows, the operand at the same place
    /// as in the last group): 1 on, 0 off.
    expect_text = 1, 0, 1;
    /// A pixel whose whole neighbourhood is white is coded with one small counter instead of the full fax
    /// model: 1 on, 0 off. Off, on the 17 files of the test set with a fax image (effort 100, one thread):
    /// from 0.37% smaller to 27.2% larger (1.0% larger in all), and compress and expand up to 3.7 times as slow
    /// (1.7 times in all). The most time a file can take stays what it was, as a page of noise goes through
    /// the full model at every pixel either way.
    fax_white_shortcut = 1, 0, 1;

    // ---- Choices of the packing side only (never in a profile).
    /// Below this effort, data that looks compressed is coded plainly (faster, about 1% larger). 0: never;
    /// 101: at every effort.
    plain_below = 50, 0, 101;
    /// How random bytes must look to be coded plainly, in hundredths of a bit per byte. Lowest 0: not at all;
    /// highest 800, the most a byte holds.
    plain_entropy = 790, 0, 800;
    /// Data smaller than this is never coded plainly. Up to the largest part an archive holds (2 GiB).
    plain_min_bytes = 1024, 0, 1 << 31;
    /// Larger data is judged on this many windows spread over it. Lowest 2: the first and the last; highest
    /// 65536: at the default window of 64 KiB that is 4 GiB, more than a part holds (2 GiB).
    plain_windows = 16, 2, 65536;
    /// The length of each window in bytes: from 1 to 2^31, the largest part.
    plain_window_bytes = 65536, 1, 1 << 31;
    /// The differences between bytes 1 to this many apart must look random too. Lowest 0: the bytes alone
    /// decide; highest 8: the test keeps a count table for each of 8 distances (each adds a count per byte).
    plain_distances = 4, 0, 8;
    /// A JPEG inside a Flate stream goes to Lepton when Lepton's data is this many percent smaller than the
    /// Flate bytes: from 0 (smaller at all) to 100 (never). Only in builds with Lepton, as the next four.
    lepton_min_saving = 10, 0, 100;
    /// A JPEG smaller than this is not tried with Lepton: from 0 (every one) to 2^31, the largest part (none).
    lepton_min_bytes = 1024, 0, 1 << 31;
    /// Lepton's data is kept when it is at least this many bytes smaller than the JPEG. Lowest 0: it is never
    /// larger than the JPEG it restores; highest 2^31, the largest part (it is never kept).
    lepton_min_gain = 1, 0, 1 << 31;
    /// Lepton's data that looks compressed is coded plainly: 1 yes, 0 never.
    lepton_plain = 1, 0, 1;
    /// Lepton codes a large JPEG in up to this many pieces (fewer: a little smaller). Lowest 1, one piece;
    /// highest 8, the most the Lepton readers before this one take.
    lepton_partitions = 8, 1, 8;
    /// A second pack without decryption is made when the pack is above this many percent of the encrypted
    /// bytes, and the smaller kept. 0: always; 100: only when the pack is larger than those bytes.
    decrypt_recheck = 98, 0, 100;
    /// When the two packs are equally long: 0 keeps the one without decryption, 1 the one with.
    decrypt_tie = 0, 0, 1;
    /// A coded archive is kept only when it is at least this many bytes smaller than the file stored as it
    /// is. Lowest 0: when it is not larger; highest 2^31: only a file that shrinks by 2 GiB is coded.
    min_saving = 0, 0, 1 << 31;
    /// A fax stream smaller than this is stored as it is, not as an image: from 0 to 2^31, the largest part
    /// (no fax stream is then coded as an image).
    fax_min_bytes = 64, 0, 1 << 31;
    /// A fax stream is stored as an image when at most this many percent of it is left undecoded: from 0
    /// (only one decoded to its end) to 100 (any).
    fax_max_leftover = 25, 0, 100;
    /// A stream with no type is taken for page content when, of its first this many bytes (from 1 to 2^24:
    /// at most 16 MiB of each such stream is read for it) ...
    content_sample = 4096, 1, 1 << 24;
    /// ... at least this many percent are printable.
    content_printable = 90, 0, 100;
    /// How many decrypted bytes must be text before a font's encrypted part counts as found. A wrong guess
    /// costs nothing but time: a font is opened only if it closes back to the same bytes. Highest 4096: each
    /// try then decrypts at most 4 KiB and 4 bytes.
    font_min_text = 32, 0, 4096;
    /// The bytes that count as text there: 0 tab, line ends and 32 to 126; 1 also 160 to 255 (Latin-1).
    font_text_bytes = 0, 0, 1;
    /// At how many places the word "eexec" appears the encrypted part of a font is looked for. Highest 64,
    /// so that the search stays short.
    font_tries = 4, 0, 64;
    /// The zlib levels tried to rebuild a stream byte for byte, as bits (1 is level 1, 256 is level 9).
    /// 0: no stream is rebuilt.
    zlib_levels = 511, 0, 511;
    /// The zlib memory levels tried, as bits (1 is memory level 1, 256 is 9).
    zlib_mem_levels = 511, 0, 511;
    /// zlib's "filtered" strategy is tried too: 1 yes, 0 no.
    zlib_filtered = 1, 0, 1;
    /// Bytes coded between two looks at the time budget, in `compress` and `expand` (a look comes at the
    /// next 4 KiB of a part). Never changes the output. Lowest 1: a look every 4 KiB; highest 2^31 (2 GiB, the
    /// largest part): in effect a look only where a run of coded bytes starts.
    deadline_step = 65536, 1, 1 << 31;
}

/// The number of shared rules: rows `0..SHARED` (see the module's text).
const SHARED: usize = Row::plain_below as usize;
const _: () = assert!(SHARED < 256, "a profile names a rule in one byte");

/// The archive's rules. `ArchiveRules::default()` is what the archive does when nothing is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveRules {
    v: [u64; RULES.len()],
}

impl Default for ArchiveRules {
    fn default() -> ArchiveRules {
        ArchiveRules { v: std::array::from_fn(|i| RULES[i].1) }
    }
}

impl ArchiveRules {
    /// (name without the `archive_` prefix, default, lowest, highest), in a fixed order.
    pub const RULES: &'static [(&'static str, u64, u64, u64)] = RULES;
    /// The first this many rules of `RULES` are shared with expand: with one of them changed an archive is
    /// version 10.
    pub const SHARED: usize = SHARED;

    /// Sets a rule. false: no such rule, or a value outside its range (nothing changes then).
    pub fn set(&mut self, name: &str, value: u64) -> bool {
        match RULES.iter().position(|r| r.0 == name) {
            Some(i) if (RULES[i].2..=RULES[i].3).contains(&value) => {
                self.v[i] = value;
                true
            }
            _ => false,
        }
    }

    pub fn get(&self, name: &str) -> Option<u64> {
        RULES.iter().position(|r| r.0 == name).map(|i| self.v[i])
    }

    /// The seven limits of `image_step_1` to `image_step_7`.
    #[cfg_attr(not(feature = "archive"), allow(dead_code))]
    pub(crate) fn image_steps(&self) -> &[u64] {
        &self.v[Row::image_step_1 as usize..=Row::image_step_7 as usize]
    }
}

#[cfg(feature = "archive")]
impl ArchiveRules {
    /// These rules with every shared one back at its default: what a decoder starts from, since the shared
    /// values are the archive's, never its caller's.
    pub(super) fn local(&self) -> ArchiveRules {
        let mut r = self.clone();
        r.v[..SHARED].copy_from_slice(&ArchiveRules::default().v[..SHARED]);
        r
    }

    /// The profile a version-10 header carries: the number of shared values that differ from their defaults
    /// (one byte), then each as its row (one byte) and its value (a base-128 number), rows rising. Empty when
    /// none differs: the archive is then version 9, which has no profile.
    pub(super) fn profile(&self) -> Vec<u8> {
        let changed: Vec<usize> = (0..SHARED).filter(|&i| self.v[i] != RULES[i].1).collect();
        let mut out = Vec::new();
        if !changed.is_empty() {
            out.push(changed.len() as u8);
            for i in changed {
                out.push(i as u8);
                super::push_varint(&mut out, self.v[i]);
            }
        }
        out
    }

    /// Reads a profile from `a` at `i` into `r`, whose shared rules are at their defaults: the rules it means
    /// and the index after it. None for anything `profile` never writes: no entry, a row that is not a shared
    /// rule or not above the one before, a value outside its rule's range or equal to its default, or a
    /// profile that is cut. Everything here is hostile input, and nothing is sized by a value before this has
    /// checked it.
    pub(super) fn read_profile(a: &[u8], mut i: usize, mut r: ArchiveRules) -> Option<(ArchiveRules, usize)> {
        let n = *a.get(i)? as usize;
        i += 1;
        let mut next = 0;
        for _ in 0..n {
            let row = *a.get(i)? as usize;
            let (v, after) = super::read_varint(a, i + 1)?;
            if row < next || row >= SHARED || v == RULES[row].1 || !(RULES[row].2..=RULES[row].3).contains(&v) {
                return None;
            }
            r.v[row] = v;
            next = row + 1;
            i = after;
        }
        (n > 0).then_some((r, i))
    }
}

#[cfg(all(test, feature = "archive"))]
mod tests {
    use super::*;

    #[test]
    fn a_profile_reads_back_as_the_shared_rules() {
        // None changed: no profile (version 9).
        assert!(ArchiveRules::default().profile().is_empty());
        // Every shared rule at its lowest, then at its highest, then every other one changed; with the rules of
        // the packing side changed too, which a profile leaves out.
        for k in 0..3 {
            let mut r = ArchiveRules::default();
            for (i, &(name, def, lo, hi)) in RULES.iter().enumerate() {
                let v = [lo, hi, if i % 2 == 0 { lo } else { hi }][k];
                assert!(r.set(name, if v == def && k == 2 { hi } else { v }));
            }
            let mut a = vec![7u8; 3];
            a.extend(r.profile());
            a.push(9);
            let (back, end) = ArchiveRules::read_profile(&a, 3, r.local()).unwrap();
            assert_eq!(back, r);
            assert_eq!(a[end], 9);
            // From the defaults: the shared values, and the defaults for the rest.
            let mut shared = ArchiveRules::default();
            shared.v[..SHARED].copy_from_slice(&r.v[..SHARED]);
            assert_eq!(ArchiveRules::read_profile(&a, 3, ArchiveRules::default()).unwrap().0, shared);
        }
    }
}
