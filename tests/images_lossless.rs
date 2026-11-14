//! The two exact image forms of the normal PDF, on small PDFs made here: a 1-bit image stored without an image
//! codec written as JBIG2, and an 8-bit image of few colours written as an Indexed image. Every pixel stays what
//! it was, the images that must not change don't, each form has its switch, and the thread count never shows.

use gpress::flate;
use gpress::pdf::filters::{chain, decode_stage, unpredict};
use gpress::pdf::object::{Dict, Obj, Stream};
use gpress::pdf::xref;
use gpress::{optimize, Optimized, Options, Outcome, Report};

/// Numbers below `k`, the same ones every run.
fn rng(seed: u64) -> impl FnMut(usize) -> usize {
    let mut r = seed;
    move |k| {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        (r % k as u64) as usize
    }
}

/// A stream object: `entries` and `/Length` around `data`.
fn stream_of(entries: &str, data: &[u8]) -> Vec<u8> {
    let mut o = format!("<< {entries} /Length {} >>\nstream\n", data.len()).into_bytes();
    o.extend_from_slice(data);
    o.extend_from_slice(b"\nendstream");
    o
}

/// A one-page PDF whose page draws these objects as image XObjects; they are numbered from 5.
fn page_of(images: &[Vec<u8>]) -> Vec<u8> {
    page_with("", images)
}

/// The same, with more entries in the page's resources.
fn page_with(resources: &str, images: &[Vec<u8>]) -> Vec<u8> {
    let names: String = (0..images.len()).map(|i| format!("/I{} {} 0 R ", i + 5, i + 5)).collect();
    let draw: String =
        (0..images.len()).map(|i| format!("q 90 0 0 60 {} 20 cm /I{} Do Q\n", 10 + 95 * i, i + 5)).collect();
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1600 100] /Contents 4 0 R /Resources << /XObject << {names}>>{resources} >> >>")
            .into_bytes(),
        stream_of("", draw.as_bytes()),
    ];
    objs.extend_from_slice(images);
    let mut pdf = b"%PDF-1.3\n".to_vec();
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
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    pdf
}

fn zlib(data: &[u8]) -> Vec<u8> {
    flate::deflate_zlib(data, &flate::Options::quick())
}

/// An image XObject under Flate.
fn image(entries: &str, samples: &[u8]) -> Vec<u8> {
    stream_of(&format!("/Type /XObject /Subtype /Image {entries} /Filter /FlateDecode"), &zlib(samples))
}

/// Stream `n` of a PDF.
fn stream(pdf: &[u8], n: u32) -> Stream {
    match xref::open(pdf).unwrap().load(n).unwrap().1 {
        Obj::Stream(s) => *s,
        x => panic!("object {n} is not a stream: {x:?}"),
    }
}

/// What a stream's general filters give, a predictor undone.
fn content(s: &Stream) -> Vec<u8> {
    let mut data = s.data.clone();
    for st in chain(&s.dict).unwrap() {
        data = unpredict(&st, &decode_stage(&st, &data, 1 << 30).unwrap()).unwrap();
    }
    data
}

fn filters(s: &Stream) -> Vec<Vec<u8>> {
    chain(&s.dict).unwrap().into_iter().map(|st| st.name).collect()
}

/// A dictionary's entries other than those in `but`.
fn entries(d: &Dict, but: &[&[u8]]) -> Vec<(Vec<u8>, Obj)> {
    d.0.iter().filter(|(k, _)| !but.contains(&k.as_slice())).cloned().collect()
}

const ENCODING: [&[u8]; 3] = [b"Length", b"Filter", b"DecodeParms"];
const INDEXED: [&[u8]; 5] = [b"Length", b"Filter", b"DecodeParms", b"ColorSpace", b"BitsPerComponent"];

fn quick() -> Options {
    Options::new().effort(0).threads(1)
}

