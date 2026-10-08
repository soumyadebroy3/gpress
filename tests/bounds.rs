//! Crafted inputs: what a file claims (an object stream's /N, the trailer's /Size, an image's /Width, predictor
//! parameters, an archive's declared length) never sizes memory or work on its own. Each case is refused, or
//! handled from what the file holds, in well under a second, with no panic and no allocation that could fail.

use gpress::flate::zlibexact::{zlib_stream, Params};
use gpress::pack::{pack, pack_with, unpack, unpack_with, PackOptions, UnpackOptions, MAGIC};
use gpress::pdf::object::Obj;
use gpress::pdf::rewrite::{optimize, Options, Outcome};
use gpress::pdf::xref;
use std::time::Instant;

fn z(d: &[u8]) -> Vec<u8> {
    zlib_stream(d, Params { level: 6, window_bits: 15, mem_level: 8, filtered: false })
}

/// A classic PDF from numbered objects (bodies without "obj"/"endobj"), one xref subsection per object so the
/// numbers can be anything, and the trailer /Size given.
fn classic(objs: &[(u64, Vec<u8>)], size: &str) -> Vec<u8> {
    let mut d = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (n, body) in objs {
        offs.push(d.len());
        d.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        d.extend_from_slice(body);
        d.extend_from_slice(b"\nendobj\n");
    }
    let x = d.len();
    d.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    for ((n, _), off) in objs.iter().zip(offs) {
        d.extend_from_slice(format!("{n} 1\n{off:010} 00000 n \n").as_bytes());
    }
    d.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes());
    d
}

/// A page with a content stream, as in the review's files.
fn page_objects() -> Vec<(u64, Vec<u8>)> {
    let content = b"BT /F1 12 Tf 10 10 Td (hi) Tj ET\n".repeat(50);
    vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec()),
        (4, stream("", &content)),
    ]
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut b = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    b.extend_from_slice(data);
    b.extend_from_slice(b"\nendstream");
    b
}

