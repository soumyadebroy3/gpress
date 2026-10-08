//! Speed options: the thread count never changes any output, every effort gives a checked result (a smaller
//! file that reads back the same, or the file untouched), and damaged input is never "improved".

use engine::flate::zlibexact::{zlib_stream, Params};
use engine::pack::{pack_with, unpack, PackOptions};
use engine::pdf::rewrite::{optimize, Options, Outcome};

fn zlib(data: &[u8]) -> Vec<u8> {
    zlib_stream(data, Params { level: 6, window_bits: 15, mem_level: 8, filtered: false })
}

fn rng(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// A small PDF with the kinds of streams the speed work touches: page content, the same font program in
/// two objects (encoded once), an 8-bit image without a predictor and one with PNG rows, a scan-like image
/// with long repeats, and an incompressible stream.
fn sample_pdf() -> Vec<u8> {
    let mut s = 0x1234_5678u64;
    let mut content = Vec::new();
    for i in 0..300 {
        content.extend_from_slice(format!("BT /F1 12 Tf 72 {} Td (Line {i} of the page) Tj ET\n", 700 - (i % 60) * 11).as_bytes());
    }
    let font: Vec<u8> = (0..20_000u32).map(|i| (i * 7 % 251) as u8 ^ ((i / 100) as u8)).collect();
    let (w, h) = (300usize, 200usize);
    let gray: Vec<u8> = (0..w * h).map(|i| ((i % w) / 3 + (i / w) / 2) as u8).collect();
    // The same picture as PNG "Sub" rows.
    let mut png = Vec::new();
    for r in 0..h {
        png.push(1u8);
        for c in 0..w {
            let x = gray[r * w + c];
            png.push(if c == 0 { x } else { x.wrapping_sub(gray[r * w + c - 1]) });
        }
    }
    // A scan-like page: white with a few dark lines (very repetitive, long matches).
    let (bw, bh) = (600usize, 400usize);
    let mut scan = vec![255u8; bw * bh];
    for r in (40..bh).step_by(37) {
        for c in 30..bw - 30 {
            scan[r * bw + c] = 0;
        }
    }
    let noise: Vec<u8> = (0..30_000).map(|_| rng(&mut s) as u8).collect();

    let mut objs: Vec<Vec<u8>> = Vec::new();
    objs.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objs.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
    objs.push(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R /F2 6 0 R >> /XObject << /I1 7 0 R /I2 8 0 R /I3 9 0 R /N 10 0 R >> >> >>".to_vec());
    let stream = |dict: &str, data: &[u8]| -> Vec<u8> {
        let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        o.extend_from_slice(data);
        o.extend_from_slice(b"\nendstream");
        o
    };
    objs.push(stream("/Filter /FlateDecode", &zlib(&content)));
    objs.push(stream("/Length1 20000 /Filter /FlateDecode", &zlib(&font)));
    objs.push(stream("/Length1 20000 /Filter /FlateDecode", &zlib(&font)));
    objs.push(stream(&format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode"), &zlib(&gray)));
    objs.push(stream(
        &format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /DecodeParms << /Predictor 15 /Columns {w} >>"),
        &zlib(&png),
    ));
    objs.push(stream(&format!("/Type /XObject /Subtype /Image /Width {bw} /Height {bh} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode"), &zlib(&scan)));
    objs.push(stream("", &noise));
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

fn opts(effort: u8, threads: usize) -> Options {
    Options { threads, ..Options::effort(effort) }
}

#[test]
fn the_thread_count_never_changes_the_output() {
    let pdf = sample_pdf();
    for effort in [0u8, 40, 100] {
        let one = optimize(&pdf, &opts(effort, 1));
        assert!(matches!(one.outcome, Outcome::Smaller(_)), "effort {effort}: {:?}", one.outcome);
        for threads in [2, 3, 8] {
            let r = optimize(&pdf, &opts(effort, threads));
            assert!(r.data == one.data, "effort {effort}: {threads} threads give other bytes");
        }
    }
}

#[test]
fn every_effort_gives_a_checked_smaller_file() {
    let pdf = sample_pdf();
    let mut sizes = Vec::new();
    for effort in [0u8, 1, 25, 50, 75, 99, 100] {
        let r = optimize(&pdf, &opts(effort, 2));
        match r.outcome {
            // The writer reads its output back and compares every object before returning it.
            Outcome::Smaller(st) => {
                assert_eq!(st.streams, 7);
                assert!(r.data.len() < pdf.len());
            }
            Outcome::Untouched(why) => panic!("effort {effort}: untouched ({why})"),
        }
        sizes.push(r.data.len());
    }
    // The strongest setting is never beaten by the quick one on this file.
    assert!(sizes[6] <= sizes[0], "{sizes:?}");
}

#[test]
fn identical_streams_are_written_identically() {
    let pdf = sample_pdf();
    let r = optimize(&pdf, &opts(100, 2));
    // Both copies of the font program are re-encoded the same way.
    let key = b"/Length1 20000";
    let starts: Vec<usize> = (0..r.data.len() - key.len()).filter(|&i| &r.data[i..i + key.len()] == key).collect();
    assert_eq!(starts.len(), 2);
    let body = |i: usize| -> &[u8] {
        let s = i + r.data[i..].windows(7).position(|w| w == b"stream\n").unwrap() + 7;
        let e = s + r.data[s..].windows(10).position(|w| w == b"\nendstream").unwrap();
        &r.data[s..e]
    };
    assert_eq!(body(starts[0]), body(starts[1]));
}

#[test]
fn damaged_input_is_left_untouched_at_every_effort() {
    let pdf = sample_pdf();
    // Cut short, cross-reference offsets off by one, and a damaged Flate stream behind a correct table.
    let cut = pdf[..pdf.len() * 2 / 3].to_vec();
    let mut shifted = pdf.clone();
    shifted.insert(20, b' ');
    let mut bad_flate = pdf.clone();
    let k = bad_flate.windows(6).position(|w| w == b"stream").unwrap() + 20;
    bad_flate[k] ^= 0xff;
    for effort in [0u8, 50, 100] {
        for threads in [1, 2] {
            for (name, input) in [("cut", &cut), ("shifted", &shifted)] {
                let r = optimize(input, &opts(effort, threads));
                assert!(matches!(r.outcome, Outcome::Untouched(_)), "{name} at effort {effort}");
                assert_eq!(&r.data, input);
            }
            // The damaged stream is kept exactly as it was; everything else may still be improved.
            let r = optimize(&bad_flate, &opts(effort, threads));
            if let Outcome::Smaller(st) = &r.outcome {
                assert!(st.streams_kept >= 1);
                let start = bad_flate.windows(6).position(|w| w == b"stream").unwrap() + 7;
                let original = &bad_flate[start..start + 64];
                assert!(r.data.windows(64).any(|w| w == original), "damaged stream changed");
            }
        }
    }
}

#[test]
fn pack_options_change_speed_never_the_restore() {
    let pdf = sample_pdf();
    let mut packed = Vec::new();
    for effort in [0u8, 49, 50, 100] {
        let (one, st) = pack_with(&pdf, &PackOptions { effort, threads: 1 });
        assert_eq!(unpack(&one).unwrap(), pdf, "effort {effort}");
        // Plain coding of the incompressible stream only below effort 50.
        assert_eq!(st.flat_bytes > 0, effort < 50, "effort {effort}: {st:?}");
        for threads in [2, 4] {
            let (z, _) = pack_with(&pdf, &PackOptions { effort, threads });
            assert!(z == one, "effort {effort}: {threads} threads give other bytes");
        }
        packed.push(one);
    }
    // Effort 100 is the default.
    assert!(engine::pack::pack(&pdf).0 == packed[3]);
}

/// A PDF of the given objects (numbered from 1, object 1 the catalog), with a cross-reference table.
fn pdf_of(objs: &[Vec<u8>]) -> Vec<u8> {
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

fn stream_obj(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    o.extend_from_slice(data);
    o.extend_from_slice(b"\nendstream");
    o
}

fn zlib1(data: &[u8]) -> Vec<u8> {
    zlib_stream(data, Params { level: 1, window_bits: 15, mem_level: 8, filtered: false })
}

#[test]
fn pack_codes_plainly_only_data_without_structure() {
    // A 512 x 512 RGB gradient: every byte value equally often, as in compressed data, but smooth.
    let (w, h) = (512usize, 512usize);
    let px: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [(x / 2) as u8, (y / 2) as u8, ((x + y) / 4) as u8])).collect();
    let pdf = pdf_of(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /I0 5 0 R >> >> >>".to_vec(),
        stream_obj("/Filter /FlateDecode", &zlib(b"q 512 0 0 512 50 100 cm /I0 Do Q")),
        stream_obj(&format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode"), &zlib(&px)),
    ]);
    let (best, _) = pack_with(&pdf, &PackOptions { effort: 100, threads: 1 });
    for effort in [0u8, 49] {
        let (fast, st) = pack_with(&pdf, &PackOptions { effort, threads: 1 });
        assert_eq!(unpack(&fast).unwrap(), pdf);
        assert_eq!(st.flat_bytes, 0, "effort {effort}: {st:?}");
        assert!(fast.len() <= best.len() + best.len() / 20, "effort {effort}: {} against {}", fast.len(), best.len());
    }
}

#[test]
fn a_short_image_is_kept_exactly() {
    // Images whose data decodes to fewer samples than their size needs (none, and half the rows) next to a
    // whole one and some text. Readers differ on short images, so those two must stay exactly as they are.
    let image = "/Type /XObject /Subtype /Image /Width 100 /Height 100 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode";
    let px: Vec<u8> = (0..100 * 100).map(|i| ((i % 100) * 2 + i / 100) as u8).collect();
    let text: Vec<u8> = (0..300).flat_map(|i| format!("q 50 0 0 50 {} 10 cm /A Do Q BT (Line {i}) Tj ET\n", i % 500).into_bytes()).collect();
    let pdf = pdf_of(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /A 5 0 R /B 6 0 R /C 7 0 R >> >> >>".to_vec(),
        stream_obj("/Filter /FlateDecode", &zlib1(&text)),
        stream_obj(image, &zlib1(b"")),
        stream_obj(image, &zlib1(&px[..5000])),
        stream_obj(image, &zlib1(&px)),
    ]);
    let before = engine::pdf::xref::open(&pdf).unwrap();
    for effort in [0u8, 100] {
        let r = optimize(&pdf, &opts(effort, 2));
        assert!(matches!(r.outcome, Outcome::Smaller(_)), "effort {effort}: {:?}", r.outcome);
        let after = engine::pdf::xref::open(&r.data).unwrap();
        for n in 4..=7u32 {
            let (engine::pdf::object::Obj::Stream(a), engine::pdf::object::Obj::Stream(b)) = (before.load(n).unwrap().1, after.load(n).unwrap().1) else { panic!("stream {n}") };
            let kept = a.data == b.data && a.dict.get(b"Filter") == b.dict.get(b"Filter");
            assert_eq!(kept, n == 5 || n == 6, "effort {effort}, object {n}");
        }
    }
}

#[test]
fn low_effort_work_follows_the_file_size() {
    // A 43 KB file with a blank 3000 x 3000 image (9 MB decoded) and some text. At low effort the image is
    // beyond the quick budget (16 decoded bytes per byte of the file at effort 0, twice as much every 10
    // points, at least 4 MiB: 5.5 MB at effort 30), so it is kept exactly as it is; from effort 40 (11 MB) it
    // is re-encoded. The text is re-encoded at every effort.
    let image = "/Type /XObject /Subtype /Image /Width 3000 /Height 3000 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode";
    let text: Vec<u8> = (0..300).flat_map(|i| format!("q 50 0 0 50 {} 10 cm /A Do Q BT (Line {i}) Tj ET\n", i % 500).into_bytes()).collect();
    let pdf = pdf_of(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /A 5 0 R >> >> >>".to_vec(),
        stream_obj("/Filter /FlateDecode", &zlib1(&text)),
        stream_obj(image, &zlib1(&vec![255u8; 3000 * 3000])),
    ]);
    assert!((40_000..46_000).contains(&pdf.len()), "{}", pdf.len());
    let before = engine::pdf::xref::open(&pdf).unwrap();
    for (effort, image_kept) in [(0u8, true), (30, true), (40, false), (100, false)] {
        let r = optimize(&pdf, &opts(effort, 2));
        assert!(matches!(r.outcome, Outcome::Smaller(_)), "effort {effort}: {:?}", r.outcome);
        let after = engine::pdf::xref::open(&r.data).unwrap();
        for (n, kept) in [(4u32, false), (5, image_kept)] {
            let (engine::pdf::object::Obj::Stream(a), engine::pdf::object::Obj::Stream(b)) = (before.load(n).unwrap().1, after.load(n).unwrap().1) else { panic!("stream {n}") };
            assert_eq!(a.data == b.data, kept, "effort {effort}, object {n}");
        }
    }
}
