//! Without the `jpeg` feature: no JPEG gets new Huffman tables (as with `Options::jpeg_tables` off).

use crate::Feature;

/// Never a new JPEG.
pub fn optimize(_jpeg: &[u8]) -> Result<Vec<u8>, Feature> {
    Err(Feature::Jpeg)
}

/// For the self-check: no JPEG is ever written with other tables, so none is taken for the same as another.
pub fn equivalent(_a: &[u8], _b: &[u8]) -> bool {
    false
}
