//! The public face of gpress: one options type, three operations, typed results, and no panics.
//!
//! Everything a caller can tune lives in [`Options`]. The only constants that are not options are the
//! ones that define the file formats (a decoder must agree with its encoder on those).
//!
//! The three operations:
//! - [`optimize`]: a smaller PDF that any PDF reader opens, with every object's value preserved.
//! - [`compress`]: the gpress archive (`.gpz`), restored byte for byte by [`expand`].
//! - [`expand`]: the original bytes back from an archive, or an error; never a wrong file.
//!
//! Every call checks its input against the limits in `Options` first, runs the engine inside a panic
//! guard (a bug can never take the host process down, which matters for the C and WebAssembly bindings),
//! and returns a plain [`Error`] instead.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use crate::pack;
use crate::pdf::rewrite;

/// Library version, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How hard to try, 0 to 100. 0 is the quick setting (still optimal-parsed Flate), 100 is everything the
/// engine knows how to do. The peyaji.com percentage control maps straight onto this.
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
}

impl Default for Effort {
    fn default() -> Self {
        Effort::MAX
    }
}

/// Everything a caller can tune. `Options::default()` is maximum effort with sensible limits; the
/// builder methods adjust one thing each.
#[derive(Clone, Debug)]
pub struct Options {
    /// Effort for both operations.
    pub effort: Effort,
    /// Largest input accepted, in bytes. Inputs over this return [`Error::TooLarge`] at once.
    pub max_input: usize,
    /// Largest single decoded stream the engine will hold in memory. Streams that decode to more than
    /// this are kept as they are (optimize) or stored (compress).
    pub max_stream: usize,
    /// Worker threads for independent work (0 = one per available CPU). Output is identical whatever the
    /// count; this only changes speed.
    pub threads: usize,
    /// Most objects per compressed object stream in a normal PDF.
    pub objects_per_stream: usize,
    /// Try the optional existing-library candidate (Lepton, for JPEG streams) when the build includes it.
    pub use_existing_libraries: bool,
    /// Normal PDF: leave files that are PDF/A, PDF/X, signed, certified or have usage rights untouched.
    /// `true` is the safe default; `false` still keeps every object's value but drops those guarantees.
    pub respect_conformance: bool,
    /// Normal PDF: also try putting Flate in front of JPEG images (never JPEG 2000 or JBIG2).
    pub flate_over_jpeg: bool,
    /// Stop early and return what the engine has when this much wall time has passed. `None` = no limit.
    /// The result is still fully verified; it is simply the result of less effort.
    pub time_budget: Option<Duration>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            effort: Effort::MAX,
            max_input: 1 << 30,
            max_stream: 64 << 20,
            threads: 0,
            objects_per_stream: 200,
            use_existing_libraries: true,
            respect_conformance: true,
            flate_over_jpeg: true,
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
    pub fn max_input(mut self, bytes: usize) -> Options {
        self.max_input = bytes;
        self
    }
    pub fn max_stream(mut self, bytes: usize) -> Options {
        self.max_stream = bytes;
        self
    }
    pub fn threads(mut self, n: usize) -> Options {
        self.threads = n;
        self
    }
    pub fn time_budget(mut self, budget: Option<Duration>) -> Options {
        self.time_budget = budget;
        self
    }
    pub fn respect_conformance(mut self, yes: bool) -> Options {
        self.respect_conformance = yes;
        self
    }

    /// The Flate encoder settings this effort level means (the engine's own steps of 20).
    pub(crate) fn flate(&self) -> crate::flate::Options {
        crate::flate::Options::effort(self.effort.level())
    }

    /// The Flate settings for this effort, for the command-line tool's fine-tuning flags.
    #[doc(hidden)]
    pub fn flate_for_cli(&self) -> crate::flate::Options {
        self.flate()
    }

    pub(crate) fn rewrite(&self) -> rewrite::Options {
        rewrite::Options {
            flate: self.flate(),
            objstm_size: self.objects_per_stream.max(1),
            max_stream: self.max_stream,
            effort: self.effort.level(),
            threads: self.threads,
            respect_conformance: self.respect_conformance,
            flate_over_jpeg: self.flate_over_jpeg,
            ..rewrite::Options::default()
        }
    }

