//! The public face of gpress: one options type, three operations, typed results, and no panics.
//!
//! Everything a caller can tune lives in [`Options`]. The only constants that are not options are the
//! ones that define the file formats (a decoder must agree with its encoder on those), the ones PDF readers
//! set, and measured thresholds; `docs/reference.md` lists them.
//!
//! The three operations:
//! - [`optimize`]: a smaller PDF that any PDF reader opens, with every object's value preserved at efforts 0
//!   to 40 (from 60 its images may change, within floors: see [`Effort`]).
//! - [`compress`]: the gpress archive (`.gpz`), restored byte for byte by [`expand`].
//! - [`expand`]: the original bytes back from an archive, or an error; never a wrong file.
//!
//! Every call checks the options and the input against the limits first, runs the engine inside a panic
//! guard (a bug can never take the host process down, which matters for the C and WebAssembly bindings),
//! and returns a plain [`Error`] with a stable code instead.
//!
//! The output is the same bytes for any thread count, on any machine, with or without a time budget: the
//! engine decides from sizes only, never from time. A budget that runs out gives [`Error::TimeBudget`] and no
//! output, in all three operations.
//!
//! Memory on hostile input follows the options rather than what a file claims: `max_input` for the input,
//! `max_stream` for each filter stage the rewrite decodes from a stream it may re-encode (a stream with no
//! filter we decode, an image codec's say, is held at its stored size, within `max_input`) and for the object
//! streams it reads, which are also bounded held at once and in all (see [`Options::max_stream`]),
//! `max_stream_archive` for each Flate stream compress decodes and for the object streams it reads to open an
//! encrypted file, and for the archive `max_decoded` (what the parts of one archive may claim in all, declared
//! in its header and refused up front when over the caller's limit) with `max_len` (the restored length,
//! likewise). Not yet bounded by an option: cross-reference streams are decoded as far as the file's size
//! allows, the PDF/A check reads metadata to 16 MiB, and the objects parsed from object streams take more
//! than the bytes the limits count (an array of small integers about 35 times its bytes). In optimize, and in compress's planning, up to
//! `threads` streams are decoded at once, each with its own copies, so the peak grows with `threads`; expand
//! runs on one thread, and compress's coding on one with its restore check beside it.
//! The encoded outputs optimize keeps for a file are bounded by `store_budget`.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use crate::clock::Deadline;
#[cfg(feature = "archive")]
use crate::pack;
// Without the archive: its format's constants, which the options and `is_archive` use in every build.
#[cfg(not(feature = "archive"))]
use crate::off::pack;
pub use crate::pdf::lossy::Lossy;
use crate::pdf::rewrite;
use crate::Feature;

/// Library version, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The archive format version this build writes and reads.
pub const ARCHIVE_VERSION: u8 = pack::VERSION;

/// Most worker threads a caller may ask for (a thread pool never runs more).
pub const MAX_THREADS: usize = crate::par::MAX_THREADS;

/// The largest `store_budget` a caller may ask for (a store never keeps more).
pub const MAX_STORE_BUDGET: usize = crate::flate::deflate::MAX_STORE_BUDGET;

/// How much to do, 0 to 100 in steps of 20. A value in between counts as the step below it (59 is 40).
///
/// 0, 20 and 40 are lossless: every object's value is kept. 0 is the fast path, 20 the strong encoder's first
/// step, and 40 (the default) the smallest lossless output: everything the engine knows how to do.
///
/// 60, 80 and 100 are lossy: the work of 40, after a step that changes images only. At 60 an image shown at
/// more than 300 pixels per inch is resampled down to 300, and a photo that is resampled, or stored without
/// JPEG, becomes a JPEG at quality 85 (line art stays lossless Flate). 80 is the same with photos down to 150
/// pixels per inch. 100 is as 80 at JPEG quality 75, and an existing JPEG photo is written again at that
/// quality when that is at least a tenth smaller. Floors that hold at every lossy level: a photo is never
/// taken below 150 pixels per inch at its displayed size, and line art (screenshots, charts, diagrams) and
/// scans never below 300; 1-bit images and small images (at most 65,536 pixels) are never touched; a lossy
/// result is kept only when it is clearly smaller than what the lossless path makes of the image and a
/// per-image quality check passes; a file the safety gate leaves untouched (signed, PDF/A, PDF/X, needing a
/// password) stays untouched, and an encrypted file keeps its encryption.
///
/// The archive ([`compress`]) is always lossless: there 40 and above all mean its strongest setting.
///
/// The engine has a finer lossless scale of its own, 0 to 100 (`pdf::rewrite::Options::effort`,
/// `pack::PackOptions::effort`): steps 0 and 20 are its 0 and 20, and 40 and above are its 100.
/// [`Options::engine_effort`] sets it directly, for measuring.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Effort(u8);

impl Effort {
    pub const MIN: Effort = Effort(0);
    pub const MAX: Effort = Effort(100);
    /// Clamped to 0..=100.
    pub fn new(level: u8) -> Effort {
        Effort(level.min(100))
    }
    pub fn level(self) -> u8 {
        self.0
    }
    /// The step this effort counts as: the multiple of 20 at or below it.
    pub fn step(self) -> u8 {
        self.0 / 20 * 20
    }
}