fn smaller(pdf: &[u8], opt: &Options) -> (Vec<u8>, Report) {
    match optimize(pdf, opt).unwrap() {
        Optimized { data, outcome: Outcome::Smaller(report) } => (data, report),
        Optimized { outcome: Outcome::Untouched(why), .. } => panic!("untouched: {why}"),
    }
}

/// A 1-bit page of `w` x `h` pixels, rows padded to a byte with 1 bits: white (1) with letter-sized dark blobs,
/// like a scan.
fn scan(w: usize, h: usize, seed: u64) -> Vec<u8> {
    let mut next = rng(seed);
    let row = w.div_ceil(8);
    let mut bits = vec![0xFFu8; row * h];
    for y in (10..h.saturating_sub(14)).step_by(18) {
        for x in (8..w.saturating_sub(9)).step_by(9) {
            let (bw, bh) = (3 + next(5), 6 + next(6));
            for yy in y..y + bh {
                for xx in x..x + bw {
                    if next(9) != 0 {
                        bits[yy * row + xx / 8] &= !(0x80 >> (xx % 8));
                    }
                }
            }
        }
    }
    bits
}

/// PNG "Up" rows of `data`, `row` bytes each.
fn png_up(data: &[u8], row: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for (r, line) in data.chunks(row).enumerate() {
        out.push(2);
        out.extend(
            line.iter().enumerate().map(|(i, &b)| b.wrapping_sub(if r == 0 { 0 } else { data[(r - 1) * row + i] })),
        );
    }
    out
}

/// `n` pixels, each one of `colours` (all of them come up).
fn pixels<const M: usize>(n: usize, colours: &[[u8; M]], seed: u64) -> Vec<u8> {
    let mut next = rng(seed);
    (0..n).flat_map(|i| if i < colours.len() { colours[i] } else { colours[next(colours.len())] }).collect()
}

const FIVE: [[u8; 3]; 5] = [[255, 255, 255], [0, 0, 0], [200, 30, 30], [30, 90, 200], [240, 200, 20]];
const FOUR_GREYS: [[u8; 1]; 4] = [[0], [85], [170], [255]];
const RGB: &str = "/ColorSpace /DeviceRGB /BitsPerComponent 8";
const GREY: &str = "/ColorSpace /DeviceGray /BitsPerComponent 8";

/// The samples an Indexed image stands for: every index of its content looked up in its table. Checks the
/// colour space's shape on the way: `[/Indexed base hival table]`, `bits` per index, `m` bytes per colour.
fn looked_up(s: &Stream, base: &Obj, w: usize, bits: usize, m: usize) -> Vec<u8> {
    let Some(Obj::Array(space)) = s.dict.get(b"ColorSpace") else { panic!("{:?}", s.dict) };
    let [Obj::Name(family), b, Obj::Int(hival), Obj::Str(table)] = space.as_slice() else { panic!("{space:?}") };
    assert!(family == b"Indexed" && b == base, "{space:?}");
    assert_eq!(s.dict.get(b"BitsPerComponent"), Some(&Obj::Int(bits as i64)));
    assert_eq!(table.len(), (*hival as usize + 1) * m);
    // Ascending, each colour once.
    assert!(table.chunks(m).collect::<Vec<_>>().windows(2).all(|p| p[0] < p[1]), "{table:?}");
    let idx = content(s);
    let row = (w * bits).div_ceil(8);
    assert!(idx.len().is_multiple_of(row));
    let mut out = Vec::new();
    for line in idx.chunks(row) {
        for x in 0..w {
            let i = (line[x * bits / 8] >> (8 - bits - x * bits % 8)) as usize & ((1 << bits) - 1);
            out.extend_from_slice(&table[i * m..(i + 1) * m]);
        }
    }
    out
}

