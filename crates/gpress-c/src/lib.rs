//! gpress-c: the C API. Every function is safe to call from any language with a C FFI: it never panics
//! across the boundary, never reads outside the buffers the caller passes, and every buffer it hands out
//! is freed with `gpress_free`.
//!
//! The header `include/gpress.h` is the contract; keep the two in step.

use std::ffi::{c_char, c_int, CString};
use std::ptr;
use std::time::Duration;

/// Options, laid out for C (the same fields as `gpress::Options`; `include/gpress.h` describes each).
/// Fill one with `gpress_options_default` and change what you need: a zeroed struct is not the defaults
/// (it means effort 0 and every switch off), though every size field left at 0 means its default.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GpressOptions {
    /// 0..=100
    pub effort: u8,
    /// Worker threads (0 = every core the machine offers; at most 64).
    pub threads: u32,
    /// Largest accepted input in bytes (0 = 1 GiB).
    pub max_input: u64,
    /// Normal PDF: largest decoded stream re-encoded (0 = 64 MiB).
    pub max_stream: u64,
    /// Archive: largest decoded stream stored decoded (0 = 256 MiB).
    pub max_stream_archive: u64,
    /// Archive, both ways: most decoded bytes the parts of one archive may claim in all (0 = 256 MiB).
    pub max_decoded: u64,
    /// Expand: largest file restored; an archive declaring more is refused (0 = 1 GiB).
    pub max_len: u64,
    /// Normal PDF: most objects per object stream (0 = 200).
    pub objects_per_stream: u32,
    /// Wall-time budget in milliseconds (0 = none): the call then fails with GPRESS_ERR_TIME_BUDGET.
    pub time_budget_ms: u64,
    /// 1 = leave PDF/A, PDF/X, signed and certified files untouched (the safe default), 0 = optimise them too.
    pub respect_conformance: u8,
    /// 1 = also try Flate in front of JPEG images.
    pub flate_over_jpeg: u8,
    /// 1 = give JPEG images optimal Huffman tables.
    pub jpeg_tables: u8,
    /// 1 = write CCITT fax images as JBIG2 when that is clearly smaller.
    pub jbig2: u8,
    /// 1 = rewrite files the empty password opens, keeping their encryption.
    pub rewrite_encrypted: u8,
    /// 1 = archive: store the streams of such files decrypted (encrypted again on expand).
    pub decrypt: u8,
    /// 1 = use the optional existing library (Lepton) when the build has it.
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

/// A buffer for C. An empty output is a null pointer with length 0, never a dangling one.
fn buffer_from(mut v: Vec<u8>) -> GpressBuffer {
    if v.is_empty() {
        return empty_buffer();
    }
    v.shrink_to_fit();
    let b = GpressBuffer { data: v.as_mut_ptr(), len: v.len(), cap: v.capacity() };
    std::mem::forget(v);
    b
}

fn message(s: &str) -> *mut c_char {
    // A NUL inside the text would truncate it; replace rather than fail.
    CString::new(s.replace('\0', " ")).map(CString::into_raw).unwrap_or(ptr::null_mut())
}

/// A C size field: 0 keeps the default, anything else is clamped to what this platform addresses.
fn size(v: u64, default: usize) -> usize {
    if v == 0 {
        default
    } else {
        usize::try_from(v).unwrap_or(usize::MAX)
    }
}