impl Default for Effort {
    /// 40: the smallest lossless output.
    fn default() -> Self {
        Effort(40)
    }
}

/// Everything a caller can tune. `Options::default()` is the smallest lossless output (effort 40) with the
/// limits the engine was measured with; the builder methods adjust one thing each, and `check` says whether a
/// set of options is one the engine accepts (every operation runs it first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Effort for both outputs, 0 to 100 (see [`Effort`]).
    pub effort: Effort,
    /// For the measuring tools, the benchmarks and the engine's tests: the engine's own lossless level, 0 to
    /// 100 (more counts as 100), in place of the one the effort means. Set, the effort decides nothing: the
    /// lossy step is off unless `lossy` is `Some`. `None`: the effort decides.
    pub engine_effort: Option<u8>,
    /// Worker threads for independent work: 0 = every core the machine offers, at most 8 for now (each thread
    /// holds a stream of its own, so memory grows with them); else at most [`MAX_THREADS`]. Output is identical
    /// whatever the count; this only changes speed. A thread the system refuses is no loss: the calling thread
    /// does the work.
    pub threads: usize,
    /// Normal PDF: bytes of encoded outputs one file keeps, so that an input of at most 256 KiB that another
    /// stream or step also encodes is encoded once (a larger repeat is encoded again); at most
    /// [`MAX_STORE_BUDGET`]. The oldest go first; 0 keeps none. Never changes the output. Nothing is kept
    /// from one call to the next.
    pub store_budget: usize,
    /// Largest input accepted, in bytes. Inputs over this return [`Error::TooLarge`] at once.
    pub max_input: usize,
    /// Normal PDF: largest decoded stream re-encoded, in bytes. A stream that decodes to more is kept as it is.
    /// It holds the file's object streams too, which are decoded once each (in an encrypted file whose metadata
    /// is not encrypted, the catalog's once more) and let go once their objects are read: each may decode to
    /// this much or 256 MiB, whichever is less, at each filter stage; the ones held at once to four times that,
    /// each header entry kept counted as 16 bytes; and all of them, their stored bytes and every stage counted,
    /// to four times that or 64 bytes per byte of the file, whichever is more. A file past any of these is left
    /// as it is (with 0, every file that has an object read from an object stream).
    pub max_stream: usize,
    /// Archive: largest decoded stream stored in its decoded form, in bytes. A stream that decodes to more
    /// is stored as it is. (The decoder takes up to 2 GiB per part, whatever this was.) The object streams
    /// read to open an encrypted file are held to it as `max_stream` holds them; past it, the file's encrypted
    /// streams are stored as they are.
    pub max_stream_archive: usize,
    /// Archive, both ways: the most decoded bytes the parts of one archive may claim in all (decoded streams,
    /// and Lepton data). Compress: in file order, a stream whose claim doesn't fit in what is left is stored
    /// as it is, decided from sizes alone; the header declares the total. Expand: an archive whose header
    /// declares more is refused with [`Error::DecodedTooLarge`] before anything is decoded, and every part
    /// is then held to what the header declared. Memory while expanding is about `max_len` for the output,
    /// `max_len + max_decoded` for the model's history, twice the largest part (at most `max_decoded`) and
    /// the model's tables (up to 164 MiB), so a service that takes archives from anyone sets this and
    /// `max_len` to what it can afford. An archive made with a budget restores wherever that budget is
    /// allowed.
    pub max_decoded: u64,
    /// Expand: largest file restored, in bytes. An archive whose header declares more is refused with
    /// [`Error::DeclaredTooLarge`] before anything is decoded or reserved for it. At most 16 GiB, the
    /// format's own limit; the default is the default `max_input`, which no archive made with default
    /// options exceeds.
    pub max_len: u64,
    /// Normal PDF: most objects per compressed object stream (at least 1).
    pub objects_per_stream: usize,
    /// Normal PDF: leave files that are PDF/A, PDF/X, signed, certified or carry usage rights untouched.
    /// `true` is the safe default; `false` still keeps every object's value but drops those guarantees.
    pub respect_conformance: bool,
    /// Normal PDF: also try our Flate in front of JPEG images (never JPEG 2000 or JBIG2).
    pub flate_over_jpeg: bool,
    /// Normal PDF: give JPEG images optimal Huffman tables (the same coefficients, read back and compared).
    pub jpeg_tables: bool,
    /// Normal PDF: write CCITT fax images as JBIG2 when that is clearly smaller (the same pixels).
    pub jbig2: bool,
    /// Normal PDF: rewrite files the empty user password opens, keeping their encryption exactly. Off,
    /// every encrypted file is left untouched.
    pub rewrite_encrypted: bool,
    /// Normal PDF: the lossy image step. `None`: what the effort means (efforts 60, 80 and 100 turn it on, see
    /// [`Effort`]); `Some`: exactly these settings whatever the effort (`Some(Lossy::OFF)` keeps any effort
    /// lossless).
    pub lossy: Option<Lossy>,
    /// Archive: store the streams of a file the empty user password opens decrypted (so they can be rebuilt
    /// and modelled) and encrypt them again on expand. Off, encrypted streams are stored as they are.
    pub decrypt: bool,
    /// Try the optional existing library (Lepton, for JPEG streams) when the build includes it (feature
    /// `existing`). An archive with parts it made expands only with such a build.
    pub use_existing_libraries: bool,
    /// Give up after this much wall time: the call then returns [`Error::TimeBudget`] and nothing else, and
    /// the input is unchanged. A call that finishes in time gives the same bytes as one without a budget
    /// (the engine never decides anything from time). Checked between streams, Flate segments and archive
    /// parts (every 64 KiB of model work) in all three operations, so a budget may be overshot by a little
    /// (on expand by one part's zlib re-creation). `None` = no limit. On a platform without a clock
    /// (WebAssembly without a host clock) there is no budget.
    pub time_budget: Option<Duration>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            effort: Effort::default(),
            engine_effort: None,
            threads: 0,
            store_budget: 2 << 20,
            max_input: 1 << 30,
            max_stream: 64 << 20,
            max_stream_archive: 256 << 20,
            max_decoded: pack::DEFAULT_MAX_DECODED,
            max_len: 1 << 30,
            objects_per_stream: 200,
            respect_conformance: true,
            flate_over_jpeg: true,
            jpeg_tables: true,
            jbig2: true,
            rewrite_encrypted: true,
            lossy: None,
            decrypt: true,
            use_existing_libraries: true,
            time_budget: None,
        }
    }
}

