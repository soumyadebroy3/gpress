//! Without the `fax` feature: no CCITT fax image and no other 1-bit image is written as JBIG2 (as with
//! `Options::jbig2` off), and the archive stores fax streams as they are.

pub mod pdf {
    use crate::pdf::object::{Dict, Obj, Stream};

    /// Follows a reference with the document's objects.
    pub type Resolve<'a> = &'a dyn Fn(&Obj) -> Option<Obj>;

    /// Never a JBIG2 stream.
    pub fn jbig2_candidate(_s: &Stream, _resolve: Resolve) -> Option<(Dict, Vec<u8>)> {
        None
    }

    /// For the self-check: no image is ever written as JBIG2, so none is taken for the same as another.
    pub fn same_image(_a: &Stream, _b: &Stream, _resolve: Resolve) -> bool {
        false
    }

    /// No image is one this build writes as JBIG2.
    pub fn one_bit(_d: &Dict) -> Result<(), &'static str> {
        Err("this build has no JBIG2")
    }

    /// Never a JBIG2 stream.
    pub fn bilevel_candidate(_s: &Stream, _bits: &[u8], _best: usize) -> Option<(Dict, Vec<u8>)> {
        None
    }

    /// For the self-check: no image is ever written as JBIG2.
    pub fn same_bits(_a: &Stream, _bits: &[u8], _b: &Stream) -> bool {
        false
    }
}
