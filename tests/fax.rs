//! Fax images in both outputs, on small PDFs made here: JBIG2 in normal PDFs only when nothing is uncertain
//! (same pixels, every other case left alone), and the peyaji format restoring every CCITT layout exactly.

use engine::fax::ccitt::{self, row_changes, Gap, Image, Layout, Rule, Scheme};
use engine::fax::jbig2;
use engine::pack::{pack, unpack};
use engine::pdf::object::Obj;
use engine::pdf::rewrite::{optimize, Options, Outcome};
use engine::pdf::xref;

/// A page of "text": rows of letter-sized blobs on white, like a scan.
fn page(w: usize, h: usize) -> Image {
    let mut s = 0x1234_5678u64;
    let mut rng = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut px = vec![vec![0u8; w]; h];
    let mut y = 10;
    while y + 14 < h {
        let mut x = 8;
        while x + 9 < w {
            if rng() % 6 != 0 {
                let (bw, bh) = (3 + (rng() % 5) as usize, 6 + (rng() % 6) as usize);
                for yy in y..y + bh {
                    for xx in x..x + bw {
                        if rng() % 9 != 0 {
                            px[yy][xx] = 1;
                        }
                    }
                }
            }
            x += 9;
        }
        y += 18;
    }
    let mut img = Image::new(w);
    let mut ch = Vec::new();
    for r in &px {
        row_changes(r, &mut ch);
        img.push_row(&ch);
    }
    img
}

fn layout(img: &Image, scheme: Scheme, eol: bool) -> Layout {
    let gaps = (0..img.rows()).map(|r| Gap { fill: 0, eol, tag: scheme == Scheme::G32D && r % 2 == 0 }).collect();
    let end = vec![Gap { fill: 0, eol: true, tag: scheme == Scheme::G32D }; if scheme == Scheme::G4 { 2 } else { 6 }];
    Layout { scheme, rule: Rule::Standard, gaps, end, pad: 0, trailing: Vec::new() }
}