impl Options {
    pub fn new() -> Options {
        Options::default()
    }
    pub fn effort(mut self, level: u8) -> Options {
        self.effort = Effort::new(level);
        self
    }
    pub fn engine_effort(mut self, level: Option<u8>) -> Options {
        self.engine_effort = level;
        self
    }
    pub fn threads(mut self, n: usize) -> Options {
        self.threads = n;
        self
    }
    pub fn store_budget(mut self, bytes: usize) -> Options {
        self.store_budget = bytes;
        self
    }
    pub fn max_input(mut self, bytes: usize) -> Options {
        self.max_input = bytes;
        self
    }
    pub fn max_stream(mut self, bytes: usize) -> Options {
        self.max_stream = bytes;
        self
    }
    pub fn max_stream_archive(mut self, bytes: usize) -> Options {
        self.max_stream_archive = bytes;
        self
    }
    pub fn max_decoded(mut self, bytes: u64) -> Options {
        self.max_decoded = bytes;
        self
    }
    pub fn max_len(mut self, bytes: u64) -> Options {
        self.max_len = bytes;
        self
    }
    pub fn objects_per_stream(mut self, n: usize) -> Options {
        self.objects_per_stream = n;
        self
    }
    pub fn respect_conformance(mut self, yes: bool) -> Options {
        self.respect_conformance = yes;
        self
    }
    pub fn flate_over_jpeg(mut self, yes: bool) -> Options {
        self.flate_over_jpeg = yes;
        self
    }
    pub fn jpeg_tables(mut self, yes: bool) -> Options {
        self.jpeg_tables = yes;
        self
    }
    pub fn jbig2(mut self, yes: bool) -> Options {
        self.jbig2 = yes;
        self
    }
    pub fn rewrite_encrypted(mut self, yes: bool) -> Options {
        self.rewrite_encrypted = yes;
        self
    }
    pub fn lossy(mut self, lossy: Option<Lossy>) -> Options {
        self.lossy = lossy;
        self
    }
    /// One lossy setting changed at a time, for the bindings and the command line: each value that is not 0
    /// takes its place in the lossy settings these options mean (`lossy`, or else the effort's), and 0 leaves
    /// its setting as it is. All 0 changes nothing.
    pub fn lossy_overrides(mut self, photo_ppi: u16, art_ppi: u16, jpeg_quality: u8) -> Options {
        if (photo_ppi, art_ppi, jpeg_quality) != (0, 0, 0) {
            let mut lossy = self.lossy_settings();
            if photo_ppi != 0 {
                lossy.photo_ppi = photo_ppi;
            }
            if art_ppi != 0 {
                lossy.art_ppi = art_ppi;
            }
            if jpeg_quality != 0 {
                lossy.jpeg_quality = jpeg_quality;
            }
            self.lossy = Some(lossy);
        }
        self
    }
    pub fn decrypt(mut self, yes: bool) -> Options {
        self.decrypt = yes;
        self
    }
    pub fn use_existing_libraries(mut self, yes: bool) -> Options {
        self.use_existing_libraries = yes;
        self
    }
    pub fn time_budget(mut self, budget: Option<Duration>) -> Options {
        self.time_budget = budget;
        self
    }

    /// Whether the engine accepts these options; every operation checks this first.
    pub fn check(&self) -> Result<(), Error> {
        if self.threads > MAX_THREADS {
            return Err(Error::InvalidOptions(format!("threads is {}, more than {MAX_THREADS}", self.threads)));
        }
        if self.store_budget > MAX_STORE_BUDGET {
            return Err(Error::InvalidOptions(format!(
                "store_budget is {}, more than {MAX_STORE_BUDGET}",
                self.store_budget
            )));
        }
        if self.time_budget == Some(Duration::ZERO) {
            return Err(Error::InvalidOptions("a time budget of zero would stop at once".into()));
        }
        if self.max_input == 0 {
            return Err(Error::InvalidOptions("max_input is 0, so every input would be refused".into()));
        }
        if self.objects_per_stream == 0 {
            return Err(Error::InvalidOptions("objects_per_stream is 0; an object stream holds at least 1".into()));
        }
        Ok(())
    }

