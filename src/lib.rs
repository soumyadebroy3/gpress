//! Our PDF compression engine. No third-party compression code: everything that encodes or decodes
//! data is written here (encrypted PDFs use RustCrypto's AES, MD5 and SHA-2).

// Index loops read more clearly than iterator chains in the coding and model loops.
#![allow(clippy::needless_range_loop)]

pub mod par;
pub mod fax;
pub mod flate;
pub mod jpeg;
pub mod pdf;
pub mod cm;
pub mod pack;
