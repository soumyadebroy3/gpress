//! gpress: lossless PDF compression as a library.
//!
//! Two outputs, both verified before they are returned:
//! - [`optimize`]: a smaller PDF that any reader opens, every object's value preserved.
//! - [`compress`] / [`expand`]: the gpress archive, restored byte for byte.
//!
//! The engine is our own: Flate encoder and decoder, zlib re-creation, PDF reader and writer, and the
//! context-mixing model. Optional existing libraries (feature `existing`) add candidates the engine keeps
//! only when they are smaller and verified. See `README.md`.

// Index loops in the codecs are written as loops on purpose (they mirror the specifications and the hot
// paths were tuned that way); this lint would only reshape them.
#![allow(clippy::needless_range_loop)]

pub mod api;
pub mod cm;
pub mod flate;
pub mod pack;
pub mod pdf;

pub use api::{
    compress, expand, is_archive, optimize, Compressed, Effort, Error, Optimized, Options, Outcome, Report, VERSION,
};
