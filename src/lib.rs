//! Our PDF compression engine. No third-party compression code: everything that encodes or decodes
//! data is written here (encrypted PDFs use RustCrypto's AES, MD5 and SHA-2).

pub mod par;
pub mod fax;
pub mod flate;
pub mod jpeg;
pub mod pdf;
pub mod cm;
pub mod pack;
