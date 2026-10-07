//! Scanned black-and-white pages: CCITT fax images (T.4, T.6) decoded and re-encoded bit for bit, a JBIG2
//! generic-region encoder and decoder (T.88) for normal PDFs, and a bilevel context model for the peyaji
//! format.

pub mod ccitt;
pub mod tables;
