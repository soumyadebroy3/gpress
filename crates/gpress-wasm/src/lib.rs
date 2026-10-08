//! gpress for WebAssembly. A plain module with a handful of exports and no bindgen layer, so it builds
//! with `cargo build --target wasm32-unknown-unknown` alone and loads with the browser's own
//! `WebAssembly.instantiate`. The JavaScript side (`js/gpress.ts`) does the typed wrapping.
//!
//! Memory protocol: the host asks for a buffer with `gpress_alloc(len)`, writes the input into it, calls
//! an operation, reads the output through `gpress_out_ptr()` / `gpress_out_len()` (the module keeps one
//! result at a time), then frees both with `gpress_free_input` / `gpress_clear_out`. Operations return
//! 0 on success or a gpress error code; `gpress_message_*` give the text (also the "untouched because"
//! reason). The engine reads no clock here (so there is no time budget: the host bounds the work by
//! running it in a worker).
//!
//! Panics: `wasm32-unknown-unknown` builds with `panic = "abort"`, so there is no unwinding and the panic
//! guards here and in the library catch nothing. A bug in the engine therefore traps the module
//! (`WebAssembly.RuntimeError` in the host, like a failed memory growth) instead of returning code 4; the
//! wrapper in `js/gpress.ts` turns the trap into a `GpressError` with that code and the instance must then
//! be loaded again. The guards stay for the native build of this crate (the tests) and for a host that
//! builds it with unwinding.
//!
//! Sizes are 32-bit here: the host refuses inputs of 2 GiB and more before they reach the module, and the
//! size options are given in MiB (0 = the library's default) and clamped to what the module can address.
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

/// Switches in the `flags` argument: a set bit turns the step off (the defaults are all on).
pub const FLAG_IGNORE_CONFORMANCE: u32 = 1;
pub const FLAG_NO_FLATE_OVER_JPEG: u32 = 2;
pub const FLAG_NO_EXISTING_LIBRARIES: u32 = 4;
pub const FLAG_NO_JPEG_TABLES: u32 = 8;
pub const FLAG_NO_JBIG2: u32 = 16;
pub const FLAG_KEEP_ENCRYPTED: u32 = 32;
pub const FLAG_NO_DECRYPT: u32 = 64;

/// A size given in MiB, or the default for 0, clamped to what this module can address.
fn mib(v: u32, default: usize) -> usize {
    if v == 0 {
        default
    } else {
        usize::try_from((v as u64) << 20).unwrap_or(usize::MAX)
    }
}

fn options(effort: u32, max_input_mb: u32, max_stream_mb: u32, objects_per_stream: u32, flags: u32) -> gpress::Options {
    let mut o = gpress::Options::default().effort(effort.min(100) as u8).threads(1);
    o.max_input = mib(max_input_mb, o.max_input);
    o.max_stream = mib(max_stream_mb, o.max_stream);
    o.max_stream_archive = mib(max_stream_mb, o.max_stream_archive);
    if objects_per_stream > 0 {
        o.objects_per_stream = objects_per_stream as usize;
    }
    o.respect_conformance = flags & FLAG_IGNORE_CONFORMANCE == 0;
    o.flate_over_jpeg = flags & FLAG_NO_FLATE_OVER_JPEG == 0;
    o.use_existing_libraries = flags & FLAG_NO_EXISTING_LIBRARIES == 0;
    o.jpeg_tables = flags & FLAG_NO_JPEG_TABLES == 0;
    o.jbig2 = flags & FLAG_NO_JBIG2 == 0;
    o.rewrite_encrypted = flags & FLAG_KEEP_ENCRYPTED == 0;
    o.decrypt = flags & FLAG_NO_DECRYPT == 0;
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
    objects_per_stream: u32,
    flags: u32,
) -> i32 {
    let data = input(ptr, len);
    let opt = options(effort, max_input_mb, max_stream_mb, objects_per_stream, flags);
    run(|| {
        let r = gpress::optimize(data, &opt)?;
        Ok(match r.outcome {
            gpress::Outcome::Smaller(_) => (r.data, 1, String::new()),
            gpress::Outcome::Untouched(why) => (r.data, 2, why),
        })
    })
}

/// A decoded-bytes budget given in MiB (0 = the library's default), a u64 on every target.
fn budget(v: u32, default: u64) -> u64 {
    if v == 0 {
        default
    } else {
        (v as u64) << 20
    }
}

