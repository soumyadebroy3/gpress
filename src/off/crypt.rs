//! Without the `encryption` feature: the names the PDF reader and writer use for an encrypted file's security
//! handler. None can be made: the reader reports every encrypted file as one this build does not open
//! (`crate::pdf::xref`), so `optimize` leaves it untouched and says why, and the archive stores its streams
//! as they are.

use crate::pdf::object::{Dict, Obj};
use std::convert::Infallible;

/// How strings or streams are encrypted: only the name the writer compares with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Identity,
}

/// A security handler, which this build never has (its last field has no value).
#[derive(Clone, Debug, PartialEq)]
pub struct Crypt {
    pub dict: Dict,
    pub obj: Option<(u32, u16)>,
    pub parts: Vec<(u32, u16, Obj)>,
    never: Infallible,
}

impl Crypt {
    pub fn is_own(&self, _n: u32) -> bool {
        match self.never {}
    }

    pub fn describe(&self) -> String {
        match self.never {}
    }

    pub fn stream_method(&self, _num: u32, _dict: &Dict) -> Result<Method, &'static str> {
        match self.never {}
    }

    pub fn decrypt_object(&self, _num: u32, _gen: u16, _o: Obj) -> Result<Obj, &'static str> {
        match self.never {}
    }

    pub fn seal_object(&self, _num: u32, _gen: u16, _o: &Obj) -> Result<Obj, &'static str> {
        match self.never {}
    }

    pub fn seal_stream(&self, _num: u32, _gen: u16, _d: &Dict, _data: &[u8]) -> Result<(Dict, Vec<u8>), &'static str> {
        match self.never {}
    }
}