/// The images of `one_bit_images_become_jbig2_with_the_same_pixels`: (entries without the filter's, bits,
/// width), stored in turn under Flate, under Flate with a PNG predictor, and without a filter.
fn one_bit_images() -> Vec<(String, Vec<u8>, usize)> {
    vec![
        ("/Width 601 /Height 400 /ColorSpace /DeviceGray /BitsPerComponent 1".into(), scan(601, 400, 1), 601),
        ("/Width 600 /Height 400 /ImageMask true /BitsPerComponent 1 /Decode [1 0]".into(), scan(600, 400, 2), 600),
        ("/Width 500 /Height 300 /ImageMask true".into(), scan(500, 300, 3), 500),
        // Too small for JBIG2's fixed bytes to pay.
        ("/Width 16 /Height 16 /ColorSpace /DeviceGray /BitsPerComponent 1".into(), scan(16, 16, 4), 16),
    ]
}

fn one_bit_pdf() -> Vec<u8> {
    let i = one_bit_images();
    let xobject = "/Type /XObject /Subtype /Image";
    let parms = "/Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors 1 /BitsPerComponent 1 /Columns 600 >>";
    page_of(&[
        image(&i[0].0, &i[0].1),
        stream_of(&format!("{xobject} {} {parms}", i[1].0), &zlib(&png_up(&i[1].1, 75))),
        stream_of(&format!("{xobject} {}", i[2].0), &i[2].1),
        image(&i[3].0, &i[3].1),
    ])
}

#[test]
#[cfg(feature = "fax")]
fn one_bit_images_become_jbig2_with_the_same_pixels() {
    use gpress::fax::{ccitt, jbig2};
    let pdf = one_bit_pdf();
    for effort in [0, 100] {
        let (out, report) = smaller(&pdf, &quick().effort(effort));
        assert_eq!((report.bilevel_to_jbig2, report.fax_to_jbig2, report.images_indexed), (3, 0, 0), "effort {effort}");
        assert!(out.starts_with(b"%PDF-1.") && out[7] >= b'4', "JBIG2Decode needs PDF 1.4");
        for (k, (_, bits, w)) in one_bit_images().into_iter().enumerate() {
            let (before, after) = (stream(&pdf, 5 + k as u32), stream(&out, 5 + k as u32));
            assert_eq!(entries(&after.dict, &ENCODING), entries(&before.dict, &ENCODING), "image {k}");
            if k == 3 {
                assert!(!filters(&after).contains(&b"JBIG2Decode".to_vec()));
                assert_eq!(content(&after), bits);
                continue;
            }
            assert!(
                filters(&after) == [b"JBIG2Decode".to_vec()] && !after.dict.has(b"DecodeParms"),
                "{:?}",
                after.dict
            );
            // JBIG2Decode gives the inverse of the bitmap: the bits the old filters gave, pixel for pixel.
            let img = jbig2::read_pdf_stream(&after.data, true, usize::MAX).expect("a JBIG2 stream of ours");
            let row = w.div_ceil(8);
            assert_eq!((img.width, img.rows()), (w, bits.len() / row), "image {k}");
            let mut px = vec![0u8; w];
            for (r, line) in bits.chunks(row).enumerate() {
                ccitt::expand_row(img.row(r), &mut px);
                assert!((0..w).all(|x| px[x] == line[x / 8] >> (7 - x % 8) & 1), "image {k}, row {r}, effort {effort}");
            }
        }
    }
    // Switched off: no JBIG2, every image's content as it was.
    let (out, report) = smaller(&pdf, &quick().jbig2(false));
    assert_eq!(report.bilevel_to_jbig2, 0);
    for (k, (_, bits, _)) in one_bit_images().into_iter().enumerate() {
        let after = stream(&out, 5 + k as u32);
        assert!(!filters(&after).contains(&b"JBIG2Decode".to_vec()), "image {k}");
        assert_eq!(content(&after), bits, "image {k}");
    }
}