/// A one-page PDF drawing one image XObject (object 4) with the given dictionary entries and data.
fn pdf(image_dict: &str, data: &[u8]) -> Vec<u8> {
    let content = b"q 300 0 0 200 50 50 cm /Im0 Do Q\n";
    let mut out = b"%PDF-1.3\n".to_vec();
    let mut offs = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offs.push(out.len());
        out.extend_from_slice(body);
    };
    obj(&mut out, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    obj(&mut out, b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    obj(&mut out, b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>\nendobj\n");
    let mut img = format!("4 0 obj\n<< /Type /XObject /Subtype /Image {image_dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    img.extend_from_slice(data);
    img.extend_from_slice(b"\nendstream\nendobj\n");
    obj(&mut out, &img);
    let mut c = format!("5 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes();
    c.extend_from_slice(content);
    c.extend_from_slice(b"\nendstream\nendobj\n");
    obj(&mut out, &c);
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for o in &offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

fn fax_pdf(img: &Image, scheme: Scheme, extra: &str, parms: &str) -> Vec<u8> {
    let k = match scheme {
        Scheme::G4 => -1,
        Scheme::G31D => 0,
        Scheme::G32D => 2,
    };
    let data = ccitt::encode(img, &layout(img, scheme, scheme != Scheme::G4));
    pdf(
        &format!("/Width {} /Height {} /BitsPerComponent 1 {extra} /Filter /CCITTFaxDecode /DecodeParms << /K {k} /Columns {} {parms} >>", img.width, img.rows(), img.width),
        &data,
    )
}

/// The image stream (object 4) of a PDF.
fn image_stream(d: &[u8]) -> engine::pdf::object::Stream {
    let doc = xref::open(d).unwrap();
    match doc.load(4).unwrap().1 {
        Obj::Stream(s) => *s,
        _ => panic!("object 4 is not a stream"),
    }
}

fn optimized(d: &[u8]) -> Vec<u8> {
    let opt = Options { flate: engine::flate::Options::quick(), ..Default::default() };
    let r = optimize(d, &opt);
    assert!(matches!(r.outcome, Outcome::Smaller(_)), "{:?}", r.outcome);
    r.data
}

/// Inverts every pixel of an image.
fn inverted(img: &Image) -> Image {
    let mut out = Image::new(img.width);
    for r in 0..img.rows() {
        let row = img.row(r);
        let ch: Vec<u32> = if row.first() == Some(&0) { row[1..].to_vec() } else { std::iter::once(0).chain(row.iter().copied()).collect() };
        out.push_row(&ch);
    }
    out
}

#[test]
fn normal_pdf_gets_jbig2_with_the_same_filter_output() {
    let img = page(600, 400);
    // BlackIs1 false: CCITTFaxDecode gives 0 for black, JBIG2Decode 1 for white: the JBIG2 bitmap is the image.
    // BlackIs1 true: CCITTFaxDecode gives 1 for black: the JBIG2 bitmap is the inverse.
    for (extra, parms, black_is_1) in [
        ("/ColorSpace /DeviceGray", "", false),
        ("/ColorSpace /DeviceGray", "/BlackIs1 true", true),
        ("/ImageMask true /Decode [1 0]", "", false),
        ("/ImageMask true", "/BlackIs1 true /Rows 400", true),
    ] {
        for scheme in [Scheme::G4, Scheme::G31D, Scheme::G32D] {
            let src = fax_pdf(&img, scheme, extra, parms);
            let out = optimized(&src);
            assert!(out.starts_with(b"%PDF-1.") && out[7] >= b'4', "JBIG2Decode needs PDF 1.4");
            let s = image_stream(&out);
            assert_eq!(s.dict.name(b"Filter"), Some(&b"JBIG2Decode"[..]), "{extra} {parms} {scheme:?}");
            assert!(!s.dict.has(b"DecodeParms"));
            // Everything else in the dictionary is as it was.
            let before = image_stream(&src).dict;
            for (k, v) in &before.0 {
                if !matches!(k.as_slice(), b"Filter" | b"DecodeParms" | b"Length") {
                    assert_eq!(s.dict.get(k), Some(v), "{}", String::from_utf8_lossy(k));
                }
            }
            let j = jbig2::read_pdf_stream(&s.data, false, usize::MAX).unwrap();
            assert_eq!(j, if black_is_1 { inverted(&img) } else { img.clone() }, "{extra} {parms} {scheme:?}");
        }
    }
}

#[test]
fn anything_uncertain_stays_ccitt() {
    let img = page(600, 400);
    let g4 = ccitt::encode(&img, &layout(&img, Scheme::G4, false));
    let dict = |h: usize, parms: &str| format!("/Width 600 /Height {h} /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 600 {parms} >>");
    // Zero bytes in the middle: the rows after them can't be decoded. (A single flipped bit is not always
    // damage: it can turn one valid code into another, a different image every reader decodes the same way.)
    let mut damaged = g4.clone();
    let mid = damaged.len() / 2;
    damaged[mid..mid + 8].fill(0);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("height differs from the rows coded", pdf(&dict(401, ""), &g4)),
        ("Rows differs from Height", pdf(&dict(400, "/Rows 399"), &g4)),
        ("byte-aligned rows", pdf(&dict(400, "/EncodedByteAlign true"), &g4)),
        ("damaged data", pdf(&dict(400, ""), &damaged)),
        ("data after the image without end codes", pdf(&dict(400, ""), &[&g4[..g4.len() - 3], b"\xff\xff\xff\xff"].concat())),
        ("a colour space other than DeviceGray", pdf("/Width 600 /Height 400 /BitsPerComponent 1 /ColorSpace [/Indexed /DeviceRGB 1 <000000FFFFFF>] /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 600 >>", &g4)),
        ("Columns differs from Width", pdf("/Width 601 /Height 400 /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 600 >>", &g4)),
        ("no end of block and no Rows", pdf(&dict(400, "/EndOfBlock false"), &g4)),
    ];
    for (what, d) in cases {
        let opt = Options { flate: engine::flate::Options::quick(), ..Default::default() };
        let r = optimize(&d, &opt);
        let out = r.data;
        let s = image_stream(&out);
        let f = format!("{:?}", engine::pdf::filters::chain(&s.dict).unwrap().iter().map(|st| String::from_utf8_lossy(&st.name).to_string()).collect::<Vec<_>>());
        assert!(!f.contains("JBIG2"), "{what}: {f}");
        assert!(f.contains("CCITTFaxDecode"), "{what}: {f}");
    }
    // A tiny image: JBIG2's fixed headers make it no smaller, so it stays CCITT.
    let small = page(40, 20);
    let opt = Options { flate: engine::flate::Options::quick(), ..Default::default() };
    let out = optimize(&fax_pdf(&small, Scheme::G4, "/ColorSpace /DeviceGray", ""), &opt).data;
    let names: Vec<Vec<u8>> = engine::pdf::filters::chain(&image_stream(&out).dict).unwrap().into_iter().map(|st| st.name).collect();
    assert!(names.contains(&b"CCITTFaxDecode".to_vec()) && !names.contains(&b"JBIG2Decode".to_vec()));
}

#[test]
fn self_check_compares_fax_and_jbig2_by_pixels() {
    let img = page(600, 400);
    let id = |o: &Obj| Some(o.clone());
    for black_is_1 in [false, true] {
        let parms = if black_is_1 { "/BlackIs1 true" } else { "" };
        let a = image_stream(&fax_pdf(&img, Scheme::G4, "/ColorSpace /DeviceGray", parms));
        let as_jbig2 = |pixels: &Image| {
            let mut b = a.clone();
            b.dict.set(b"Filter", Obj::Name(b"JBIG2Decode".to_vec()));
            b.dict.remove(b"DecodeParms");
            b.data = jbig2::pdf_stream(pixels, black_is_1);
            b
        };
        assert!(engine::fax::pdf::same_image(&a, &as_jbig2(&img), &id));
        // One pixel flipped (the first pixel of row 200), or the other polarity: caught.
        let mut flipped = Image::new(img.width);
        for r in 0..img.rows() {
            let mut px = vec![0u8; img.width];
            ccitt::expand_row(img.row(r), &mut px);
            if r == 200 {
                px[0] ^= 1;
            }
            let mut ch = Vec::new();
            row_changes(&px, &mut ch);
            flipped.push_row(&ch);
        }
        assert!(!engine::fax::pdf::same_image(&a, &as_jbig2(&flipped), &id));
        let mut wrong_polarity = as_jbig2(&img);
        wrong_polarity.data = jbig2::pdf_stream(&img, !black_is_1);
        assert!(!engine::fax::pdf::same_image(&a, &wrong_polarity, &id));
    }
}

#[test]
fn peyaji_restores_every_fax_layout_exactly() {
    let img = page(500, 300);
    for scheme in [Scheme::G4, Scheme::G31D, Scheme::G32D] {
        for adobe in [false, true] {
            let mut lay = layout(&img, scheme, scheme != Scheme::G4);
            if adobe {
                lay.rule = Rule::Adobe;
            }
            // Byte-aligned EOLs with fill bits, and a CR after the end codes.
            if scheme != Scheme::G4 {
                for (i, g) in lay.gaps.iter_mut().enumerate() {
                    g.fill = (i % 8) as u32;
                }
            }
            lay.trailing = b"\r".to_vec();
            let data = ccitt::encode(&img, &lay);
            let k = match scheme {
                Scheme::G4 => -1,
                Scheme::G31D => 0,
                Scheme::G32D => 2,
            };
            let d = pdf(&format!("/Width 500 /Height 300 /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode /DecodeParms << /K {k} /Columns 500 >>"), &data);
            let (packed, stats) = pack(&d);
            assert_eq!(stats.fax, 1, "{scheme:?} {stats:?}");
            assert!(packed.len() * 10 < d.len() * 9, "{scheme:?}: {} -> {}", d.len(), packed.len());
            assert_eq!(unpack(&packed).unwrap(), d, "{scheme:?} adobe={adobe}");
        }
    }
}

#[test]
fn peyaji_refuses_damaged_fax_parts() {
    let img = page(300, 200);
    let d = fax_pdf(&img, Scheme::G4, "/ColorSpace /DeviceGray", "");
    let (packed, stats) = pack(&d);
    assert_eq!(stats.fax, 1);
    let mut s = 99u64;
    for _ in 0..400 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let mut b = packed.clone();
        let i = 25 + (s as usize) % (b.len() - 25);
        b[i] ^= 1 << (s >> 32) % 8;
        match unpack(&b) {
            Ok(out) => assert_eq!(out, d, "a damaged file restored to something else"),
            Err(_) => {}
        }
    }
    for cut in (0..packed.len()).step_by(7) {
        if let Ok(out) = unpack(&packed[..cut]) {
            assert_eq!(out, d);
        }
    }
}