/// The gpress archive (outcome 1, or 2 when it had to be stored as it was). `max_decoded_mb` is the
/// archive's decoded-bytes budget (0 = the library's default): streams past it are stored as they are.
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
    max_decoded_mb: u32,
    flags: u32,
) -> i32 {
    let data = input(ptr, len);
    let mut opt = options(effort, max_input_mb, max_stream_mb, 0, flags);
    opt.max_decoded = budget(max_decoded_mb, opt.max_decoded);
    run(|| {
        let c = gpress::compress(data, &opt)?;
        Ok((c.data, if c.stored { 2 } else { 1 }, String::new()))
    })
}

/// The original bytes from an archive, or an error code (never a wrong file). An archive declaring more
/// than `max_len_mb` MiB, or parts claiming more than `max_decoded_mb` MiB of decoded bytes in all (0 = the
/// library's default for either), is refused before anything is decoded.
///
/// # Safety
/// See `input`.
#[no_mangle]
pub unsafe extern "C" fn gpress_expand(
    ptr: *const u8,
    len: usize,
    max_input_mb: u32,
    max_len_mb: u32,
    max_decoded_mb: u32,
) -> i32 {
    let data = input(ptr, len);
    let mut opt = options(100, max_input_mb, 0, 0, 0);
    opt.max_len = budget(max_len_mb, opt.max_len);
    opt.max_decoded = budget(max_decoded_mb, opt.max_decoded);
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
/// The archive format version this module writes and reads.
#[no_mangle]
pub extern "C" fn gpress_archive_version() -> u32 {
    gpress::api::ARCHIVE_VERSION as u32
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
            assert_eq!(gpress_compress(p, input.len(), 100, 0, 0, 0, 0), 0);
            let archive = std::slice::from_raw_parts(gpress_out_ptr(), gpress_out_len()).to_vec();
            gpress_free_input(p, input.len());
            let q = gpress_alloc(archive.len());
            std::ptr::copy_nonoverlapping(archive.as_ptr(), q, archive.len());
            assert_eq!(gpress_expand(q, archive.len(), 0, 0, 0), 0);
            assert_eq!(std::slice::from_raw_parts(gpress_out_ptr(), gpress_out_len()), &input[..]);
            // A declared length over max_len (1 MiB here is plenty for 15 bytes, so a limit of 0 MiB can't be
            // asked for; the native test uses the library directly for that).
            gpress_free_input(q, archive.len());
            // Damaged: code 2 and a message.
            let bad = gpress_alloc(3);
            assert_eq!(gpress_expand(bad, 3, 0, 0, 0), 2);
            assert!(gpress_message_len() > 0);
            gpress_free_input(bad, 3);
            gpress_clear_out();
            assert_eq!(gpress_out_len(), 0);
            // Not a PDF: untouched with a reason.
            let r = gpress_alloc(5);
            std::ptr::copy_nonoverlapping(b"hello".as_ptr(), r, 5);
            assert_eq!(gpress_optimize(r, 5, 100, 0, 0, 0, FLAG_NO_JBIG2 | FLAG_IGNORE_CONFORMANCE), 0);
            assert_eq!(gpress_outcome(), 2);
            assert!(gpress_message_len() > 0);
            gpress_free_input(r, 5);
        }
        assert_eq!(gpress_archive_version(), 8);
    }

    #[test]
    fn the_flags_and_sizes_reach_the_options() {
        let o = options(30, 3, 5, 9, FLAG_NO_JPEG_TABLES | FLAG_KEEP_ENCRYPTED | FLAG_NO_DECRYPT);
        assert_eq!(
            (o.effort.level(), o.max_input, o.max_stream, o.max_stream_archive),
            (30, 3 << 20, 5 << 20, 5 << 20)
        );
        assert_eq!(o.objects_per_stream, 9);
        assert!(!o.jpeg_tables && !o.rewrite_encrypted && !o.decrypt);
        assert!(o.jbig2 && o.respect_conformance && o.flate_over_jpeg && o.use_existing_libraries);
        let d = options(100, 0, 0, 0, 0);
        assert_eq!(d, gpress::Options::default().threads(1));
        // A size past the module's address space is clamped, never wrapped; a budget is a u64 everywhere.
        assert_eq!(mib(u32::MAX, 0), usize::try_from((u32::MAX as u64) << 20).unwrap_or(usize::MAX));
        assert_eq!(budget(u32::MAX, 0), (u32::MAX as u64) << 20);
        assert_eq!(budget(0, 7), 7);
    }
}
