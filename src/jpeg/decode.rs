// Stand-in until the module lands on this branch: the lossy step finds nothing to do with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub planes: Vec<Plane>,
}
pub fn decode(_d: &[u8], _max_samples: usize, _deadline: crate::clock::Deadline) -> Result<Picture, super::Refused> {
    Err(super::Refused("stub"))
}
