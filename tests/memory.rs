//! Memory: optimizing a PDF with many images takes memory in proportion to the file, a few copies of it at
//! most, never a remembered copy of every encode (outputs are remembered per stream, and dropped with it).
//! The allocator below counts the bytes in use, so this file holds a single test.

use gpress::flate::zlibexact::{zlib_stream, Params};
use gpress::pdf::rewrite::{optimize, Options, Outcome};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = NOW.fetch_add(by, Ordering::SeqCst) + by;
    PEAK.fetch_max(now, Ordering::SeqCst);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        NOW.fetch_sub(l.size(), Ordering::SeqCst);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                grew(new - l.size());
            } else {
                NOW.fetch_sub(l.size() - new, Ordering::SeqCst);
            }
        }
        q
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The most bytes in use while `f` runs, above what was in use before.
fn peak_during(f: impl FnOnce()) -> usize {
    let before = NOW.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    f();
    PEAK.load(Ordering::SeqCst) - before
}

/// A page with `k` different photo-like RGB images (a gradient plus noise, so Flate shrinks them only a
/// little), stored with zlib level 1.
fn pdf_with_images(k: usize, w: usize, h: usize) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()];
    let names: String = (0..k).map(|i| format!("/I{i} {} 0 R ", 5 + i)).collect();
    objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << {names}>> >> >>").into_bytes());
    let content: String = (0..k).map(|i| format!("q 50 0 0 50 {} {} cm /I{i} Do Q\n", i * 55 % 550, i * 60 % 700)).collect();
    let stream = |dict: &str, data: &[u8]| -> Vec<u8> {
        let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        o.extend_from_slice(data);
        o.extend_from_slice(b"\nendstream");
        o
    };
    let z = |d: &[u8]| zlib_stream(d, Params { level: 1, window_bits: 15, mem_level: 8, filtered: false });
    objs.push(stream("/Filter /FlateDecode", &z(content.as_bytes())));
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    for i in 0..k {
        let mut px = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                for c in 0..3 {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    px.push(((x * 255 / w + y * (c + 1) + i * 37) as u64 + s % 25) as u8);
                }
            }
        }
        objs.push(stream(&format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode"), &z(&px)));
    }
    let mut pdf = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(o);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    pdf
}

#[test]
fn memory_follows_the_file_not_every_encode() {
    // Two files that differ by 8 images (both with enough images to keep every thread busy).
    let (w, h) = (128, 128);
    let few = pdf_with_images(3, w, h);
    let many = pdf_with_images(11, w, h);
    // The stored size of one image, about what each takes in the file.
    let per_image = (many.len() - few.len()) / 8;
    for effort in [0u8, 100] {
        for threads in [1usize, 2] {
            let opt = Options { threads, ..Options::effort(effort) };
            let peak = |pdf: &[u8]| {
                peak_during(|| {
                    let r = optimize(pdf, &opt);
                    assert!(matches!(r.outcome, Outcome::Smaller(_)), "{:?}", r.outcome);
                })
            };
            let (p_few, p_many) = (peak(&few), peak(&many));
            // Each image is held a few times: parsed, written, in the output and read back for the check
            // (about 2 times its stored size, measured). Remembering every encode of every image took 5 to 7.
            let extra = p_many.saturating_sub(p_few) / 8;
            eprintln!("effort {effort}, {threads} threads: peak {p_few} for 3 images, {p_many} for 11: {extra} per image of {per_image} bytes");
            assert!(extra <= 4 * per_image, "effort {effort}, {threads} threads: {extra} bytes per image of {per_image}");
        }
    }
}
