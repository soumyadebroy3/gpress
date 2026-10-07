//! gpress-c: the C API. Every function is safe to call from any language with a C FFI: it never panics
//! across the boundary, never reads outside the buffers the caller passes, and every buffer it hands out
//! is freed with `gpress_free`.
//!
//! The header `include/gpress.h` is the contract; keep the two in step.

use std::ffi::{c_char, c_int, CString};
use std::ptr;
use std::time::Duration;

/// Options, laid out for C. `gpress_options_default` fills one in; a zeroed struct is also valid and
/// means "defaults" for every field that is 0 (effort 0 means quick, so set it explicitly).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GpressOptions {
    /// 0..=100
    pub effort: u8,
    /// Largest accepted input in bytes (0 = 1 GiB).
    pub max_input: u64,
    /// Largest decoded stream kept in memory (0 = 64 MiB).
    pub max_stream: u64,
    /// Worker threads (0 = one per CPU).
    pub threads: u32,
    /// Wall-time budget in milliseconds (0 = none).
    pub time_budget_ms: u64,
    /// 1 = leave PDF/A, PDF/X, signed and certified files untouched (the safe default), 0 = optimise them too.
    pub respect_conformance: u8,
    /// 1 = also try Flate in front of JPEG images.
    pub flate_over_jpeg: u8,
    /// 1 = use the optional existing libraries when the build has them.
    pub use_existing_libraries: u8,
}

/// A buffer gpress allocated. Free it with `gpress_free`; never with `free()`.
#[repr(C)]
pub struct GpressBuffer {
    pub data: *mut u8,
    pub len: usize,
    cap: usize,
}

/// What `gpress_optimize` did (the `outcome` field of `GpressResult`).
pub const GPRESS_OUTCOME_SMALLER: c_int = 1;
pub const GPRESS_OUTCOME_UNTOUCHED: c_int = 2;

/// Result of a call. `code` 0 = success; otherwise an error code matching `gpress::Error::code` and a
/// message in `message` (free with `gpress_free_message`).
#[repr(C)]
pub struct GpressResult {
    pub code: c_int,
    pub outcome: c_int,
    pub output: GpressBuffer,
    pub message: *mut c_char,
}

fn empty_buffer() -> GpressBuffer {
    GpressBuffer { data: ptr::null_mut(), len: 0, cap: 0 }
}

fn buffer_from(mut v: Vec<u8>) -> GpressBuffer {
    v.shrink_to_fit();
    let b = GpressBuffer { data: v.as_mut_ptr(), len: v.len(), cap: v.capacity() };
    std::mem::forget(v);
    b
}

fn message(s: &str) -> *mut c_char {
    // A NUL inside the text would truncate it; replace rather than fail.
    CString::new(s.replace('\0', " ")).map(CString::into_raw).unwrap_or(ptr::null_mut())
}

fn options_from(o: Option<&GpressOptions>) -> gpress::Options {
    let mut opt = gpress::Options::default();
    if let Some(o) = o {
        opt.effort = gpress::Effort::new(o.effort);
        if o.max_input > 0 {
            opt.max_input = o.max_input.min(usize::MAX as u64) as usize;
        }
        if o.max_stream > 0 {
            opt.max_stream = o.max_stream.min(usize::MAX as u64) as usize;
        }
        opt.threads = o.threads as usize;
        opt.time_budget = if o.time_budget_ms > 0 { Some(Duration::from_millis(o.time_budget_ms)) } else { None };
        opt.respect_conformance = o.respect_conformance != 0;
        opt.flate_over_jpeg = o.flate_over_jpeg != 0;
        opt.use_existing_libraries = o.use_existing_libraries != 0;
    }
    opt
}

/// # Safety
/// `input` must point to `len` readable bytes (or be null with `len` 0); `options` may be null.
unsafe fn input_slice<'a>(input: *const u8, len: usize) -> Option<&'a [u8]> {
    if input.is_null() {
        if len == 0 {
            return Some(&[]);
        }
        return None;
    }
    // SAFETY: the caller promises `len` readable bytes at `input` (documented contract).
    Some(std::slice::from_raw_parts(input, len))
}

fn failure(code: c_int, msg: &str) -> GpressResult {
    GpressResult { code, outcome: 0, output: empty_buffer(), message: message(msg) }
}

fn from_error(e: gpress::Error) -> GpressResult {
    failure(e.code(), &e.to_string())
}

/// The library version as a static C string (do not free).
#[no_mangle]
pub extern "C" fn gpress_version() -> *const c_char {
    static V: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();
    V.as_ptr() as *const c_char
}

/// Fills `out` with the default options. A null pointer is ignored.
///
/// # Safety
/// `out` must be null or point to writable memory for one `GpressOptions`.
#[no_mangle]
pub unsafe extern "C" fn gpress_options_default(out: *mut GpressOptions) {
    if out.is_null() {
        return;
    }
    let d = gpress::Options::default();
    let o = GpressOptions {
        effort: d.effort.level(),
        max_input: d.max_input as u64,
        max_stream: d.max_stream as u64,
        threads: d.threads as u32,
        time_budget_ms: 0,
        respect_conformance: d.respect_conformance as u8,
        flate_over_jpeg: d.flate_over_jpeg as u8,
        use_existing_libraries: d.use_existing_libraries as u8,
    };
    // SAFETY: `out` is non-null and the caller owns the struct it points to.
    unsafe { ptr::write(out, o) };
}

