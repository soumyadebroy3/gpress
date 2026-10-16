//! Without the `fax` feature: no CCITT fax image is written as JBIG2 (as with `Options::jbig2` off), and the
//! archive stores fax streams as they are.

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
}
