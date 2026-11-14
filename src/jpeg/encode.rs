// Stand-in until the module lands on this branch: the lossy step finds nothing to do with it.
pub fn encode(_planes: &[super::Plane], _quality: u8, _deadline: crate::clock::Deadline) -> Option<Vec<u8>> {
    None
}
pub fn ycc_planes(_rgb: &[u8], _w: usize, _h: usize) -> Option<Vec<super::Plane>> {
    None
}
