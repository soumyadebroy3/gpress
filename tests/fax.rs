//! Fax images in both outputs, on small PDFs made here: JBIG2 in normal PDFs only when nothing is uncertain
//! (same pixels, every other case left alone), and the gpress archive restoring every CCITT layout exactly.

// Only in a build with the fax feature (Cargo.toml, [features]).
#![cfg(feature = "fax")]

use gpress::fax::ccitt::{self, row_changes, Gap, Image, Layout, Rule, Scheme};
use gpress::fax::jbig2;
#[cfg(feature = "archive")]
use gpress::pack::{pack, unpack};
use gpress::pdf::object::Obj;
use gpress::pdf::rewrite::{optimize, Options, Outcome};
use gpress::pdf::xref;

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
                for row in px.iter_mut().skip(y).take(bh) {
                    for p in row.iter_mut().skip(x).take(bw) {
                        if rng() % 9 != 0 {
                            *p = 1;
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
    let mut img =
        format!("4 0 obj\n<< /Type /XObject /Subtype /Image {image_dict} /Length {} >>\nstream\n", data.len())
            .into_bytes();
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
fn image_stream(d: &[u8]) -> gpress::pdf::object::Stream {
    let doc = xref::open(d).unwrap();
    match doc.load(4).unwrap().1 {
        Obj::Stream(s) => *s,
        _ => panic!("object 4 is not a stream"),
    }
}

fn optimized(d: &[u8]) -> Vec<u8> {
    let opt = Options { flate: gpress::flate::Options::quick(), ..Default::default() };
    let r = optimize(d, &opt);
    assert!(matches!(r.outcome, Outcome::Smaller(_)), "{:?}", r.outcome);
    r.data
}

/// Inverts every pixel of an image.
fn inverted(img: &Image) -> Image {
    let mut out = Image::new(img.width);
    for r in 0..img.rows() {
        let row = img.row(r);
        let ch: Vec<u32> = if row.first() == Some(&0) {
            row[1..].to_vec()
        } else {
            std::iter::once(0).chain(row.iter().copied()).collect()
        };
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
    let dict = |h: usize, parms: &str| {
        format!("/Width 600 /Height {h} /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 600 {parms} >>")
    };
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
        let opt = Options { flate: gpress::flate::Options::quick(), ..Default::default() };
        let r = optimize(&d, &opt);
        let out = r.data;
        let s = image_stream(&out);
        let f = format!(
            "{:?}",
            gpress::pdf::filters::chain(&s.dict)
                .unwrap()
                .iter()
                .map(|st| String::from_utf8_lossy(&st.name).to_string())
                .collect::<Vec<_>>()
        );
        assert!(!f.contains("JBIG2"), "{what}: {f}");
        assert!(f.contains("CCITTFaxDecode"), "{what}: {f}");
    }
    // A tiny image: JBIG2's fixed headers make it no smaller, so it stays CCITT.
    let small = page(40, 20);
    let opt = Options { flate: gpress::flate::Options::quick(), ..Default::default() };
    let out = optimize(&fax_pdf(&small, Scheme::G4, "/ColorSpace /DeviceGray", ""), &opt).data;
    let names: Vec<Vec<u8>> =
        gpress::pdf::filters::chain(&image_stream(&out).dict).unwrap().into_iter().map(|st| st.name).collect();
    assert!(names.contains(&b"CCITTFaxDecode".to_vec()) && !names.contains(&b"JBIG2Decode".to_vec()));
}

/// What normal-PDF output does with the image of a PDF made by `pdf` (object 4), in words.
fn explain(d: &[u8]) -> String {
    let id = |o: &Obj| Some(o.clone());
    gpress::fax::pdf::explain(&image_stream(d), &id)
}

/// Data that readers see differently stays CCITT: Poppler and xpdf look for an EOL after every row when
/// EndOfLine is set or when the data starts with an EOL (PDFium and libtiff don't), and huge JBIG2 pages are
/// beyond PDFium's decoder (it shows nothing where it shows the CCITT image).
#[test]
fn what_readers_see_differently_stays_ccitt() {
    let img = page(600, 400);
    let gray = "/ColorSpace /DeviceGray";
    let with = |scheme: Scheme, first_eol_only: bool, parms: &str| {
        let mut lay = layout(&img, scheme, false);
        lay.gaps[0].eol = first_eol_only;
        let k = if scheme == Scheme::G31D {
            0
        } else if scheme == Scheme::G32D {
            2
        } else {
            -1
        };
        pdf(&format!("/Width 600 /Height 400 /BitsPerComponent 1 {gray} /Filter /CCITTFaxDecode /DecodeParms << /K {k} /Columns 600 {parms} >>"), &ccitt::encode(&img, &lay))
    };
    let cases = [
        (
            "Group 4 data with EndOfLine",
            with(Scheme::G4, false, "/EndOfLine true"),
            "kept: EndOfLine set for Group 4 data",
        ),
        (
            "K 0, an EOL before the first row only",
            with(Scheme::G31D, true, ""),
            "kept: a row without EOL after EndOfLine or a leading EOL",
        ),
        (
            "K 2, an EOL before the first row only",
            with(Scheme::G32D, true, ""),
            "kept: a row without EOL after EndOfLine or a leading EOL",
        ),
        (
            "K 0, EndOfLine but no EOLs",
            with(Scheme::G31D, false, "/EndOfLine true"),
            "kept: a row without EOL after EndOfLine or a leading EOL",
        ),
    ];
    for (what, d, why) in &cases {
        assert_eq!(explain(d), *why, "{what}");
        assert!(stays_ccitt(d), "{what}");
    }
    // The same data without the EOL before the first row, or without EndOfLine, is clear to every reader.
    for (scheme, parms) in
        [(Scheme::G4, ""), (Scheme::G31D, ""), (Scheme::G32D, ""), (Scheme::G31D, "/EndOfLine false")]
    {
        assert!(explain(&with(scheme, false, parms)).starts_with("JBIG2"), "{scheme:?} {parms}");
    }

    // An all-black image, its width rounded up to 32 times its height: at most 2^30 pixels become JBIG2.
    let black = |w: usize, h: usize| {
        let mut img = Image::new(w);
        for _ in 0..h {
            img.push_row(&[0]);
        }
        pdf(&format!("/Width {w} /Height {h} /BitsPerComponent 1 {gray} /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns {w} >>"), &ccitt::encode(&img, &layout(&img, Scheme::G4, false)))
    };
    assert!(explain(&black(32768, 32768)).starts_with("JBIG2"));
    assert!(explain(&black(32737, 32768)).starts_with("JBIG2"));
    assert_eq!(explain(&black(32768, 32769)), "kept: too large for some JBIG2 decoders");
    // Where PDFium's decoder shows nothing: 46368 x 46314.
    let d = black(46368, 46314);
    assert_eq!(explain(&d), "kept: too large for some JBIG2 decoders");
    assert!(stays_ccitt(&d));
}

/// Normal-PDF output of a PDF made by `pdf`: its image is still CCITT (Flate may be put in front), not JBIG2.
fn stays_ccitt(d: &[u8]) -> bool {
    let opt = Options { flate: gpress::flate::Options::quick(), ..Default::default() };
    let s = image_stream(&optimize(d, &opt).data);
    let names: Vec<Vec<u8>> = gpress::pdf::filters::chain(&s.dict).unwrap().into_iter().map(|st| st.name).collect();
    names.contains(&b"CCITTFaxDecode".to_vec()) && !names.contains(&b"JBIG2Decode".to_vec())
}

/// Rows of a "bomb": alternately white and with one black pixel, so that no row repeats the row above, at a few
/// bits per row.
#[cfg(feature = "archive")]
fn bomb(w: usize, h: usize, x: u32) -> Image {
    let mut img = Image::new(w);
    for r in 0..h {
        if r % 2 == 0 {
            img.push_row(&[x, x + 1]);
        } else {
            img.push_row(&[]);
        }
    }
    img
}

/// A few bytes of CCITT data can describe a huge image: the work of coding one is bounded by the size of its
/// data (normal PDF) or of the file (gpress archive), so that a file's size bounds its time.
#[test]
#[cfg(feature = "archive")]
fn fax_work_is_bounded_by_the_data() {
    // Normal PDF: 4096 rows of 131072 pixels from about 4.4 KB of Group 4 data stay CCITT.
    let img = bomb(131072, 4096, 10);
    let data = ccitt::encode(&img, &layout(&img, Scheme::G4, false));
    assert!(data.len() < 5000 && img.work() == 1 << 29, "{} bytes, work {}", data.len(), img.work());
    let d = pdf("/Width 131072 /Height 4096 /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 131072 >>", &data);
    assert_eq!(explain(&d), "kept: too much work for the size of the data");

    // gpress archive: the file's fax streams up to any point of it may take 2^24 plus 512 per byte of the file
    // up to there, in file order; the rest are coded plainly. Each of these images needs nearly 2^24, so only
    // the first fits.
    let mut objs = Vec::new();
    for i in 0..3u32 {
        let img = bomb(131072, 120, 10 + i);
        let data = ccitt::encode(&img, &layout(&img, Scheme::G4, false));
        objs.push(("/Width 131072 /Height 120 /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 131072 >>".to_string(), data));
    }
    let d = pdf_images(&objs);
    let (budget, cost) = (gpress::fax::part::budget(d.len()), gpress::fax::part::cost(&bomb(131072, 120, 10)));
    assert!(cost <= budget && budget < 2 * cost, "budget {budget}, cost {cost}");
    let (packed, stats) = pack(&d);
    assert_eq!(stats.fax, 1, "{stats:?}");
    assert_eq!(unpack(&packed).unwrap(), d);
    // With room for all three (more of the file before them), all three are coded as images.
    let per_byte = gpress::fax::part::budget(1) - gpress::fax::part::budget(0);
    let pad = (3 * cost / per_byte) as usize;
    let mut big = d.clone();
    big.splice(9..9, std::iter::repeat_n(b'%', pad).chain(*b"\n"));
    let big = fix_xref(&big);
    let (packed, stats) = pack(&big);
    assert_eq!(stats.fax, 3, "{stats:?}");
    assert_eq!(unpack(&packed).unwrap(), big);
}

/// A PDF made by `pdf_images` with its xref table rebuilt (after bytes were inserted before the objects).
#[cfg(feature = "archive")]
fn fix_xref(d: &[u8]) -> Vec<u8> {
    let at = |pat: &[u8], from: usize| d[from..].windows(pat.len()).position(|w| w == pat).map(|i| i + from);
    let xref = at(b"\nxref\n", 0).unwrap() + 1;
    let mut offs = Vec::new();
    let mut i = 0;
    while let Some(j) = at(b" 0 obj\n", i) {
        if j >= xref {
            break;
        }
        let mut s = j;
        while s > 0 && d[s - 1].is_ascii_digit() {
            s -= 1;
        }
        offs.push(s);
        i = j + 1;
    }
    let mut out = d[..xref].to_vec();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offs.len() + 1).as_bytes());
    for o in &offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offs.len() + 1).as_bytes(),
    );
    out
}