/// Without the fax codecs no image becomes JBIG2: the output is the one `jbig2` off gives.
#[test]
#[cfg(not(feature = "fax"))]
fn without_fax_no_one_bit_image_becomes_jbig2() {
    let pdf = one_bit_pdf();
    let (on, report) = smaller(&pdf, &quick());
    assert_eq!(report.bilevel_to_jbig2, 0);
    assert!((5..9).all(|n| !filters(&stream(&on, n)).contains(&b"JBIG2Decode".to_vec())));
    assert!(on == smaller(&pdf, &quick().jbig2(false)).0);
}

#[test]
#[cfg(feature = "fax")]
fn the_jbig2_proof_compares_pixels_and_nothing_else_passes() {
    use gpress::fax::pdf::{bilevel_candidate, same_bits};
    let dict = |text: &str| match gpress::pdf::parse::Lexer::new(text.as_bytes(), 0).parse_obj() {
        Ok(Obj::Dict(d)) => d,
        x => panic!("{text}: {x:?}"),
    };
    let image = |entries: &str, data: &[u8]| Stream {
        dict: dict(&format!("<< /Subtype /Image {entries} >>")),
        data: data.to_vec(),
    };
    let bits = scan(301, 200, 7);
    let grey = "/Width 301 /Height 200 /ColorSpace /DeviceGray /BitsPerComponent 1";
    let a = image(grey, &bits);
    let (d, z) = bilevel_candidate(&a, &bits, bits.len()).expect("JBIG2 is smaller than the bits");
    let b = Stream { dict: d, data: z };
    assert!(same_bits(&a, &bits, &b));
    // The padding bits of a row are not pixels.
    let mut padded = bits.clone();
    padded.chunks_mut(38).for_each(|row| row[37] &= 0xF8);
    assert!(padded != bits && same_bits(&a, &padded, &b));
    assert_eq!(bilevel_candidate(&image(grey, &padded), &padded, padded.len()).map(|x| x.1), Some(b.data.clone()));
    // One pixel changed, the other polarity, a row missing, another width: not the same image.
    let mut flipped = bits.clone();
    flipped[38 * 100 + 5] ^= 0x10;
    assert!(!same_bits(&a, &flipped, &b));
    let inverse: Vec<u8> = bits.iter().map(|x| !x).collect();
    assert!(!same_bits(&a, &inverse, &b));
    assert!(!same_bits(&a, &bits[38..], &b) && !same_bits(&a, &[&bits[..], &[0xFF; 38]].concat(), &b));
    let wide = image("/Width 304 /Height 200 /ColorSpace /DeviceGray /BitsPerComponent 1", &bits);
    assert!(!same_bits(&wide, &bits, &b));
    // Not a JBIG2 stream alone, or one with parameters.
    let mut chained = b.clone();
    chained
        .dict
        .set(b"Filter", Obj::Array(vec![Obj::Name(b"FlateDecode".to_vec()), Obj::Name(b"JBIG2Decode".to_vec())]));
    assert!(!same_bits(&a, &bits, &chained));
    let mut with_parms = b.clone();
    with_parms.dict.set(b"DecodeParms", Obj::Dict(Dict::default()));
    assert!(!same_bits(&a, &bits, &with_parms));
    // Only a 1-bit DeviceGray image or a stencil mask with direct sizes is written as JBIG2 at all.
    for entries in [
        "/Width 301 /Height 200 /ColorSpace /DeviceGray /BitsPerComponent 2",
        "/Width 301 /Height 200 /ColorSpace /DeviceRGB /BitsPerComponent 1",
        "/Width 301 /Height 200 /ColorSpace [/Indexed /DeviceRGB 1 <000000FFFFFF>] /BitsPerComponent 1",
        "/Width 301 /Height 9 0 R /ColorSpace /DeviceGray /BitsPerComponent 1",
        "/Width 301 /Height 200 /ImageMask true /BitsPerComponent 8",
        "/Width 301 /Height 199 /ColorSpace /DeviceGray /BitsPerComponent 1",
    ] {
        let other = image(entries, &bits);
        assert!(bilevel_candidate(&other, &bits, bits.len()).is_none() && !same_bits(&other, &bits, &b), "{entries}");
    }
    // At least 5% smaller than the best other encoding, or nothing.
    let n = b.data.len();
    assert!(bilevel_candidate(&a, &bits, n * 100 / 95 + 1).is_some());
    assert!(bilevel_candidate(&a, &bits, n * 100 / 95 - 1).is_none());
}

