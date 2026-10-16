//! Without the `archive` feature: the archive format's constants, which the options (`max_decoded`'s
//! default) and `is_archive` use in every build. The values are those of `crate::pack`; `tests/features.rs`
//! checks the same values in builds with and without the archive.

/// The first bytes of every gpress archive.
pub const MAGIC: &[u8; 4] = b"\x89GPZ";
/// The archive format version a build with the archive writes and reads.
pub const VERSION: u8 = 8;
/// The default budget of decoded bytes for the parts of one archive.
pub const DEFAULT_MAX_DECODED: u64 = 256 << 20;