    /// Whether this build has the existing library (Lepton) at all.
    pub fn has_existing_libraries() -> bool {
        cfg!(feature = "existing")
    }

    fn deadline(&self) -> Deadline {
        self.time_budget.map_or(Deadline::NONE, Deadline::after)
    }

    /// The engine's lossless level these options mean: `engine_effort` when set, else the effort's step as the
    /// engine counts it (0 is 0, 20 is 20, 40 and above are 100).
    fn engine_level(&self) -> u8 {
        match self.engine_effort {
            Some(level) => level.min(100),
            None => match self.effort.step() {
                0 => 0,
                20 => 20,
                _ => 100,
            },
        }
    }

    /// The lossy settings these options mean: `lossy` when set, else the effort's (steps 60, 80 and 100 are
    /// `Lossy::level` 1, 2 and 3), and off with `engine_effort`.
    fn lossy_settings(&self) -> Lossy {
        let level = if self.engine_effort.is_some() { 0 } else { (self.effort.step() / 20).saturating_sub(2) };
        self.lossy.unwrap_or(Lossy::level(level))
    }

    /// The Flate encoder settings of the engine's level: the fast encoder below 20, the strong encoder's
    /// steps of 20 from there (see `flate::Options::effort`).
    pub(crate) fn flate(&self) -> crate::flate::Options {
        crate::flate::Options::effort(self.engine_level())
    }

    /// The Flate settings of the engine's level, for the command-line tool's fine-tuning flags.
    #[doc(hidden)]
    pub fn flate_for_cli(&self) -> crate::flate::Options {
        self.flate()
    }

    /// The normal-PDF settings these options mean.
    pub fn rewrite(&self) -> rewrite::Options {
        rewrite::Options {
            flate: self.flate(),
            objstm_size: self.objects_per_stream.max(1),
            max_stream: self.max_stream,
            effort: self.engine_level(),
            threads: self.threads,
            store_budget: self.store_budget,
            respect_conformance: self.respect_conformance,
            flate_over_jpeg: self.flate_over_jpeg,
            jbig2: self.jbig2,
            jpeg_tables: self.jpeg_tables,
            rewrite_encrypted: self.rewrite_encrypted,
            lossy: self.lossy_settings(),
            deadline: self.deadline(),
        }
    }

    /// The archive settings these options mean. The archive is always lossless: it takes the engine's level
    /// and nothing else from the effort.
    #[cfg(feature = "archive")]
    pub fn pack(&self) -> pack::PackOptions {
        pack::PackOptions {
            effort: self.engine_level(),
            threads: self.threads,
            decrypt: self.decrypt,
            max_stream: self.max_stream_archive,
            max_decoded: self.max_decoded,
            existing: self.use_existing_libraries && Options::has_existing_libraries(),
            deadline: self.deadline(),
        }
    }

    /// The expand settings these options mean.
    #[cfg(feature = "archive")]
    pub fn unpack(&self) -> pack::UnpackOptions {
        pack::UnpackOptions { max_len: self.max_len, max_decoded: self.max_decoded, deadline: self.deadline() }
    }
}

/// Why a call returned nothing. Every variant is a plain value: nothing here panics or carries a
/// backtrace, so the C and WebAssembly layers can pass it through as a code and a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The input is larger than `Options::max_input`.
    TooLarge { size: usize, limit: usize },
    /// Not a gpress archive, or a damaged one. Nothing was restored.
    Damaged(String),
    /// The archive's format version is not the one this build reads.
    UnsupportedVersion(u8),
    /// The engine hit a bug. The input is unchanged; please report it with the file.
    Internal(String),
    /// The options are outside what the engine accepts (see `Options::check`).
    InvalidOptions(String),
    /// The time budget ran out before the work was done. The input is unchanged.
    TimeBudget(Duration),
    /// Expand: the archive declares a restored size over `Options::max_len`. Nothing was decoded.
    DeclaredTooLarge { declared: u64, limit: u64 },
    /// Expand: the archive has parts made with the existing library (Lepton) and this build doesn't have it.
    NeedsExistingLibrary,
    /// Expand: the archive's parts claim more decoded bytes in all than `Options::max_decoded`. Nothing was
    /// decoded.
    DecodedTooLarge { declared: u64, limit: u64 },
    /// The call needs a part of the engine this build leaves out (a Cargo feature): `compress` and `expand`
    /// without the archive, or `expand` of an archive with parts that need fax or encryption. Nothing was
    /// done. A damaged archive whose damage reads as such a part gives it too: only a build with the part can
    /// tell the two apart (as with `NeedsExistingLibrary`, which an archive with Lepton's parts gives).
    /// (`optimize` skips a step whose feature is left out, and leaves an encrypted file untouched without
    /// `encryption`.)
    NeedsFeature(Feature),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooLarge { size, limit } => write!(f, "input is {size} bytes, over the limit of {limit}"),
            Error::Damaged(why) => write!(f, "{why}"),
            Error::UnsupportedVersion(v) => {
                write!(f, "gpress archive version {v} is not one this build reads (it reads version {ARCHIVE_VERSION})")
            }
            Error::Internal(why) => write!(f, "internal error: {why}"),
            Error::InvalidOptions(why) => write!(f, "invalid options: {why}"),
            Error::TimeBudget(budget) => write!(f, "the time budget of {} ms ran out", budget.as_millis()),
            Error::DeclaredTooLarge { declared, limit } => {
                write!(f, "the archive restores to {declared} bytes, more than the limit of {limit}")
            }
            Error::NeedsExistingLibrary => write!(
                f,
                "this gpress archive has parts made with the existing library (Lepton); expanding it needs a build with it"
            ),
            Error::DecodedTooLarge { declared, limit } => {
                write!(f, "the archive's parts claim {declared} decoded bytes, more than the limit of {limit}")
            }
            Error::NeedsFeature(feature) => f.write_str(&feature.missing()),
        }
    }
}

