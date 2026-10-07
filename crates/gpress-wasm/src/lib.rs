//! gpress for WebAssembly. A plain module with a handful of exports and no bindgen layer, so it builds
//! with `cargo build --target wasm32-unknown-unknown` alone and loads with the browser's own
//! `WebAssembly.instantiate`. The JavaScript side (`js/gpress.ts`) does the typed wrapping.
//!
//! Memory protocol: the host asks for a buffer with `gpress_alloc(len)`, writes the input into it, calls
//! an operation, reads the output through `gpress_out_ptr()` / `gpress_out_len()` (the module keeps one
//! result at a time), then frees both with `gpress_free_input` / `gpress_clear_out`. Operations return
//! 0 on success or a gpress error code; `gpress_message_*` give the text (also the "untouched because"
//! reason). The module never traps: every entry point runs inside a panic guard.
//!
//! The module is single-threaded (threads are an option on native builds only) and deterministic: the
//! same input and options give the same bytes as the native library.

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};

thread_local! {
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static MSG: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static OUTCOME: RefCell<i32> = const { RefCell::new(0) };
}

fn set_out(v: Vec<u8>) {
    OUT.with(|o| *o.borrow_mut() = v);
}
fn set_msg(s: &str) {
    MSG.with(|m| *m.borrow_mut() = s.as_bytes().to_vec());
}
fn set_outcome(v: i32) {
    OUTCOME.with(|o| *o.borrow_mut() = v);
}

fn options(effort: u32, max_input_mb: u32, max_stream_mb: u32, flags: u32) -> gpress::Options {
    let mut o = gpress::Options::default().effort(effort.min(100) as u8).threads(1);
    if max_input_mb > 0 {
        o.max_input = (max_input_mb as usize) << 20;
    }
    if max_stream_mb > 0 {
        o.max_stream = (max_stream_mb as usize) << 20;
    }
    o.respect_conformance = flags & 1 == 0;
    o.flate_over_jpeg = flags & 2 == 0;
    o.use_existing_libraries = flags & 4 == 0;
    o
}

/// # Safety
/// `ptr`/`len` must describe a buffer from `gpress_alloc` that the host filled.
unsafe fn input<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len)
    }
}

fn run(f: impl FnOnce() -> Result<(Vec<u8>, i32, String), gpress::Error>) -> i32 {
    set_out(Vec::new());
    set_msg("");
    set_outcome(0);
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok((data, outcome, msg))) => {
            set_out(data);
            set_outcome(outcome);
            set_msg(&msg);
            0
        }
        Ok(Err(e)) => {
            set_msg(&e.to_string());
            e.code()
        }
        Err(_) => {
            set_msg("internal error (panic)");
            4
        }
    }
}

/// Allocates `len` bytes for the host to write the input into. Returns null when the allocation fails.
#[no_mangle]
pub extern "C" fn gpress_alloc(len: usize) -> *mut u8 {
    let mut v: Vec<u8> = Vec::new();
    if v.try_reserve_exact(len.max(1)).is_err() {
        return std::ptr::null_mut();
    }
    v.resize(len.max(1), 0);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Frees a buffer from `gpress_alloc` (same `len`).
///
/// # Safety
/// `ptr` must come from `gpress_alloc(len)` and not be freed twice.
#[no_mangle]
pub unsafe extern "C" fn gpress_free_input(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(Vec::from_raw_parts(ptr, len.max(1), len.max(1)));
    }
}

/// A smaller PDF (outcome 1) or the input untouched (outcome 2, message says why).
///
/// # Safety
/// See `input`.
#[no_mangle]
pub unsafe extern "C" fn gpress_optimize(
    ptr: *const u8,
    len: usize,
    effort: u32,
    max_input_mb: u32,
    max_stream_mb: u32,
    flags: u32,
) -> i32 {
    let data = input(ptr, len);
    let opt = options(effort, max_input_mb, max_stream_mb, flags);
    run(|| {
        let r = gpress::optimize(data, &opt)?;
        Ok(match r.outcome {
            gpress::Outcome::Smaller(_) => (r.data, 1, String::new()),
            gpress::Outcome::Untouched(why) => (r.data, 2, why),
        })
    })
}

/// The gpress archive (outcome 1, or 2 when it had to be stored as it was).
///
/// # Safety
/// See `input`.
#[no_mangle]
pub unsafe extern "C" fn gpress_compress(
    ptr: *const u8,
    len: usize,
    effort: u32,
    max_input_mb: u32,
    max_stream_mb: u32,
    flags: u32,
) -> i32 {
    let data = input(ptr, len);
    let opt = options(effort, max_input_mb, max_stream_mb, flags);
    run(|| {
        let c = gpress::compress(data, &opt)?;
        Ok((c.data, if c.stored { 2 } else { 1 }, String::new()))
    })
}

/// The original bytes from an archive, or an error code (never a wrong file).
///
/// # Safety
/// See `input`.
#[no_mangle]
pub unsafe extern "C" fn gpress_expand(ptr: *const u8, len: usize, max_input_mb: u32) -> i32 {
    let data = input(ptr, len);
    let opt = options(100, max_input_mb, 0, 0);
    run(|| Ok((gpress::expand(data, &opt)?, 0, String::new())))
}

#[no_mangle]
pub extern "C" fn gpress_out_ptr() -> *const u8 {
    OUT.with(|o| o.borrow().as_ptr())
}
#[no_mangle]
pub extern "C" fn gpress_out_len() -> usize {
    OUT.with(|o| o.borrow().len())
}
#[no_mangle]
pub extern "C" fn gpress_outcome() -> i32 {
    OUTCOME.with(|o| *o.borrow())
}
#[no_mangle]
pub extern "C" fn gpress_message_ptr() -> *const u8 {
    MSG.with(|m| m.borrow().as_ptr())
}
#[no_mangle]
pub extern "C" fn gpress_message_len() -> usize {
    MSG.with(|m| m.borrow().len())
}
/// Drops the current result so its memory can be reused.
#[no_mangle]
pub extern "C" fn gpress_clear_out() {
    set_out(Vec::new());
    set_msg("");
}
/// Library version as "major.minor.patch" packed into a number: major * 1_000_000 + minor * 1000 + patch.
#[no_mangle]
pub extern "C" fn gpress_version_number() -> u32 {
    let mut parts = env!("CARGO_PKG_VERSION").split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (a, b, c) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    a * 1_000_000 + b * 1000 + c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_round_trip_on_native() {
        let input = b"%PDF-1.4\n%%EOF\n";
        unsafe {
            let p = gpress_alloc(input.len());
            std::ptr::copy_nonoverlapping(input.as_ptr(), p, input.len());
            assert_eq!(gpress_compress(p, input.len(), 100, 0, 0, 0), 0);
            let archive = std::slice::from_raw_parts(gpress_out_ptr(), gpress_out_len()).to_vec();
            gpress_free_input(p, input.len());
            let q = gpress_alloc(archive.len());
            std::ptr::copy_nonoverlapping(archive.as_ptr(), q, archive.len());
            assert_eq!(gpress_expand(q, archive.len(), 0), 0);
            assert_eq!(std::slice::from_raw_parts(gpress_out_ptr(), gpress_out_len()), &input[..]);
            gpress_free_input(q, archive.len());
            // Damaged: code 2 and a message.
            let bad = gpress_alloc(3);
            assert_eq!(gpress_expand(bad, 3, 0), 2);
            assert!(gpress_message_len() > 0);
            gpress_free_input(bad, 3);
            gpress_clear_out();
            assert_eq!(gpress_out_len(), 0);
        }
    }
}