/// The same page with the catalog and the pages object inside an object stream (PDF 1.5, cross-reference
/// stream), with the object stream's /N and /First as given.
fn with_object_stream(n: &str, first: &str) -> Vec<u8> {
    let inner = b"<< /Type /Catalog /Pages 2 0 R >>\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
    let head = format!("1 0 2 {} ", inner.iter().position(|&c| c == b'\n').unwrap() + 1);
    let first_real = head.len();
    let first = if first == "real" { first_real.to_string() } else { first.to_string() };
    let mut raw = head.into_bytes();
    raw.extend_from_slice(inner);
    let content = b"BT /F1 12 Tf 10 10 Td (hi) Tj ET\n".repeat(50);
    let objs: Vec<(u64, Vec<u8>)> = vec![
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec()),
        (4, stream("", &content)),
        (5, stream(&format!("/Type /ObjStm /N {n} /First {first}"), &raw)),
    ];
    let mut d = b"%PDF-1.5\n".to_vec();
    let mut offs = Vec::new();
    for (num, body) in &objs {
        offs.push((*num, d.len()));
        d.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        d.extend_from_slice(body);
        d.extend_from_slice(b"\nendobj\n");
    }
    let x = d.len();
    // Rows for objects 0 to 6: 1 and 2 in the object stream, 3 to 5 at their offsets, 6 the xref stream.
    let mut rows = vec![0u8, 0, 0, 0, 0, 0, 0];
    for (num, idx) in [(1u64, 0u8), (2, 1)] {
        let _ = num;
        rows.extend_from_slice(&[2, 0, 0, 0, 5, 0, idx]);
    }
    for (_, off) in &offs {
        rows.push(1);
        rows.extend_from_slice(&(*off as u32).to_be_bytes());
        rows.extend_from_slice(&[0, 0]);
    }
    rows.push(1);
    rows.extend_from_slice(&(x as u32).to_be_bytes());
    rows.extend_from_slice(&[0, 0]);
    d.extend_from_slice(b"6 0 obj\n");
    d.extend_from_slice(&stream("/Type /XRef /Size 7 /W [1 4 2] /Root 1 0 R", &rows));
    d.extend_from_slice(format!("\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    d
}

fn quick(input: &[u8], opt: &Options) -> gpress::pdf::rewrite::Optimized {
    let t = Instant::now();
    let r = optimize(input, opt);
    assert!(t.elapsed().as_millis() < 1000, "{} bytes took {:?}", input.len(), t.elapsed());
    r
}

fn untouched(r: &gpress::pdf::rewrite::Optimized, input: &[u8]) -> String {
    assert_eq!(r.data, input);
    match &r.outcome {
        Outcome::Untouched(why) => why.clone(),
        Outcome::Smaller(s) => panic!("made smaller: {s:?}"),
    }
}

fn smaller(r: &gpress::pdf::rewrite::Optimized, input: &[u8]) {
    assert!(matches!(r.outcome, Outcome::Smaller(_)), "{:?}", r.outcome);
    assert!(r.data.len() < input.len());
    assert!(xref::open(&r.data).is_ok());
}

/// The archive of a file restores with both outputs' checks: packs are made and restored, whatever the file.
fn packs_and_restores(input: &[u8]) {
    let t = Instant::now();
    let (packed, _) = pack_with(input, &PackOptions { threads: 1, ..PackOptions::default() });
    assert_eq!(unpack(&packed).unwrap(), input);
    assert!(t.elapsed().as_millis() < 2000, "pack took {:?}", t.elapsed());
}

#[test]
fn object_stream_headers_are_checked_before_anything_is_sized_by_them() {
    // The honest object stream is read, and the file made smaller.
    let honest = with_object_stream("2", "real");
    smaller(&quick(&honest, &Options::effort(100)), &honest);
    // /N far beyond what the data could hold (the review's 100,000,000,000 tried to reserve 1.6 TB), negative,
    // at the edges of 64 bits; /First past the data or negative: the object stream is refused (nothing is
    // reachable, the file is untouched), at once.
    for (n, first) in [
        ("100000000000", "real"),
        ("-1", "real"),
        ("9223372036854775807", "real"),
        ("4611686018427387904", "real"),
        ("-5", "real"),
        ("2", "999999"),
        ("2", "-7"),
        ("2", "9223372036854775807"),
    ] {
        let bad = with_object_stream(n, first);
        let why = untouched(&quick(&bad, &Options::effort(100)), &bad);
        assert!(why.contains("unreadable object"), "/N {n} /First {first}: {why}");
        packs_and_restores(&bad);
    }
}

#[test]
fn the_trailer_size_sizes_nothing() {
    // Objects 1 to 4 with /Size 4,000,000,000 (the review's file tried to reserve 12 GB of cross-reference
    // rows), 2^32, 2^32 - 1, i64::MAX: the output's cross-reference stream covers the objects written, so the
    // file is made smaller, in milliseconds, with a small /Size of its own.
    for size in ["4000000000", "4294967296", "4294967295", "9223372036854775807", "5"] {
        let pdf = classic(&page_objects(), size);
        let r = quick(&pdf, &Options::effort(100));
        smaller(&r, &pdf);
        let out = xref::open(&r.data).unwrap();
        assert_eq!(out.trailer.get(b"Size"), Some(&Obj::Int(7)), "/Size {size}");
        packs_and_restores(&pdf);
    }
    // Objects at or beyond /Size are still refused (readers disagree on whether they exist).
    let pdf = classic(&page_objects(), "4");
    assert_eq!(untouched(&quick(&pdf, &Options::effort(100)), &pdf), "object numbers beyond /Size");
}

#[test]
fn a_dangling_reference_keeps_its_number_free() {
    // The catalog refers to an outline object that doesn't exist (a null reference, as in a Kodak test file
    // where /Outlines /First points past the last object). New objects are numbered above every number the
    // document refers to, so the reference stays null in the output instead of pointing at the new object
    // stream (which the self-check would refuse).
    let mut objs = page_objects();
    objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Outlines << /Type /Outlines /First 9 0 R /Count 0 >> >>".to_vec();
    let pdf = classic(&objs, "10");
    let r = quick(&pdf, &Options::effort(100));
    smaller(&r, &pdf);
    let out = xref::open(&r.data).unwrap();
    assert!(out.load(9).is_err(), "object 9 must stay missing");
    let in_use: Vec<u32> =
        out.entries.iter().filter(|(_, e)| !matches!(e, xref::Entry::Free)).map(|(&n, _)| n).collect();
    assert!(in_use.iter().all(|&n| n <= 4 || n >= 10), "{in_use:?}");
    // A dangling reference far away counts like an object there: the file is left as it is.
    objs[0].1 =
        b"<< /Type /Catalog /Pages 2 0 R /Outlines << /Type /Outlines /First 4000000000 0 R /Count 0 >> >>".to_vec();
    let pdf = classic(&objs, "4000000001");
    assert_eq!(untouched(&quick(&pdf, &Options::effort(100)), &pdf), "object numbers far apart");
}

/// The page with its content stream numbered `n` and the trailer /Size just above it.
fn page_with_content_at(n: u64) -> Vec<u8> {
    let mut objs = page_objects();
    objs[3].0 = n;
    objs[2].1 = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {n} 0 R >>").into_bytes();
    classic(&objs, &(n + 1).to_string())
}

#[test]
fn numbering_far_apart_is_left_untouched() {
    // The output's cross-reference stream has a row for every number up to the highest one used, and a file
    // may have at most 16 rows per byte plus 65536 (the reader's bound too). One object numbered
    // 4,000,000,000, or 120,000 in a 2 KB file: left as it is, at once.
    for n in [4_000_000_000u64, 120_000] {
        let pdf = page_with_content_at(n);
        assert!(pdf.len() < 4096);
        assert_eq!(untouched(&quick(&pdf, &Options::effort(100)), &pdf), "object numbers far apart", "{n}");
        packs_and_restores(&pdf);
    }
    // Numbers thousands apart are what some writers leave behind (pdf.js's reduced test files keep a few
    // objects with their original numbers): still made smaller, with the rows that takes (60,000 rows cost
    // about a second of Flate at effort 100).
    for n in [4000u64, 60_000] {
        let pdf = page_with_content_at(n);
        let r = optimize(&pdf, &Options::effort(100));
        smaller(&r, &pdf);
        assert_eq!(xref::open(&r.data).unwrap().trailer.get(b"Size"), Some(&Obj::Int(n as i64 + 3)), "{n}");
    }
}

/// The page with a cross-reference stream of `rows` rows (the page's objects, then free rows): a few bytes
/// of Flate for each thousand rows.
fn with_free_rows(rows: u32) -> Vec<u8> {
    let objs = page_objects();
    let mut d = b"%PDF-1.5\n".to_vec();
    let mut offs = Vec::new();
    for (num, body) in &objs {
        offs.push((*num, d.len()));
        d.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        d.extend_from_slice(body);
        d.extend_from_slice(b"\nendobj\n");
    }
    let x = d.len();
    let mut rec = vec![0u8; 5];
    for (_, off) in &offs {
        rec.push(1);
        rec.extend_from_slice(&(*off as u32).to_be_bytes());
    }
    rec.push(1);
    rec.extend_from_slice(&(x as u32).to_be_bytes());
    rec.resize(rows as usize * 5, 0);
    d.extend_from_slice(b"5 0 obj\n");
    d.extend_from_slice(&stream(
        &format!("/Type /XRef /Size {rows} /W [1 4 0] /Root 1 0 R /Filter /FlateDecode"),
        &z(&rec),
    ));
    d.extend_from_slice(format!("\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    d
}

#[test]
fn cross_reference_rows_are_bounded_by_the_file_size() {
    // 2 million free rows from 10 KB of Flate (20 million took the reader 1.3 GB and 8 CPU seconds):
    // refused as the rows are read, before anything holds them. 50,000 rows (within the 65,536 every file
    // may have): read, and the file made smaller.
    let bomb = with_free_rows(2_000_000);
    assert!(bomb.len() < 20_000, "{}", bomb.len());
    let why = untouched(&quick(&bomb, &Options::effort(100)), &bomb);
    assert!(why.contains("rows than the file can hold"), "{why}");
    assert!(xref::open(&bomb).is_err());
    // The pack doesn't read cross-reference data; it stores the stream's 10 MB decoded (a few seconds).
    let (packed, _) = pack_with(&bomb, &PackOptions { threads: 1, ..PackOptions::default() });
    assert_eq!(unpack(&packed).unwrap(), bomb);
    let many = with_free_rows(50_000);
    smaller(&quick(&many, &Options::effort(100)), &many);
}

#[test]
fn an_image_width_that_wraps_around_is_refused() {
    // 8-bit gray with /Width 2^61: colours x bits x width wraps to 0 in 64 bits (the review's file divided by
    // it), with and without a TIFF predictor naming the same /Columns. No predictor is tried for such an
    // image; whatever else happens to the file (the predictor one fails the self-check and stays as it is),
    // the image's samples read back the same.
    let px: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
    for parms in ["", " /DecodeParms << /Predictor 2 /Colors 1 /BitsPerComponent 8 /Columns 2305843009213693952 >>"] {
        let mut objs = page_objects();
        objs[2].1 = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>".to_vec();
        objs.push((5, stream(&format!("/Type /XObject /Subtype /Image /Width 2305843009213693952 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode{parms}"), &z(&px))));
        let pdf = classic(&objs, "6");
        for effort in [0u8, 100] {
            let r = quick(&pdf, &Options { threads: 2, ..Options::effort(effort) });
            if parms.is_empty() {
                smaller(&r, &pdf);
            }
            let out = xref::open(&r.data).unwrap();
            match out.load(5).unwrap().1 {
                Obj::Stream(s) => {
                    assert_eq!(s.dict.get(b"DecodeParms").is_some(), !parms.is_empty());
                    assert_eq!(
                        gpress::pdf::filters::decode_stage(
                            &gpress::pdf::filters::chain(&s.dict).unwrap()[0],
                            &s.data,
                            1 << 20
                        )
                        .unwrap(),
                        px
                    );
                }
                o => panic!("{o:?}"),
            }
        }
        packs_and_restores(&pdf);
    }
}

#[test]
fn predictor_rows_are_cut_to_the_data() {
    // A content stream whose predictor parameters describe a 1 GB row: the row is only as long as the data,
    // so decoding it (the self-check undoes predictors) reserves nothing for the claim.
    let content = b"BT /F1 12 Tf 10 10 Td (hi) Tj ET\n".repeat(50);
    let mut objs = page_objects();
    objs[3].1 = stream(
        "/Filter /FlateDecode /DecodeParms << /Predictor 12 /Colors 32 /BitsPerComponent 16 /Columns 16777216 >>",
        &z(&content),
    );
    let pdf = classic(&objs, "5");
    let r = quick(&pdf, &Options::effort(100));
    assert!(matches!(r.outcome, Outcome::Smaller(_) | Outcome::Untouched(_)));
    packs_and_restores(&pdf);
}

#[test]
fn the_decoder_refuses_a_declared_length_over_its_limit() {
    // An honest archive of a 60 KB file: refused below its length, restored at it.
    let big: Vec<u8> = (0..60_000u32).map(|i| (i % 251) as u8).collect();
    let mut objs = page_objects();
    objs.push((5, stream("", &big)));
    let pdf = classic(&objs, "6");
    let (packed, _) = pack(&pdf);
    let err = unpack_with(&packed, &UnpackOptions { max_len: 50_000 }).unwrap_err();
    assert!(err.contains("limit"), "{err}");
    assert_eq!(unpack_with(&packed, &UnpackOptions { max_len: pdf.len() as u64 }).unwrap(), pdf);
    assert_eq!(unpack(&packed).unwrap(), pdf);
    // A crafted header declaring 1 GiB over 16 KB of zeros (the review's file grew past 800 MB before failing):
    // refused at once under a limit, before anything is decoded.
    let mut crafted = MAGIC.to_vec();
    crafted.push(0);
    let mut n: u64 = 1 << 30;
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            crafted.push(b);
            break;
        }
        crafted.push(b | 0x80);
    }
    crafted.extend_from_slice(&[0u8; 16]);
    crafted.extend_from_slice(&[0u8; 16384]);
    for limit in [1u64 << 20, (1 << 30) - 1] {
        let t = Instant::now();
        let err = unpack_with(&crafted, &UnpackOptions { max_len: limit }).unwrap_err();
        assert!(err.contains("limit"), "{err}");
        assert!(t.elapsed().as_millis() < 50, "{:?}", t.elapsed());
    }
    // The default limit is the format's own.
    assert_eq!(UnpackOptions::default().max_len, 1 << 34);
}
