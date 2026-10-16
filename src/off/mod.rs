//! What a build without a feature has in place of it: the few names the rest of the engine calls, doing
//! nothing, so the code that calls them needs no change. Nothing here can make a smaller file or a wrong one:
//! a step whose feature is left out is skipped (as when its option is off), and a file that needs it is left
//! as it is. `Cargo.toml` lists the features; `crate::Feature` names them.

#[cfg(not(feature = "encryption"))]
pub mod crypt;
#[cfg(not(feature = "fax"))]
pub mod fax;
#[cfg(not(feature = "jpeg"))]
pub mod jpeg;
#[cfg(not(feature = "archive"))]
pub mod pack;