/// The images of `few_colours_become_an_indexed_image`: (entries, samples, width, components, bits per index
/// when it becomes Indexed).
type Charts = Vec<(String, Vec<u8>, usize, usize, Option<usize>)>;

fn charts() -> Charts {
    let many: Vec<[u8; 3]> = (0..300).map(|k| [k as u8, (k / 256) as u8, 7]).collect();
    let seventeen: Vec<[u8; 1]> = (0..17).map(|k| [k * 15]).collect();
    vec![
        (format!("/Width 121 /Height 80 {RGB}"), pixels(121 * 80, &FIVE, 11), 121, 3, Some(4)),
        (format!("/Width 120 /Height 80 {RGB}"), pixels(120 * 80, &many, 12), 120, 3, None),
        // A reader may round a table's colours another way than an image's samples in an ICCBased or a
        // calibrated colour space: those images stay as they are.
        (
            "/Width 50 /Height 90 /ColorSpace [/ICCBased 14 0 R] /BitsPerComponent 8".into(),
            pixels(50 * 90, &FIVE[..3], 13),
            50,
            3,
            None,
        ),
        // Two colours take 2 bits each as well: a reader may draw a 1-bit image another way.
        (format!("/Width 99 /Height 70 {GREY}"), pixels(99 * 70, &[[0u8], [255]], 14), 99, 1, Some(2)),
        (format!("/Width 99 /Height 70 {GREY}"), pixels(99 * 70, &seventeen, 15), 99, 1, None),
        (format!("/Width 64 /Height 64 {RGB}"), pixels(64 * 64, &many[..256], 16), 64, 3, Some(8)),
        ("/Width 64 /Height 64 /ColorSpace 15 0 R /BitsPerComponent 8".into(), pixels(64 * 64, &FIVE, 18), 64, 3, None),
        (
            "/Width 60 /Height 40 /ColorSpace [/CalGray << /WhitePoint [0.9505 1 1.089] >>] /BitsPerComponent 8".into(),
            pixels(60 * 40, &FOUR_GREYS, 17),
            60,
            1,
            None,
        ),
        (format!("/Width 60 /Height 40 {GREY}"), pixels(60 * 40, &FOUR_GREYS, 17), 60, 1, Some(2)),
    ]
}

/// The charts as objects 5 to 13, then an ICC profile of three components (14; the engine reads only its /N)
/// and a colour space by reference (15).
fn charts_pdf() -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = charts().iter().map(|(entries, px, ..)| image(entries, px)).collect();
    objs.push(stream_of("/N 3", &[0u8; 128]));
    objs.push(b"[/CalRGB << /WhitePoint [0.9505 1 1.089] >>]".to_vec());
    page_of(&objs)
}

