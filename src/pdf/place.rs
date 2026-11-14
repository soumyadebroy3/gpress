// Stand-in until the module lands on this branch: the lossy step finds nothing to do with it.
use super::object::{Dict, Obj};
use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    pub width: f64,
    pub height: f64,
    pub scan: bool,
}
pub fn shown(
    _objs: &BTreeMap<u32, (u16, Obj)>,
    _trailer: &Dict,
    _max_stream: usize,
    _deadline: crate::clock::Deadline,
) -> BTreeMap<u32, Shown> {
    BTreeMap::new()
}