fn options_from(o: Option<&GpressOptions>) -> gpress::Options {
    let mut opt = gpress::Options::default();
    if let Some(o) = o {
        opt.effort = gpress::Effort::new(o.effort);
        opt.threads = o.threads as usize;
        opt.max_input = size(o.max_input, opt.max_input);
        opt.max_stream = size(o.max_stream, opt.max_stream);
        opt.max_stream_archive = size(o.max_stream_archive, opt.max_stream_archive);
        if o.max_decoded > 0 {
            opt.max_decoded = o.max_decoded;
        }
        if o.max_len > 0 {
            opt.max_len = o.max_len;
        }
        if o.objects_per_stream > 0 {
            opt.objects_per_stream = o.objects_per_stream as usize;
        }
        opt.time_budget = if o.time_budget_ms > 0 { Some(Duration::from_millis(o.time_budget_ms)) } else { None };
        opt.respect_conformance = o.respect_conformance != 0;
        opt.flate_over_jpeg = o.flate_over_jpeg != 0;
        opt.jpeg_tables = o.jpeg_tables != 0;
        opt.jbig2 = o.jbig2 != 0;
        opt.rewrite_encrypted = o.rewrite_encrypted != 0;
        opt.decrypt = o.decrypt != 0;
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

/// A null input with a length: nothing to read, so refused (GPRESS_ERR_BAD_ARGUMENT).
const BAD_ARGUMENT: c_int = 5;

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
        threads: d.threads as u32,
        max_input: d.max_input as u64,
        max_stream: d.max_stream as u64,
        max_stream_archive: d.max_stream_archive as u64,
        max_decoded: d.max_decoded,
        max_len: d.max_len,
        objects_per_stream: d.objects_per_stream as u32,
        time_budget_ms: 0,
        respect_conformance: d.respect_conformance as u8,
        flate_over_jpeg: d.flate_over_jpeg as u8,
        jpeg_tables: d.jpeg_tables as u8,
        jbig2: d.jbig2 as u8,
        rewrite_encrypted: d.rewrite_encrypted as u8,
        decrypt: d.decrypt as u8,
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
    let Some(data) = input_slice(input, len) else { return failure(BAD_ARGUMENT, "null input with a non-zero length") };
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
    let Some(data) = input_slice(input, len) else { return failure(BAD_ARGUMENT, "null input with a non-zero length") };
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
    let Some(data) = input_slice(input, len) else { return failure(BAD_ARGUMENT, "null input with a non-zero length") };
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

/// Frees a message from a result, given the address of the field: the message is freed and the field set
/// to null, so freeing it again, or freeing the whole result afterwards, does nothing. Safe on null and on a
/// field that is already null.
///
/// # Safety
/// `msg` must be null or point to a field holding null or a message returned by this library.
#[no_mangle]
pub unsafe extern "C" fn gpress_free_message(msg: *mut *mut c_char) {
    if msg.is_null() {
        return;
    }
    let m = ptr::replace(msg, ptr::null_mut());
    if !m.is_null() {
        // SAFETY: a non-null field holds a pointer `message` made with CString::into_raw, and the field is
        // nulled above, so it is freed once.
        drop(CString::from_raw(m));
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
    gpress_free_message(&mut r.message);
    r.code = 0;
    r.outcome = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header's code for a part this build leaves out is the library's.
    #[test]
    fn the_header_has_the_code_for_a_missing_feature() {
        let code = gpress::Error::NeedsFeature(gpress::Feature::Archive).code();
        assert!(include_str!("../include/gpress.h").contains(&format!("GPRESS_ERR_NEEDS_FEATURE = {code}")));
    }

    /// Without the archive, compress and expand (which return the library's error as it is) give that code,
    /// with a message that names the feature.
    #[test]
    #[cfg(not(feature = "archive"))]
    fn the_archive_calls_say_they_need_the_archive() {
        let opt = options_from(None);
        let input = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n";
        for r in [gpress::compress(input, &opt).map(|_| ()), gpress::expand(input, &opt).map(|_| ())] {
            let e = r.unwrap_err();
            assert_eq!(e.code(), 10);
            assert!(e.to_string().contains("`archive`"), "{e}");
        }
        assert!(gpress::optimize(input, &opt).is_ok());
    }

    #[test]
    #[cfg(feature = "archive")]
    fn round_trip_through_the_c_surface() {
        let input = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n".to_vec();
        // SAFETY: a zeroed options struct is plain data; gpress_options_default then fills it.
        let mut o: GpressOptions = unsafe { std::mem::zeroed() };
        unsafe {
            gpress_options_default(&mut o);
            assert_eq!((o.effort, o.jbig2, o.decrypt, o.max_len, o.max_decoded), (100, 1, 1, 1 << 30, 256 << 20));
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
            // Null with length: refused, not read. The message freed by its address, then again, then with
            // the whole result: the field is nulled by the first free, so nothing is freed twice.
            let mut r = gpress_expand(ptr::null(), 10, ptr::null());
            assert_eq!(r.code, BAD_ARGUMENT);
            assert!(!r.message.is_null());
            gpress_free_message(&mut r.message);
            assert!(r.message.is_null());
            gpress_free_message(&mut r.message);
            gpress_result_free(&mut r);
            gpress_free_message(ptr::null_mut());
            // An empty output (an archive of nothing restores nothing; a non-PDF of no bytes is untouched):
            // a null pointer with length 0, never a dangling one, and freeing it does nothing.
            let mut c0 = gpress_compress(ptr::null(), 0, &o);
            assert_eq!(c0.code, 0);
            let mut e0 = gpress_expand(c0.output.data, c0.output.len, &o);
            assert!(e0.code == 0 && e0.output.data.is_null() && e0.output.len == 0);
            let mut p0 = gpress_optimize(ptr::null(), 0, &o);
            assert!(p0.code == 0 && p0.output.data.is_null() && p0.output.len == 0);
            gpress_result_free(&mut c0);
            gpress_result_free(&mut e0);
            gpress_result_free(&mut p0);
            // Options the engine refuses: the library's code and message, nothing allocated for output.
            let mut too_many = o;
            too_many.threads = 65;
            let mut r = gpress_compress(input.as_ptr(), input.len(), &too_many);
            assert_eq!(r.code, 5);
            assert!(r.output.data.is_null() && !r.message.is_null());
            gpress_result_free(&mut r);
            // A declared length over max_len: refused before anything is decoded.
            let mut small = o;
            small.max_len = 4;
            let mut r = gpress_expand(archive.as_ptr(), archive.len(), &small);
            assert_eq!(r.code, 7);
            gpress_result_free(&mut r);
            // A declared decoded total over max_decoded: the same, with its own code. The archive of a
            // stream of 160 KB of text claims 160 KB; with a budget of 1 byte on compress the stream is
            // stored as it is, so that archive restores under the same budget (the wide one is refused).
            let text: Vec<u8> = (0..4000)
                .flat_map(|i| format!("BT /F1 12 Tf 72 {} Td (Line {i}) Tj ET\n", i % 60 * 11).into_bytes())
                .collect();
            let z = gpress::flate::zlibexact::zlib_stream(
                &text,
                gpress::flate::zlibexact::Params { level: 6, window_bits: 15, mem_level: 8, filtered: false },
            );
            let mut pdf =
                format!("%PDF-1.4\n1 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n", z.len()).into_bytes();
            pdf.extend_from_slice(&z);
            pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
            let mut wide = gpress_compress(pdf.as_ptr(), pdf.len(), &o);
            assert_eq!(wide.code, 0);
            let mut narrow = o;
            narrow.max_decoded = 1;
            let mut r = gpress_expand(wide.output.data, wide.output.len, &narrow);
            assert_eq!(r.code, 9);
            assert!(!r.message.is_null() && r.output.data.is_null());
            gpress_result_free(&mut r);
            let mut stored = gpress_compress(pdf.as_ptr(), pdf.len(), &narrow);
            assert_eq!(stored.code, 0);
            assert!(
                std::slice::from_raw_parts(stored.output.data, stored.output.len)
                    != std::slice::from_raw_parts(wide.output.data, wide.output.len)
            );
            let mut back = gpress_expand(stored.output.data, stored.output.len, &narrow);
            assert!(back.code == 0 && std::slice::from_raw_parts(back.output.data, back.output.len) == &pdf[..]);
            gpress_result_free(&mut wide);
            gpress_result_free(&mut stored);
            gpress_result_free(&mut back);
            // Double free is harmless.
            let mut z = empty_buffer();
            gpress_free(&mut z);
            gpress_free(ptr::null_mut());
        }
    }
}