#[test]
fn few_colours_become_an_indexed_image() {
    let pdf = charts_pdf();
    for effort in [0, 100] {
        let (out, report) = smaller(&pdf, &quick().effort(effort));
        assert_eq!((report.images_indexed, report.bilevel_to_jbig2), (4, 0), "effort {effort}");
        for (k, (_, px, w, m, bits)) in charts().into_iter().enumerate() {
            let (before, after) = (stream(&pdf, 5 + k as u32), stream(&out, 5 + k as u32));
            match bits {
                Some(bits) => {
                    assert_eq!(entries(&after.dict, &INDEXED), entries(&before.dict, &INDEXED), "image {k}");
                    let base = before.dict.get(b"ColorSpace").unwrap();
                    assert!(looked_up(&after, base, w, bits, m) == px, "image {k}, effort {effort}");
                }
                None => {
                    assert_eq!(entries(&after.dict, &ENCODING), entries(&before.dict, &ENCODING), "image {k}");
                    assert!(content(&after) == px, "image {k}, effort {effort}");
                }
            }
        }
    }
    // Switched off: every image keeps its colour space and its samples.
    let (out, report) = smaller(&pdf, &quick().palette(false));
    assert_eq!(report.images_indexed, 0);
    for (k, (_, px, ..)) in charts().into_iter().enumerate() {
        let (before, after) = (stream(&pdf, 5 + k as u32), stream(&out, 5 + k as u32));
        assert_eq!(entries(&after.dict, &ENCODING), entries(&before.dict, &ENCODING), "image {k}");
        assert!(content(&after) == px, "image {k}");
    }
}

/// Images that an index can't stand for, each a chart of five colours or four greys that would otherwise
/// become Indexed (5, 7 and 14 do): (entries, samples, whether it becomes Indexed).
fn guarded() -> Vec<(String, Vec<u8>, bool)> {
    let chart = |seed: u64| pixels(120 * 80, &FIVE, seed);
    let greys = |seed: u64| pixels(120 * 80, &FOUR_GREYS, seed);
    let size = "/Width 120 /Height 80";
    vec![
        (format!("{size} {RGB} /SMask 6 0 R"), chart(21), true),
        // 6: the soft mask of 5 stays DeviceGray.
        (format!("{size} {GREY}"), greys(22), false),
        // 7: the same kind of image, no one's mask.
        (format!("{size} {GREY}"), greys(23), true),
        // 8: identical to 6, which is encoded once for both.
        (format!("{size} {GREY}"), greys(22), false),
        (format!("{size} {RGB} /Decode [0 1 0 1 0 1]"), chart(24), false),
        (format!("{size} {RGB} /Mask [0 0 0 0 0 0]"), chart(25), false),
        (format!("{size} {RGB} /Interpolate true"), chart(26), false),
        // 12: its soft mask (13) has a matte colour, which is given in the image's colour space.
        (format!("{size} {RGB} /SMask 13 0 R"), chart(27), false),
        (format!("{size} {GREY} /Matte [1 1 1]"), greys(28), false),
        // 14: a stencil mask (15) is no colour key.
        (format!("{size} {RGB} /Mask 15 0 R"), chart(29), true),
        (format!("{size} /ImageMask true"), scan(120, 80, 30), false),
        (format!("{size} {RGB} /Alternates [<< /Image 5 0 R >>]"), chart(31), false),
        (
            "/Width 120 /Height 80 /ColorSpace /DeviceRGB /BitsPerComponent 16".into(),
            pixels(120 * 80 * 2, &FIVE, 32),
            false,
        ),
    ]
}

#[test]
fn masks_and_images_that_say_more_than_colours_keep_their_colour_space() {
    let images = guarded();
    let pdf = page_of(&images.iter().map(|(entries, px, _)| image(entries, px)).collect::<Vec<_>>());
    let (out, report) = smaller(&pdf, &quick().jbig2(false));
    assert_eq!(report.images_indexed, 3);
    for (k, (entries_, px, indexed)) in images.iter().enumerate() {
        let (before, after) = (stream(&pdf, 5 + k as u32), stream(&out, 5 + k as u32));
        if *indexed {
            let m = if entries_.contains("DeviceRGB") { 3 } else { 1 };
            let bits = if m == 3 { 4 } else { 2 };
            assert!(looked_up(&after, before.dict.get(b"ColorSpace").unwrap(), 120, bits, m) == *px, "image {}", 5 + k);
        } else {
            assert_eq!(entries(&after.dict, &ENCODING), entries(&before.dict, &ENCODING), "image {}", 5 + k);
            assert!(content(&after) == *px, "image {}", 5 + k);
        }
    }
}