impl std::error::Error for Error {}

/// Numeric codes for the C and WebAssembly layers (stable across versions; 0 is success).
impl Error {
    pub fn code(&self) -> i32 {
        match self {
            Error::TooLarge { .. } => 1,
            Error::Damaged(_) => 2,
            Error::UnsupportedVersion(_) => 3,
            Error::Internal(_) => 4,
            Error::InvalidOptions(_) => 5,
            Error::TimeBudget(_) => 6,
            Error::DeclaredTooLarge { .. } => 7,
            Error::NeedsExistingLibrary => 8,
            Error::DecodedTooLarge { .. } => 9,
            Error::NeedsFeature(_) => 10,
        }
    }
}

/// What `optimize` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A smaller PDF with every object's value preserved, but for the images a lossy effort changed. The
    /// statistics say what changed.
    Smaller(Report),
    /// The input is returned exactly as it was, with the reason (encrypted with a password, signed,
    /// PDF/A, damaged, or nothing to gain).
    Untouched(String),
}

/// What changed in a normal-PDF optimisation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub objects: usize,
    pub streams: usize,
    pub streams_reencoded: usize,
    pub streams_kept: usize,
    pub object_streams: usize,
    pub unreachable_objects_dropped: usize,
    /// JPEG images written again with optimal Huffman tables (the same coefficients).
    pub jpegs_retabled: usize,
    /// CCITT fax images written as JBIG2 (the same pixels).
    pub fax_to_jbig2: usize,
    /// How the file is encrypted, kept exactly as it was (e.g. "R4/AESV2"); empty when it isn't.
    pub encryption: String,
}

/// A normal-PDF result.
#[derive(Clone, Debug)]
pub struct Optimized {
    pub data: Vec<u8>,
    pub outcome: Outcome,
}

/// What [`optimize_to_size`] picked for a target size.
#[derive(Clone, Debug)]
pub struct Picked {
    pub result: Optimized,
    /// The effort that made `result`: 0, 20, 40, 60, 80 or 100.
    pub effort: Effort,
    /// Whether `result.data` is at most the target. `false`: no effort reaches it, and `result` is the
    /// smallest of the six.
    pub reached: bool,
}

/// A gpress archive and what went into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compressed {
    pub data: Vec<u8>,
    /// The archive is the whole input stored as it was: nothing could be made smaller.
    pub stored: bool,
    /// Streams the model saw: decoded, with the existing library's help, or stored as they were.
    pub streams: usize,
    /// Streams stored decoded and rebuilt exactly on expand (the gain comes from these), and their bytes.
    pub streams_rebuilt: usize,
    pub rebuilt_bytes: usize,
    /// JPEG streams recompressed with the existing library (Lepton).
    pub streams_lepton: usize,
    /// Type 1 font programs stored with both layers of their encryption removed.
    pub type1_fonts: usize,
    /// CCITT fax images stored as their pixels.
    pub fax_images: usize,
    /// Encrypted streams stored decrypted.
    pub streams_decrypted: usize,
    /// Decoded bytes that went through the model.
    pub decoded_bytes: usize,
    /// The decoded bytes the archive's parts claim in all, as its header declares: what `max_decoded` must
    /// cover to expand it (at most this call's `max_decoded`).
    pub declared_decoded: u64,
}

fn guard<T>(what: &str, f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(Error::Internal(format!("{what}: {}", without_paths(&msg))))
        }
    }
}

/// A panic message without the source path a panic carries ("at src/x.rs:1:2" is the host's business,
/// not the caller's).
fn without_paths(msg: &str) -> String {
    msg.lines().next().unwrap_or("").trim().to_string()
}

fn check(input: &[u8], opt: &Options) -> Result<(), Error> {
    opt.check()?;
    if input.len() > opt.max_input {
        return Err(Error::TooLarge { size: input.len(), limit: opt.max_input });
    }
    Ok(())
}