/// A PDF with one page drawing several image XObjects (objects 4, 5, ...; the content stream is the last).
#[cfg(feature = "archive")]
fn pdf_images(images: &[(String, Vec<u8>)]) -> Vec<u8> {
    let n = images.len();
    let mut content = Vec::new();
    for i in 0..n {
        content.extend_from_slice(format!("q 300 0 0 200 50 50 cm /Im{i} Do Q\n").as_bytes());
    }
    let xobjects: String = (0..n).map(|i| format!("/Im{i} {} 0 R ", 4 + i)).collect();
    let mut out = b"%PDF-1.3\n".to_vec();
    let mut offs = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offs.push(out.len());
        out.extend_from_slice(body);
    };
    obj(&mut out, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    obj(&mut out, b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    obj(&mut out, format!("3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Resources << /XObject << {xobjects}>> >> /Contents {} 0 R >>\nendobj\n", 4 + n).as_bytes());
    for (i, (dict, data)) in images.iter().enumerate() {
        let mut s =
            format!("{} 0 obj\n<< /Type /XObject /Subtype /Image {dict} /Length {} >>\nstream\n", 4 + i, data.len())
                .into_bytes();
        s.extend_from_slice(data);
        s.extend_from_slice(b"\nendstream\nendobj\n");
        obj(&mut out, &s);
    }
    let mut c = format!("{} 0 obj\n<< /Length {} >>\nstream\n", 4 + n, content.len()).into_bytes();
    c.extend_from_slice(&content);
    c.extend_from_slice(b"\nendstream\nendobj\n");
    obj(&mut out, &c);
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", 5 + n).as_bytes());
    for o in &offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", 5 + n).as_bytes());
    out
}

/// EncodedByteAlign data with the fill before each EOL, so that every EOL ends on a byte boundary (libtiff
/// writes it so): stored as an image like any other fax stream, and restored exactly.
#[test]
#[cfg(feature = "archive")]
fn archive_reads_eols_that_end_on_a_byte_boundary() {
    let img = page(613, 401);
    let mut lay = layout(&img, Scheme::G31D, true);
    let mut wr = ccitt::Writer::new();
    let mut enc = ccitt::RowEncoder::new(img.width, Scheme::G31D, Rule::Standard);
    for r in 0..img.rows() {
        lay.gaps[r].fill = ((16 - (wr.bit_pos() + 12) % 8) % 8) as u32;
        let g = lay.gaps[r];
        enc.row(&mut wr, &g, img.row(r));
    }
    let data = ccitt::encode(&img, &lay);
    // Read with the alignment the parameters give, an EOL that starts before a byte boundary is not found.
    let aligned = ccitt::parse(&data, Scheme::G31D, 613, true, ccitt::Limits::default()).unwrap();
    assert!(aligned.image.rows() < img.rows());
    let p = gpress::fax::part::prepare(&data, Scheme::G31D, 613, true).unwrap();
    assert_eq!(p.image, img);
    let d = pdf("/Width 613 /Height 401 /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode /DecodeParms << /K 0 /Columns 613 /EncodedByteAlign true /EndOfLine true >>", &data);
    let (packed, stats) = pack(&d);
    assert_eq!(stats.fax, 1, "{stats:?}");
    assert!(packed.len() * 2 < d.len(), "{} -> {}", d.len(), packed.len());
    assert_eq!(unpack(&packed).unwrap(), d);
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
        assert!(gpress::fax::pdf::same_image(&a, &as_jbig2(&img), &id));
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
        assert!(!gpress::fax::pdf::same_image(&a, &as_jbig2(&flipped), &id));
        let mut wrong_polarity = as_jbig2(&img);
        wrong_polarity.data = jbig2::pdf_stream(&img, !black_is_1);
        assert!(!gpress::fax::pdf::same_image(&a, &wrong_polarity, &id));
    }
}

#[test]
#[cfg(feature = "archive")]
fn archive_restores_every_fax_layout_exactly() {
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
#[cfg(feature = "archive")]
fn archive_refuses_damaged_fax_parts() {
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
        b[i] ^= 1 << ((s >> 32) % 8);
        if let Ok(out) = unpack(&b) {
            assert_eq!(out, d, "a damaged file restored to something else");
        }
    }
    for cut in (0..packed.len()).step_by(7) {
        if let Ok(out) = unpack(&packed[..cut]) {
            assert_eq!(out, d);
        }
    }
}