/// An image that a predictor shrinks more than a colour table can: a walk up through all 256 greys, round and
/// round in steps of one, stored as RGB. Flate alone gets little from it, the Sub predictor nearly everything, and the Indexed form
/// would pay 768 bytes of table to save a few: it is compared with the predicted image, not with Flate alone.
#[test]
fn an_indexed_image_is_compared_with_the_best_other_encoding() {
    let mut next = rng(41);
    let mut v = 128u8;
    let px: Vec<u8> = (0..64 * 64)
        .flat_map(|_| {
            v = if next(4) == 0 { v.wrapping_sub(1) } else { v.wrapping_add(1) };
            [v; 3]
        })
        .collect();
    let pdf = page_of(&[image(&format!("/Width 64 /Height 64 {RGB}"), &px)]);
    for effort in [0, 100] {
        let (on, report) = smaller(&pdf, &quick().effort(effort));
        assert_eq!(report.images_indexed, 0, "effort {effort}");
        assert!(on == smaller(&pdf, &quick().effort(effort).palette(false)).0, "effort {effort}");
    }
}

/// A default colour space gives DeviceRGB the meaning of another colour space for everything on the page: no
/// image of such a file becomes Indexed.
#[test]
fn a_file_with_a_default_colour_space_keeps_its_images() {
    let chart = image(&format!("/Width 121 /Height 80 {RGB}"), &pixels(121 * 80, &FIVE, 11));
    let defaults = "/ColorSpace << /DefaultRGB [/CalRGB << /WhitePoint [0.9505 1 1.089] >>] >>";
    let (_, report) = smaller(&page_with(defaults, std::slice::from_ref(&chart)), &quick());
    assert_eq!(report.images_indexed, 0);
    let (_, report) = smaller(&page_of(&[chart]), &quick());
    assert_eq!(report.images_indexed, 1);
}

fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// One chart of five colours, stored without a filter (so the file's bytes depend on nothing but this test).
fn chart_pdf() -> Vec<u8> {
    let px = pixels(121 * 80, &FIVE, 11);
    page_of(&[stream_of(&format!("/Type /XObject /Subtype /Image /Width 121 /Height 80 {RGB}"), &px)])
}

/// With `palette` off the output is what it was before there were Indexed images: the bytes the tool built
/// from main at 1956435 gives for the same file at effort 0 (`gpress optimize --effort 0 --threads 1`), by their
/// length and FNV-1a hash.
#[test]
fn palette_off_gives_the_bytes_of_before() {
    let pdf = chart_pdf();
    assert_eq!((pdf.len(), fnv(&pdf)), CHART_PDF, "the test file itself changed");
    let (off, report) = smaller(&pdf, &quick().palette(false));
    assert_eq!(report.images_indexed, 0);
    assert_eq!((off.len(), fnv(&off)), CHART_BEFORE);
    let (on, report) = smaller(&pdf, &quick());
    assert!(report.images_indexed == 1 && on.len() < off.len(), "{} against {}", on.len(), off.len());
}

/// `chart_pdf()` and main's output for it: (length, FNV-1a).
const CHART_PDF: (usize, u64) = (29700, 0x2148_5f86_0c17_dcac);
const CHART_BEFORE: (usize, u64) = (4923, 0xaeff_2c19_76aa_fac0);

#[test]
fn one_and_four_threads_give_the_same_bytes() {
    let guarded: Vec<Vec<u8>> = guarded().iter().map(|(entries, px, _)| image(entries, px)).collect();
    for pdf in [one_bit_pdf(), charts_pdf(), page_of(&guarded)] {
        for effort in [0, 100] {
            let one = smaller(&pdf, &quick().effort(effort));
            let four = smaller(&pdf, &quick().effort(effort).threads(4));
            assert!(one.0 == four.0 && one.1 == four.1, "effort {effort}");
        }
    }
}