    pub(crate) fn pack(&self) -> pack::PackOptions {
        pack::PackOptions {
            effort: self.effort.level(),
            threads: self.threads,
            max_stream: self.max_stream,
            existing: self.use_existing_libraries && pack::PackOptions::default().existing,
            ..pack::PackOptions::default()
        }
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
    /// The archive was made by a newer gpress than this one.
    UnsupportedVersion(u8),
    /// The engine hit a bug. The input is unchanged; please report it with the file.
    Internal(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooLarge { size, limit } => write!(f, "input is {size} bytes, over the limit of {limit}"),
            Error::Damaged(why) => write!(f, "damaged gpress archive: {why}"),
            Error::UnsupportedVersion(v) => write!(f, "gpress archive version {v} needs a newer gpress"),
            Error::Internal(why) => write!(f, "internal error: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// Numeric codes for the C and WebAssembly layers (stable across versions).
impl Error {
    pub fn code(&self) -> i32 {
        match self {
            Error::TooLarge { .. } => 1,
            Error::Damaged(_) => 2,
            Error::UnsupportedVersion(_) => 3,
            Error::Internal(_) => 4,
        }
    }
}

/// What `optimize` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A smaller PDF with every object's value preserved. The statistics say what changed.
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
}

/// A normal-PDF result.
#[derive(Clone, Debug)]
pub struct Optimized {
    pub data: Vec<u8>,
    pub outcome: Outcome,
}

/// A gpress archive and what went into it.
#[derive(Clone, Debug)]
pub struct Compressed {
    pub data: Vec<u8>,
    /// Bytes of streams rebuilt exactly from their decoded form (the gain comes from these).
    pub rebuilt_bytes: usize,
    /// Streams the model saw decoded, with an existing library's help, or stored as they were.
    pub streams: usize,
    pub streams_rebuilt: usize,
    pub streams_lepton: usize,
    /// The archive is the whole input stored as it was: nothing could be made smaller.
    pub stored: bool,
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
            Err(Error::Internal(format!("{what}: {msg}")))
        }
    }
}

fn check_size(input: &[u8], opt: &Options) -> Result<(), Error> {
    if input.len() > opt.max_input {
        return Err(Error::TooLarge { size: input.len(), limit: opt.max_input });
    }
    Ok(())
}

/// A smaller PDF that any reader opens. Every object's value is preserved and the output is read back
/// and compared before it is returned; when in doubt the input comes back untouched.
pub fn optimize(input: &[u8], opt: &Options) -> Result<Optimized, Error> {
    check_size(input, opt)?;
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
            }),
            rewrite::Outcome::Untouched(why) => Outcome::Untouched(why),
        };
        Ok(Optimized { data: r.data, outcome })
    })
}

/// The gpress archive: any file (a PDF gains the most), restored byte for byte by [`expand`]. Every
/// archive is expanded and compared before it is returned.
pub fn compress(input: &[u8], opt: &Options) -> Result<Compressed, Error> {
    check_size(input, opt)?;
    guard("compress", || {
        let (data, st) = pack::pack_with(input, &opt.pack());
        Ok(Compressed {
            data,
            rebuilt_bytes: st.rebuilt_bytes,
            streams: st.streams,
            streams_rebuilt: st.rebuilt,
            streams_lepton: st.leptons,
            stored: st.stored,
        })
    })
}

/// The original bytes from an archive. A damaged archive is an error, never a wrong file: the length and
/// a 128-bit hash of the restored bytes are checked against the header.
pub fn expand(archive: &[u8], opt: &Options) -> Result<Vec<u8>, Error> {
    check_size(archive, opt)?;
    if archive.len() >= 5
        && &archive[..4] == pack::MAGIC
        && archive[4] > pack::VERSION
        && archive[4] != 0
        && archive[4] != 2
    {
        return Err(Error::UnsupportedVersion(archive[4]));
    }
    guard("expand", || pack::unpack(archive).map_err(Error::Damaged))
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
    fn anything_round_trips_and_garbage_is_refused() {
        let opt = Options::default();
        for input in [b"".to_vec(), b"%PDF-1.7\n%%EOF".to_vec(), (0..3000u32).map(|i| (i * 7919 % 251) as u8).collect()]
        {
            let c = compress(&input, &opt).unwrap();
            assert_eq!(expand(&c.data, &opt).unwrap(), input);
        }
        assert!(matches!(expand(b"not an archive", &opt), Err(Error::Damaged(_))));
        let mut future = compress(b"x", &opt).unwrap().data;
        future[4] = 200;
        assert_eq!(expand(&future, &opt), Err(Error::UnsupportedVersion(200)));
    }

    #[test]
    fn effort_levels_move_every_knob_monotonically() {
        let mut last = Options::new().effort(0).flate();
        for e in 1..=100u8 {
            let f = Options::new().effort(e).flate();
            assert!(
                f.iterations >= last.iterations && f.exact_rounds >= last.exact_rounds && f.max_chain >= last.max_chain,
                "effort {e}"
            );
            last = f;
        }
        assert_eq!(Options::new().effort(100).flate().max_chain, crate::flate::Options::max().max_chain);
        assert_eq!(Options::new().effort(0).flate().max_chain, crate::flate::Options::quick().max_chain);
    }
}