/// A smaller PDF that any reader opens, or the input untouched (outcome tells which).
///
/// # Safety
/// `input` must point to `len` readable bytes; `options` may be null for defaults.
#[no_mangle]
pub unsafe extern "C" fn gpress_optimize(input: *const u8, len: usize, options: *const GpressOptions) -> GpressResult {
    let Some(data) = input_slice(input, len) else { return failure(5, "null input with a non-zero length") };
    let opt = options_from(options.as_ref());
    match gpress::optimize(data, &opt) {
        Ok(r) => {
            let outcome = match r.outcome {
                gpress::Outcome::Smaller(_) => GPRESS_OUTCOME_SMALLER,
                gpress::Outcome::Untouched(_) => GPRESS_OUTCOME_UNTOUCHED,
            };
            let msg = match &r.outcome {
                gpress::Outcome::Untouched(why) => message(why),
                _ => ptr::null_mut(),
            };
            GpressResult { code: 0, outcome, output: buffer_from(r.data), message: msg }
        }
        Err(e) => from_error(e),
    }
}

/// The gpress archive of any input (restored byte for byte by `gpress_expand`).
///
/// # Safety
/// `input` must point to `len` readable bytes; `options` may be null for defaults.
#[no_mangle]
pub unsafe extern "C" fn gpress_compress(input: *const u8, len: usize, options: *const GpressOptions) -> GpressResult {
    let Some(data) = input_slice(input, len) else { return failure(5, "null input with a non-zero length") };
    let opt = options_from(options.as_ref());
    match gpress::compress(data, &opt) {
        Ok(c) => GpressResult {
            code: 0,
            outcome: if c.stored { GPRESS_OUTCOME_UNTOUCHED } else { GPRESS_OUTCOME_SMALLER },
            output: buffer_from(c.data),
            message: ptr::null_mut(),
        },
        Err(e) => from_error(e),
    }
}

/// The original bytes from an archive; a damaged archive gives an error, never a wrong file.
///
/// # Safety
/// `input` must point to `len` readable bytes; `options` may be null for defaults.
#[no_mangle]
pub unsafe extern "C" fn gpress_expand(input: *const u8, len: usize, options: *const GpressOptions) -> GpressResult {
    let Some(data) = input_slice(input, len) else { return failure(5, "null input with a non-zero length") };
    let opt = options_from(options.as_ref());
    match gpress::expand(data, &opt) {
        Ok(v) => GpressResult { code: 0, outcome: 0, output: buffer_from(v), message: ptr::null_mut() },
        Err(e) => from_error(e),
    }
}

/// Frees a buffer from a result. Safe on an empty or already-cleared buffer; the buffer is zeroed after.
///
/// # Safety
/// `buf` must be null or point to a `GpressBuffer` returned by this library and not freed before.
#[no_mangle]
pub unsafe extern "C" fn gpress_free(buf: *mut GpressBuffer) {
    if buf.is_null() {
        return;
    }
    let b = &mut *buf;
    if !b.data.is_null() && b.cap > 0 {
        // SAFETY: the triple came from `buffer_from`, which forgot a Vec with exactly these values.
        drop(Vec::from_raw_parts(b.data, b.len, b.cap));
    }
    *b = empty_buffer();
}

/// Frees a message from a result. Safe on null.
///
/// # Safety
/// `msg` must be null or a message returned by this library and not freed before.
#[no_mangle]
pub unsafe extern "C" fn gpress_free_message(msg: *mut c_char) {
    if !msg.is_null() {
        drop(CString::from_raw(msg));
    }
}

/// Frees everything in a result (buffer and message) and zeroes it.
///
/// # Safety
/// `res` must be null or point to a result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn gpress_result_free(res: *mut GpressResult) {
    if res.is_null() {
        return;
    }
    let r = &mut *res;
    gpress_free(&mut r.output);
    gpress_free_message(r.message);
    r.message = ptr::null_mut();
    r.code = 0;
    r.outcome = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_the_c_surface() {
        let input = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n".to_vec();
        let mut o = GpressOptions {
            effort: 0,
            max_input: 0,
            max_stream: 0,
            threads: 1,
            time_budget_ms: 0,
            respect_conformance: 1,
            flate_over_jpeg: 1,
            use_existing_libraries: 1,
        };
        unsafe {
            gpress_options_default(&mut o);
            let mut c = gpress_compress(input.as_ptr(), input.len(), &o);
            assert_eq!(c.code, 0);
            let archive = std::slice::from_raw_parts(c.output.data, c.output.len).to_vec();
            let mut e = gpress_expand(archive.as_ptr(), archive.len(), &o);
            assert_eq!(e.code, 0);
            assert_eq!(std::slice::from_raw_parts(e.output.data, e.output.len), &input[..]);
            gpress_result_free(&mut c);
            gpress_result_free(&mut e);
            assert!(c.output.data.is_null() && e.message.is_null());
            // Damaged input: an error code and a message, nothing else.
            let mut bad = gpress_expand(b"xx".as_ptr(), 2, ptr::null());
            assert_eq!(bad.code, 2);
            assert!(!bad.message.is_null());
            gpress_result_free(&mut bad);
            // Null with length: refused, not read.
            let r = gpress_expand(ptr::null(), 10, ptr::null());
            assert_eq!(r.code, 5);
            gpress_free_message(r.message);
            // Double free is harmless.
            let mut z = empty_buffer();
            gpress_free(&mut z);
            gpress_free(ptr::null_mut());
        }
    }
}