/// A smaller PDF that any reader opens. At efforts 0 to 40 every object's value is preserved; from 60 the
/// lossy image step may change images, within the floors [`Effort`] gives, and every other value is preserved.
/// The output is read back and compared before it is returned; when in doubt the input comes back untouched.
pub fn optimize(input: &[u8], opt: &Options) -> Result<Optimized, Error> {
    check(input, opt)?;
    guard("optimize", || {
        let r = rewrite::optimize(input, &opt.rewrite());
        let outcome = match r.outcome {
            rewrite::Outcome::Smaller(s) => Outcome::Smaller(Report {
                objects: s.objects,
                streams: s.streams,
                streams_reencoded: s.streams_reencoded,
                streams_kept: s.streams_kept,
                object_streams: s.objstms,
                unreachable_objects_dropped: s.dropped_unreachable,
                jpegs_retabled: s.jpegs_retabled,
                fax_to_jbig2: s.fax_to_jbig2,
                encryption: s.encryption,
            }),
            rewrite::Outcome::Untouched(why) => Outcome::Untouched(why),
            rewrite::Outcome::TimedOut => return Err(Error::TimeBudget(opt.time_budget.unwrap_or_default())),
        };
        Ok(Optimized { data: r.data, outcome })
    })
}

/// A normal PDF of at most `target` bytes, at the lowest effort that reaches it: [`optimize`] at efforts 0, 20,
/// 40, 60, 80 and 100 in turn (so up to six runs of the engine, the other options as given), and the first
/// result that is small enough. When none is, the smallest of the six, with `reached` false. Efforts 60 and
/// above are lossy, within the floors [`Effort`] gives, so a target no effort reaches within them is missed
/// rather than forced. `opt.effort` is not used; `engine_effort` and `lossy` must be `None`
/// ([`Error::InvalidOptions`] otherwise). One time budget covers the whole call.
pub fn optimize_to_size(input: &[u8], target: u64, opt: &Options) -> Result<Picked, Error> {
    if opt.engine_effort.is_some() || opt.lossy.is_some() {
        return Err(Error::InvalidOptions(
            "a target size picks the effort itself: engine_effort and lossy must not be set".into(),
        ));
    }
    check(input, opt)?;
    let start = crate::clock::now();
    let mut best: Option<Picked> = None;
    for level in (0..=100u8).step_by(20) {
        let mut at = opt.clone().effort(level);
        // Each run gets what is left of the one budget.
        if let Some(budget) = opt.time_budget {
            let left = budget.saturating_sub(crate::clock::since(start));
            if left.is_zero() {
                return Err(Error::TimeBudget(budget));
            }
            at.time_budget = Some(left);
        }
        let result = optimize(input, &at).map_err(|e| match (e, opt.time_budget) {
            (Error::TimeBudget(_), Some(budget)) => Error::TimeBudget(budget),
            (e, _) => e,
        })?;
        let reached = result.data.len() as u64 <= target;
        // On a tie the lower effort stays: it changed less.
        if reached || best.as_ref().is_none_or(|b| result.data.len() < b.result.data.len()) {
            best = Some(Picked { result, effort: at.effort, reached });
        }
        if reached {
            break;
        }
    }
    best.ok_or_else(|| Error::Internal("optimize_to_size: no effort was tried".into()))
}

/// The gpress archive: any file (a PDF gains the most), restored byte for byte by [`expand`]. Every
/// archive is expanded and compared before it is returned.
#[cfg(feature = "archive")]
pub fn compress(input: &[u8], opt: &Options) -> Result<Compressed, Error> {
    check(input, opt)?;
    guard("compress", || {
        let (data, st) = pack::pack_with(input, &opt.pack());
        if st.timed_out {
            return Err(Error::TimeBudget(opt.time_budget.unwrap_or_default()));
        }
        Ok(Compressed {
            data,
            stored: st.stored,
            streams: st.streams,
            streams_rebuilt: st.rebuilt,
            rebuilt_bytes: st.rebuilt_bytes,
            streams_lepton: st.leptons,
            type1_fonts: st.type1,
            fax_images: st.fax,
            streams_decrypted: st.decrypted,
            decoded_bytes: st.decoded_bytes,
            declared_decoded: st.claimed,
        })
    })
}

/// The original bytes from an archive. A damaged archive is an error, never a wrong file: the length and
/// a 128-bit hash of the restored bytes are checked against the header. An archive declaring more than
/// `Options::max_len` is refused before anything is decoded.
#[cfg(feature = "archive")]
pub fn expand(archive: &[u8], opt: &Options) -> Result<Vec<u8>, Error> {
    check(archive, opt)?;
    guard("expand", || {
        pack::unpack_with(archive, &opt.unpack()).map_err(|e| match e {
            pack::UnpackError::UnknownVersion(v) => Error::UnsupportedVersion(v),
            pack::UnpackError::TooLong { declared, limit } => Error::DeclaredTooLarge { declared, limit },
            pack::UnpackError::TooMuchDecoded { declared, limit } => Error::DecodedTooLarge { declared, limit },
            pack::UnpackError::NeedsExisting => Error::NeedsExistingLibrary,
            pack::UnpackError::NeedsFeature(f) => Error::NeedsFeature(f),
            pack::UnpackError::TimedOut => Error::TimeBudget(opt.time_budget.unwrap_or_default()),
            other => Error::Damaged(other.to_string()),
        })
    })
}

