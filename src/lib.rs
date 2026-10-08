//! gpress: lossless PDF compression as a library.
//!
//! Two outputs, both verified before they are returned:
//! - [`optimize`]: a smaller PDF that any reader opens, every object's value preserved.
//! - [`compress`] / [`expand`]: the gpress archive, restored byte for byte.
//!
//! The engine is our own: Flate encoder and decoder, zlib re-creation, PDF reader and writer, JPEG Huffman
//! tables, CCITT and JBIG2 fax coding, Type 1 font opening, and the context-mixing model. Encrypted PDFs use
//! RustCrypto's AES, MD5 and SHA-2. The optional existing library (feature `existing`, Lepton for JPEG
//! streams) adds a candidate the engine keeps only when it is smaller and verified. See `README.md`.

// Index loops read more clearly than iterator chains in the coding and model loops.
#![allow(clippy::needless_range_loop)]

pub mod api;
pub mod cm;
pub mod fax;
pub mod flate;
pub mod jpeg;
pub mod pack;
pub mod par;
pub mod pdf;

pub use api::{
    compress, expand, is_archive, optimize, Compressed, Effort, Error, Optimized, Options, Outcome, Report, VERSION,
};
