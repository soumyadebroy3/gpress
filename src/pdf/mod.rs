//! Our own PDF reader and writer.

#[cfg(feature = "encryption")]
pub mod crypt;
// Without the `encryption` feature: a security handler that is never made (see `crate::off`).
#[cfg(not(feature = "encryption"))]
pub(crate) use crate::off::crypt;
pub mod filters;
pub mod images;
pub mod object;
pub mod parse;
pub mod rewrite;
pub mod xref;
