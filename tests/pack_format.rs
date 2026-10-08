//! The peyaji format: exact restore, and damaged files refused (never a wrong result, never a crash).

use engine::flate::zlibexact::{zlib_stream, Params};
use engine::pack::{pack, unpack};

/// A small PDF whose one content stream names its filter by reference (`/Filter 3 0 R`).
fn pdf_with_indirect_filter() -> Vec<u8> {
    let mut content = Vec::new();
    for i in 0..200 {
        content.extend_from_slice(format!("BT /F1 12 Tf 72 {} Td (Line {i} of the test page) Tj ET\n", 700 - i * 3).as_bytes());
    }
    let z = zlib_stream(&content, Params { level: 6, window_bits: 15, mem_level: 8, filtered: false });
    let mut d = Vec::new();
    d.extend_from_slice(b"%PDF-1.4\n");
    d.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    d.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");
    d.extend_from_slice(b"3 0 obj\n[/FlateDecode]\nendobj\n");
    d.extend_from_slice(b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>\nendobj\n");
    d.extend_from_slice(format!("5 0 obj\n<< /Length {} /Filter 3 0 R >>\nstream\n", z.len()).as_bytes());
    d.extend_from_slice(&z);
    d.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Size 6 /Root 1 0 R >>\n%%EOF\n");
    d
}

#[test]
fn restores_exactly_and_rebuilds_streams_found_by_their_bytes() {
    let pdf = pdf_with_indirect_filter();
    let (packed, stats) = pack(&pdf);
    assert_eq!(stats.rebuilt, 1, "{stats:?}");
    assert!(packed.len() < pdf.len() / 2, "{} -> {}", pdf.len(), packed.len());
    assert_eq!(unpack(&packed).unwrap(), pdf);
}

#[test]
fn restores_anything_exactly() {
    let mut x: u32 = 12345;
    let random: Vec<u8> = (0..5000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    for input in [Vec::new(), b"%PDF-1.7\n".to_vec(), random, b"stream\nendstream".repeat(50)] {
        let (packed, _) = pack(&input);
        assert_eq!(unpack(&packed).unwrap(), input);
    }
}

#[test]
fn refuses_damaged_files() {
    let pdf = pdf_with_indirect_filter();
    let (packed, _) = pack(&pdf);
    let mut cases: Vec<Vec<u8>> = Vec::new();
    // Every single-bit change in the first 64 bytes after the header, and a sample of the rest.
    for i in (25..packed.len()).filter(|i| *i < 89 || i % 7 == 0) {
        for bit in [0, 3, 7] {
            let mut b = packed.clone();
            b[i] ^= 1 << bit;
            cases.push(b);
        }
    }
    // Truncated anywhere.
    for cut in (0..packed.len()).step_by(5) {
        cases.push(packed[..cut].to_vec());
    }
    // Garbage after a valid header, and an impossible size in the header.
    cases.push([&packed[..25], &[0xA5; 3000][..]].concat());
    let mut huge = packed.clone();
    huge[5..10].copy_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x7f]);
    cases.push(huge);
    for (k, c) in cases.iter().enumerate() {
        if let Ok(out) = unpack(c) {
            assert_eq!(out, pdf, "case {k}: a damaged file restored to something else");
        }
    }
}

/// A PDF packed by the first build of the current format version (`tests/data/format-v7.*`: syntax, a content
/// stream, an image, long repeats and noise). It must still restore, and packing it again must give the same
/// bytes: the model is part of the format, so a faster model has to compute exactly the same thing.
#[test]
fn stored_format_files_still_restore_and_pack_the_same() {
    let pdf = include_bytes!("data/format-v7.pdf");
    let pyj = include_bytes!("data/format-v7.pyj");
    assert_eq!(unpack(pyj).unwrap(), pdf);
    assert!(pack(pdf).0 == pyj);
    // One bit changed is refused, not restored wrongly.
    let mut bad = pyj.to_vec();
    bad[pyj.len() / 2] ^= 8;
    assert!(unpack(&bad).is_err());
}
