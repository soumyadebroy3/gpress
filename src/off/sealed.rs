//! `crate::pack::sealed` without the `encryption` feature: no stream is stored decrypted (as with
//! `PackOptions::decrypt` off), and an archive with decrypted streams restores only in a build with the
//! feature (`left_out` stops at its first such part, so nothing here is reached on restore).

use super::*;

/// An encrypted stream's settings, which this build never has.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Sealed {}

/// The plans unchanged: encrypted streams are stored as they are.
pub(super) fn crypt_plans(_d: &[u8], plans: Vec<Plan>, _opt: &PackOptions) -> Vec<Plan> {
    plans
}

pub(super) fn seal_payload(s: &Sealed, _key: &[u8], _plain: &[u8]) -> Option<Vec<u8>> {
    match *s {}
}

pub(super) fn code_key<C: Coder>(_io: &mut Io<C>, _key_in: Option<&[u8]>) -> Option<Vec<u8>> {
    None
}

pub(super) fn code_settings<C: Coder>(
    _io: &mut Io<C>,
    _s_in: Option<&Sealed>,
    _out: &[u8],
    _key: Option<&[u8]>,
) -> Option<Sealed> {
    None
}