/// Without the `archive` feature: the options and the input are checked as always, then
/// [`Error::NeedsFeature`].
#[cfg(not(feature = "archive"))]
pub fn compress(input: &[u8], opt: &Options) -> Result<Compressed, Error> {
    check(input, opt)?;
    Err(Error::NeedsFeature(Feature::Archive))
}

/// Without the `archive` feature: the options and the input are checked as always, then
/// [`Error::NeedsFeature`].
#[cfg(not(feature = "archive"))]
pub fn expand(archive: &[u8], opt: &Options) -> Result<Vec<u8>, Error> {
    check(archive, opt)?;
    Err(Error::NeedsFeature(Feature::Archive))
}

/// True when the bytes start like a gpress archive (cheap; no decoding).
pub fn is_archive(data: &[u8]) -> bool {
    data.len() >= 4 && &data[..4] == pack::MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_checked_first() {
        let opt = Options::new().max_input(10);
        let big = vec![0u8; 11];
        assert_eq!(optimize(&big, &opt).unwrap_err(), Error::TooLarge { size: 11, limit: 10 });
        assert_eq!(compress(&big, &opt).unwrap_err(), Error::TooLarge { size: 11, limit: 10 });
        assert_eq!(expand(&big, &opt).unwrap_err(), Error::TooLarge { size: 11, limit: 10 });
    }

    #[test]
    fn options_outside_the_engine_are_refused_with_a_code() {
        let bad = [
            Options::new().threads(MAX_THREADS + 1),
            Options::new().store_budget(MAX_STORE_BUDGET + 1),
            Options::new().time_budget(Some(Duration::ZERO)),
            Options::new().max_input(0),
            Options::new().objects_per_stream(0),
        ];
        for opt in &bad {
            let e = compress(b"x", opt).unwrap_err();
            assert!(matches!(e, Error::InvalidOptions(_)), "{opt:?}: {e}");
            assert_eq!(e.code(), 5);
            assert_eq!(opt.check(), Err(e));
        }
        assert_eq!(Options::new().threads(MAX_THREADS).check(), Ok(()));
        assert_eq!(Options::new().effort(255).effort, Effort::MAX);
    }

    #[test]
    #[cfg(feature = "archive")]
    fn anything_round_trips_and_garbage_is_refused() {
        let opt = Options::default();
        for input in [b"".to_vec(), b"%PDF-1.7\n%%EOF".to_vec(), (0..3000u32).map(|i| (i * 7919 % 251) as u8).collect()]
        {
            let c = compress(&input, &opt).unwrap();
            assert!(is_archive(&c.data));
            assert_eq!(expand(&c.data, &opt).unwrap(), input);
        }
        assert!(matches!(expand(b"not an archive", &opt), Err(Error::Damaged(_))));
        let mut other = compress(b"x", &opt).unwrap().data;
        other[4] = 200;
        assert_eq!(expand(&other, &opt), Err(Error::UnsupportedVersion(200)));
        other[4] = 7;
        assert_eq!(expand(&other, &opt), Err(Error::UnsupportedVersion(7)));
    }

    #[test]
    fn every_error_has_its_own_code_and_a_message_without_paths() {
        let errors = [
            Error::TooLarge { size: 2, limit: 1 },
            Error::Damaged("damaged gpress archive (x)".into()),
            Error::UnsupportedVersion(9),
            Error::Internal("boom".into()),
            Error::InvalidOptions("threads".into()),
            Error::TimeBudget(Duration::from_millis(5)),
            Error::DeclaredTooLarge { declared: 2, limit: 1 },
            Error::NeedsExistingLibrary,
            Error::DecodedTooLarge { declared: 2, limit: 1 },
            Error::NeedsFeature(Feature::Archive),
        ];
        let codes: Vec<i32> = errors.iter().map(Error::code).collect();
        assert_eq!(codes, (1..=10).collect::<Vec<i32>>());
        for e in &errors {
            let m = e.to_string();
            assert!(!m.is_empty() && !m.contains("src/") && !m.contains(".rs:"), "{m}");
        }
        assert_eq!(without_paths("boom\nat src/api.rs:1:2"), "boom");
    }

    #[test]
    fn effort_levels_move_every_knob_monotonically() {
        // The engine's levels. Below 20 the fast encoder (its level for streams other than images; images get
        // theirs in the plan).
        let at = |e: u8| Options::new().engine_effort(Some(e)).flate();
        for e in 0..crate::pdf::rewrite::FAST_BELOW {
            assert_eq!(at(e).fast, crate::flate::fast::LEVELS, "effort {e}");
        }
        // From 20 the strong encoder, every knob moving one way.
        let mut last = at(20);
        assert_eq!(last.fast, 0);
        for e in 21..=100u8 {
            let f = at(e);
            assert!(
                f.fast == 0
                    && f.iterations >= last.iterations
                    && f.exact_rounds >= last.exact_rounds
                    && f.max_chain >= last.max_chain,
                "effort {e}"
            );
            last = f;
        }
        assert_eq!(at(100).max_chain, crate::flate::Options::max().max_chain);
    }

    #[test]
    fn an_effort_means_its_step_and_the_engine_level_is_a_knob_of_its_own() {
        assert_eq!((Effort::default(), Options::default().effort), (Effort::new(40), Effort::new(40)));
        assert_eq!((Options::default().engine_effort, Options::default().lossy), (None, None));
        for e in 0..=100u8 {
            let step = e / 20 * 20;
            assert_eq!(Effort::new(e).step(), step);
            let (engine, lossy) = match step {
                0 => (0, 0),
                20 => (20, 0),
                40 => (100, 0),
                60 => (100, 1),
                80 => (100, 2),
                _ => (100, 3),
            };
            let opt = Options::new().effort(e);
            let r = opt.rewrite();
            assert_eq!((r.effort, r.lossy), (engine, Lossy::level(lossy)), "effort {e}");
            assert_eq!(r.flate.max_chain, crate::flate::Options::effort(engine).max_chain, "effort {e}");
            // The archive takes the lossless level and nothing else: it has no lossy setting to take.
            #[cfg(feature = "archive")]
            assert_eq!(opt.pack().effort, engine, "effort {e}");
            // The engine's own level: the effort then decides nothing, and nothing is lossy.
            let own = opt.clone().engine_effort(Some(e));
            assert_eq!((own.rewrite().effort, own.rewrite().lossy), (e, Lossy::OFF), "engine level {e}");
            #[cfg(feature = "archive")]
            assert_eq!(own.pack().effort, e, "engine level {e}");
            // `lossy` set wins both ways, over the effort and over the engine's level.
            for l in [Lossy::OFF, Lossy::level(2)] {
                assert_eq!(opt.clone().lossy(Some(l)).rewrite().lossy, l, "effort {e}");
                assert_eq!(own.clone().lossy(Some(l)).rewrite().lossy, l, "engine level {e}");
            }
            assert_eq!(opt.clone().lossy(Some(Lossy::level(2))).rewrite().effort, engine, "effort {e}");
        }
        assert_eq!(Options::new().engine_effort(Some(255)).rewrite().effort, 100);
        assert!(Lossy::level(0).is_off() && !Lossy::level(1).is_off());
    }

    #[test]
    fn a_lossy_override_replaces_one_setting_of_what_the_options_mean() {
        // All 0: nothing changes, and `lossy` stays unset.
        assert_eq!(Options::new().effort(80).lossy_overrides(0, 0, 0), Options::new().effort(80));
        // One value: that setting, the rest as the effort has them (off below 60, and with the engine's level).
        let at80 = Options::new().effort(80).lossy_overrides(200, 0, 0);
        assert_eq!(at80.lossy, Some(Lossy { photo_ppi: 200, ..Lossy::level(2) }));
        let at40 = Options::new().lossy_overrides(0, 400, 60);
        assert_eq!(at40.lossy, Some(Lossy { art_ppi: 400, jpeg_quality: 60, ..Lossy::OFF }));
        let own = Options::new().effort(100).engine_effort(Some(50)).lossy_overrides(0, 0, 90);
        assert_eq!(own.rewrite().lossy, Lossy { jpeg_quality: 90, ..Lossy::OFF });
        // On top of `lossy` when that is set.
        let set = Options::new().lossy(Some(Lossy::level(3))).lossy_overrides(0, 600, 0);
        assert_eq!(set.rewrite().lossy, Lossy { art_ppi: 600, ..Lossy::level(3) });
    }

    #[test]
    fn the_options_reach_the_engine() {
        let opt = Options::new()
            .engine_effort(Some(40))
            .threads(3)
            .store_budget(4096)
            .max_stream(1000)
            .max_stream_archive(2000)
            .max_decoded(2500)
            .max_len(3000)
            .objects_per_stream(7)
            .respect_conformance(false)
            .flate_over_jpeg(false)
            .jpeg_tables(false)
            .jbig2(false)
            .rewrite_encrypted(false)
            .decrypt(false)
            .use_existing_libraries(false)
            .time_budget(Some(Duration::from_secs(9)));
        let r = opt.rewrite();
        assert_eq!((r.effort, r.threads, r.store_budget, r.max_stream, r.objstm_size), (40, 3, 4096, 1000, 7));
        assert!(!r.respect_conformance && !r.flate_over_jpeg && !r.jpeg_tables && !r.jbig2 && !r.rewrite_encrypted);
        assert!(r.deadline.is_set() && r.flate.deadline == Deadline::NONE, "the pool sets the Flate deadline");
        assert!(!Options::default().rewrite().deadline.is_set());
        #[cfg(feature = "archive")]
        {
            let p = opt.pack();
            assert_eq!((p.effort, p.threads, p.max_stream, p.max_decoded), (40, 3, 2000, 2500));
            assert!(!p.decrypt && !p.existing && p.deadline.is_set());
            let u = opt.unpack();
            assert_eq!((u.max_len, u.max_decoded), (3000, 2500));
            assert!(u.deadline.is_set() && !Options::default().unpack().deadline.is_set());
            assert_eq!(Options::default().pack().existing, Options::has_existing_libraries());
        }
    }
}
