//! The lossy image step, end to end through `gpress::optimize` on small PDFs built here.

use gpress::pdf::filters::{chain, decode_stage, is_general, unpredict};
use gpress::pdf::object::Obj;
use gpress::{Lossy, Options, Outcome};

/// A PDF of these objects, numbered from 1 (the first is the catalog).
fn build(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut d = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(d.len());
        d.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        d.extend_from_slice(o);
        d.extend_from_slice(b"\nendobj\n");
    }
    let xref = d.len();
    d.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        d.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    d.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    d
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    [format!("<< {dict} /Length {} >>\nstream\n", data.len()).as_bytes(), data, b"\nendstream"].concat()
}

/// An image XObject of 8-bit (or `bpc`-bit) samples behind Flate; `more` is further dictionary entries.
fn image(w: usize, h: usize, cs: &str, bpc: u8, more: &str, px: &[u8]) -> Vec<u8> {
    let z = gpress::flate::deflate_zlib(px, &gpress::flate::Options::quick());
    let dict = format!(
        "/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /{cs} /BitsPerComponent {bpc} \
         /Filter /FlateDecode {more}"
    );
    stream(&dict, &z)
}

/// One page of 612 by 792 points that shows `image` (object 4) `side` points wide and high, and whatever
/// else `content` draws; `more` are the objects from 6 on and `resources` further /XObject entries.
fn page(image: Vec<u8>, side: u32, resources: &str, content: &str, more: &[Vec<u8>]) -> Vec<u8> {
    let content = format!("q {side} 0 0 {side} 20 20 cm /Im0 Do Q {content}");
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R \
             /Resources << /XObject << /Im0 4 0 R {resources} >> >> >>"
        )
        .into_bytes(),
        image,
        stream("", content.as_bytes()),
    ];
    objs.extend_from_slice(more);
    build(&objs)
}

fn rng(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// A soft picture, like a camera's: slow ramps with a little noise, `ch` bytes a pixel.
fn photo(w: usize, h: usize, ch: usize) -> Vec<u8> {
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut px = Vec::with_capacity(w * h * ch);
    for i in 0..w * h {
        let (x, y) = (i % w, i / w);
        for c in 0..ch {
            px.push((40 + x * (150 - 30 * c) / w + y * (50 + 20 * c) / h + (rng(&mut s) % 9) as usize) as u8);
        }
    }
    px
}

/// Line art, grey: dark boxes on white, and one sample in eight flipped to black or white so that Flate cannot
/// make it tiny at any size.
fn line_art(w: usize, h: usize) -> Vec<u8> {
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut px = vec![255u8; w * h];
    for k in 0..12 {
        let (x0, y0) = (w * (k % 4) / 4 + 5, h * (k / 4) / 3 + 5);
        for y in y0..(y0 + h / 6).min(h) {
            px[y * w + x0..y * w + (x0 + w / 8).min(w)].fill(0);
        }
    }
    for v in px.iter_mut() {
        if rng(&mut s).is_multiple_of(8) {
            *v = !*v;
        }
    }
    px
}

fn options(lossy: Option<Lossy>, threads: usize) -> Options {
    Options::default().effort(0).threads(threads).lossy(lossy)
}

/// The optimised file (the input itself when nothing got smaller) and what the lossy step did to its images,
/// as (resampled, to JPEG, JPEG again).
fn optimize(pdf: &[u8], lossy: Option<Lossy>) -> (Vec<u8>, (usize, usize, usize)) {
    let r = gpress::optimize(pdf, &options(lossy, 1)).expect("optimize");
    let did = match &r.outcome {
        Outcome::Smaller(s) => (s.images_resampled, s.images_to_jpeg, s.jpegs_rewritten),
        Outcome::Untouched(_) => (0, 0, 0),
    };
    (r.data, did)
}

/// The image (object 4) of an optimised file: its width and height, whether it is a JPEG, and its data with
/// the general filters and their predictors undone (the samples, or the JPEG file).
fn image_of(pdf: &[u8]) -> (i64, i64, bool, Vec<u8>) {
    let doc = gpress::pdf::xref::open(pdf).expect("readable");
    let (_, o) = doc.load(4).expect("image object");
    let Obj::Stream(s) = o else { panic!("not a stream") };
    let mut bytes = s.data.clone();
    let mut jpeg = false;
    for st in chain(&s.dict).expect("chain") {
        if is_general(&st.name) {
            assert!(!jpeg, "general filter after the codec");
            bytes = decode_stage(&st, &bytes, 1 << 30).expect("decodes");
            if st.parms.is_some() {
                bytes = unpredict(&st, &bytes).expect("predictor");
            }
        } else {
            assert_eq!(st.name, b"DCTDecode");
            jpeg = true;
        }
    }
    let int = |k: &[u8]| s.dict.get(k).and_then(|v| v.as_int()).expect("a size");
    (int(b"Width"), int(b"Height"), jpeg, bytes)
}

/// Every level leaves `pdf` byte for byte what the lossless path gives, and counts nothing.
fn untouched_by_every_level(pdf: &[u8], what: &str) {
    let (lossless, _) = optimize(pdf, None);
    for level in 1..=3 {
        let (out, did) = optimize(pdf, Some(Lossy::level(level)));
        assert_eq!(did, (0, 0, 0), "{what}, level {level}");
        assert!(out == lossless, "{what}, level {level}: not the lossless output");
    }
}

#[test]
fn off_is_the_lossless_path() {
    let pdf = page(image(600, 600, "DeviceRGB", 8, "", &photo(600, 600, 3)), 72, "", "", &[]);
    let (plain, did) = optimize(&pdf, None);
    assert_eq!(did, (0, 0, 0));
    assert!(optimize(&pdf, Some(Lossy::OFF)).0 == plain);
    assert!(optimize(&pdf, Some(Lossy::level(0))).0 == plain);
    let (w, h, jpeg, px) = image_of(&plain);
    assert_eq!((w, h, jpeg), (600, 600, false));
    assert!(px == photo(600, 600, 3), "the lossless path keeps every sample");
}

#[cfg(feature = "jpeg")]
#[test]
fn a_photo_is_resampled_to_its_floor_and_becomes_a_jpeg() {
    use gpress::clock::Deadline;
    // 600 pixels over an inch.
    for (cs, ch) in [("DeviceRGB", 3), ("DeviceGray", 1)] {
        let px = photo(600, 600, ch);
        let pdf = page(image(600, 600, cs, 8, "", &px), 72, "", "", &[]);
        let (lossless, _) = optimize(&pdf, None);
        for (level, side) in [(1u8, 300usize), (2, 150), (3, 150)] {
            let (out, did) = optimize(&pdf, Some(Lossy::level(level)));
            assert_eq!(did, (1, 1, 0), "{cs}, level {level}");
            assert!(out.len() < lossless.len() / 4, "{cs}, level {level}: {} of {}", out.len(), lossless.len());
            let (w, h, jpeg, bytes) = image_of(&out);
            assert_eq!((w, h, jpeg), (side as i64, side as i64, true), "{cs}, level {level}");
            // The JPEG is the picture: its luma is close to the original's, averaged down.
            let pic = gpress::jpeg::decode(&bytes, 1 << 30, Deadline::NONE).expect("our JPEG decodes");
            assert_eq!((pic.width, pic.height, pic.planes.len()), (side, side, ch), "{cs}, level {level}");
            let f = 600 / side;
            let mut worst = 0u32;
            for y in 0..side {
                for x in 0..side {
                    let mut sum = 0u32;
                    for (dy, dx) in (0..f).flat_map(|dy| (0..f).map(move |dx| (dy, dx))) {
                        let p = &px[((y * f + dy) * 600 + x * f + dx) * ch..][..ch];
                        sum += if ch == 1 {
                            p[0] as u32
                        } else {
                            (299 * p[0] as u32 + 587 * p[1] as u32 + 114 * p[2] as u32) / 1000
                        };
                    }
                    let mean = sum / (f * f) as u32;
                    worst = worst.max(mean.abs_diff(pic.planes[0].px[y * side + x] as u32));
                }
            }
            assert!(worst <= 16, "{cs}, level {level}: luma off by {worst}");
        }
        // The thread count changes nothing.
        for level in 1..=3 {
            let one = gpress::optimize(&pdf, &options(Some(Lossy::level(level)), 1)).unwrap().data;
            let four = gpress::optimize(&pdf, &options(Some(Lossy::level(level)), 4)).unwrap().data;
            assert!(one == four, "{cs}, level {level}: 1 and 4 threads differ");
        }
    }
}

#[cfg(feature = "jpeg")]
#[test]
fn a_photo_at_200_ppi_keeps_its_size() {
    // 600 pixels over three inches: under 5/4 of level 1's floor of 300.
    let pdf = page(image(600, 600, "DeviceRGB", 8, "", &photo(600, 600, 3)), 216, "", "", &[]);
    let (lossless, _) = optimize(&pdf, None);
    // Without a JPEG quality nothing is done to it.
    let (out, did) = optimize(&pdf, Some(Lossy { jpeg_quality: 0, ..Lossy::level(1) }));
    assert_eq!(did, (0, 0, 0));
    assert!(out == lossless);
    // Level 1 writes it as a JPEG of the same size.
    let (out, did) = optimize(&pdf, Some(Lossy::level(1)));
    assert_eq!(did, (0, 1, 0));
    let (w, h, jpeg, _) = image_of(&out);
    assert_eq!((w, h, jpeg), (600, 600, true));
    // Level 2's floor of 150 is 3/4 of its resolution: 450 pixels.
    let (out, did) = optimize(&pdf, Some(Lossy::level(2)));
    assert_eq!(did, (1, 1, 0));
    let (w, h, jpeg, _) = image_of(&out);
    assert_eq!((w, h, jpeg), (450, 450, true));
}

#[cfg(feature = "jpeg")]
#[test]
fn a_jpeg_photo_is_resampled_or_written_again() {
    // The JPEG our own step makes of the photo at level 1's quality, shown at 600 and at 200 pixels an inch.
    let first = page(image(600, 600, "DeviceRGB", 8, "", &photo(600, 600, 3)), 216, "", "", &[]);
    let (_, _, jpeg, file) = image_of(&optimize(&first, Some(Lossy::level(1))).0);
    assert!(jpeg);
    let as_jpeg = |side: u32| {
        let dict = "/Type /XObject /Subtype /Image /Width 600 /Height 600 /ColorSpace /DeviceRGB \
                    /BitsPerComponent 8 /Filter /DCTDecode";
        page(stream(dict, &file), side, "", "", &[])
    };
    // At 600: resampled to each level's floor, still a JPEG, and not counted as a new JPEG.
    for (level, side) in [(1u8, 300i64), (2, 150), (3, 150)] {
        let (out, did) = optimize(&as_jpeg(72), Some(Lossy::level(level)));
        assert_eq!(did, (1, 0, 0), "level {level}");
        let (w, h, jpeg, _) = image_of(&out);
        assert_eq!((w, h, jpeg), (side, side, true), "level {level}");
    }
    // At 200: levels 1 and 2 have nothing to do to a JPEG (level 2 resamples it to 450); level 3 writes it
    // again at its lower quality.
    let pdf = as_jpeg(216);
    let (lossless, _) = optimize(&pdf, None);
    let (out, did) = optimize(&pdf, Some(Lossy::level(1)));
    assert_eq!(did, (0, 0, 0));
    assert!(out == lossless);
    let (out, did) = optimize(&pdf, Some(Lossy { photo_ppi: 300, ..Lossy::level(3) }));
    assert_eq!(did, (0, 0, 1));
    let (w, h, jpeg, again) = image_of(&out);
    assert_eq!((w, h, jpeg), (600, 600, true));
    assert!(again.len() * 10 <= file.len() * 9, "{} of {}", again.len(), file.len());
}

#[test]
fn line_art_is_resampled_to_300_ppi_and_never_a_jpeg() {
    let px = line_art(600, 600);
    let pdf = page(image(600, 600, "DeviceGray", 8, "", &px), 72, "", "", &[]);
    let (lossless, _) = optimize(&pdf, None);
    for level in 1..=3 {
        let (out, did) = optimize(&pdf, Some(Lossy::level(level)));
        assert_eq!(did, (1, 0, 0), "level {level}");
        assert!(out.len() * 10 <= lossless.len() * 9, "level {level}: {} of {}", out.len(), lossless.len());
        let (w, h, jpeg, small) = image_of(&out);
        assert_eq!((w, h, jpeg), (300, 300, false), "level {level}");
        // Each sample is the mean of the four it covers, rounded to the nearest.
        assert_eq!(small.len(), 300 * 300);
        for (i, &v) in small.iter().enumerate() {
            let (x, y) = (2 * (i % 300), 2 * (i / 300));
            let sum: u32 =
                [(0, 0), (0, 1), (1, 0), (1, 1)].iter().map(|(dy, dx)| px[(y + dy) * 600 + x + dx] as u32).sum();
            assert_eq!(v as u32, (sum + 2) / 4, "sample {i}");
        }
        let four = gpress::optimize(&pdf, &options(Some(Lossy::level(level)), 4)).unwrap().data;
        assert!(out == four, "level {level}: 1 and 4 threads differ");
    }
    // At 200 pixels an inch it is under its floor at every level.
    untouched_by_every_level(&page(image(600, 600, "DeviceGray", 8, "", &px), 216, "", "", &[]), "line art at 200 ppi");
}

#[test]
fn protected_images_are_what_the_lossless_path_gives() {
    let rgb = photo(600, 600, 3);
    let grey = photo(600, 600, 1);
    // One bit a sample: 2,000 by 2,000 over an inch.
    let bits = image(2000, 2000, "DeviceGray", 1, "", &line_art(250, 2000));
    untouched_by_every_level(&page(bits, 72, "", "", &[]), "1 bit");
    // 200 by 200: a small image, however densely it is shown.
    let small = image(200, 200, "DeviceRGB", 8, "", &photo(200, 200, 3));
    untouched_by_every_level(&page(small, 18, "", "", &[]), "small");
    // A soft mask that carries /Matte: its image's colours are blended with it.
    let matte = image(600, 600, "DeviceGray", 8, "/Matte [0 0 0]", &grey);
    let pdf = page(image(600, 600, "DeviceRGB", 8, "/SMask 6 0 R", &rgb), 72, "", "", &[matte]);
    untouched_by_every_level(&pdf, "/SMask with /Matte");
    // An image that is drawn and is also the soft mask of another (a small one, protected by its size).
    let masked = image(100, 100, "DeviceRGB", 8, "/SMask 4 0 R", &photo(100, 100, 3));
    let pdf = page(
        image(600, 600, "DeviceGray", 8, "", &grey),
        72,
        "/Im1 6 0 R",
        "q 9 0 0 9 300 300 cm /Im1 Do Q",
        &[masked],
    );
    untouched_by_every_level(&pdf, "also a soft mask");
    // The others: a colour-key mask, a /Decode array, an indexed colour space, an image no page draws.
    let pdf = page(image(600, 600, "DeviceRGB", 8, "/Mask [0 0 0 0 0 0]", &rgb), 72, "", "", &[]);
    untouched_by_every_level(&pdf, "colour-key mask");
    let pdf = page(image(600, 600, "DeviceRGB", 8, "/Decode [1 0 1 0 1 0]", &rgb), 72, "", "", &[]);
    untouched_by_every_level(&pdf, "/Decode");
    // The short key of an inline image beside the full one: a reader may go by either.
    let pdf = page(image(600, 600, "DeviceRGB", 8, "/W 600", &rgb), 72, "", "", &[]);
    untouched_by_every_level(&pdf, "/W beside /Width");
    let undrawn = image(600, 600, "DeviceRGB", 8, "", &rgb);
    let pdf = page(image(200, 200, "DeviceGray", 8, "", &photo(200, 200, 1)), 72, "/Im1 6 0 R", "", &[undrawn]);
    untouched_by_every_level(&pdf, "never drawn");
}

#[test]
fn an_image_a_page_does_not_measure_is_left_alone() {
    let px = line_art(600, 600);
    // An inline image on the page: none of the page's images are measured.
    let pdf = page(image(600, 600, "DeviceGray", 8, "", &px), 72, "", "BI /W 1 /H 1 /BPC 8 /CS /G ID \x00 EI", &[]);
    untouched_by_every_level(&pdf, "a page with an inline image");
    // A second use the walk cannot follow: the image is also an annotation's appearance entry.
    let annot = b"<< /Type /Annot /Subtype /Stamp /Rect [0 0 9 9] /Odd 4 0 R >>".to_vec();
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Annots [6 0 R] \
          /Resources << /XObject << /Im0 4 0 R >> >> >>"
            .to_vec(),
        image(600, 600, "DeviceGray", 8, "", &px),
        stream("", b"q 72 0 0 72 20 20 cm /Im0 Do Q"),
    ];
    objs.push(annot);
    untouched_by_every_level(&build(&objs), "referenced from outside a resource dictionary");
}

#[test]
fn settings_are_floors_and_single_overrides() {
    let px = line_art(1200, 1200);
    // 1,200 pixels over an inch.
    let pdf = page(image(1200, 1200, "DeviceGray", 8, "", &px), 72, "", "", &[]);
    // Asked for 100: the floor of 300 holds. Asked for 600: 600. Asked for 0: never.
    for (art_ppi, side) in [(100u16, Some(300i64)), (600, Some(600)), (0, None)] {
        let (out, did) = optimize(&pdf, Some(Lossy { art_ppi, ..Lossy::level(1) }));
        let (w, h, jpeg, _) = image_of(&out);
        assert_eq!((w, h, jpeg), (side.unwrap_or(1200), side.unwrap_or(1200), false), "art_ppi {art_ppi}");
        assert_eq!(did, (side.is_some() as usize, 0, 0), "art_ppi {art_ppi}");
    }
    // A scan (an image over most of its page) has the line-art floor even when it looks like a photo.
    #[cfg(feature = "jpeg")]
    {
        let scan = image(900, 900, "DeviceGray", 8, "", &photo(900, 900, 1));
        let objs = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Contents 5 0 R \
              /Resources << /XObject << /Im0 4 0 R >> >> >>"
                .to_vec(),
            scan,
            stream("", b"q 72 0 0 72 0 0 cm /Im0 Do Q"),
        ];
        // 900 pixels over an inch, level 2 (photos 150, line art and scans 300): 300, as a JPEG.
        let (out, did) = optimize(&build(&objs), Some(Lossy::level(2)));
        assert_eq!(did, (1, 1, 0));
        let (w, h, jpeg, _) = image_of(&out);
        assert_eq!((w, h, jpeg), (300, 300, true));
    }
}

/// The six efforts through the public options: 0, 20 and 40 never run the step; 60, 80 and 100 are the work of
/// 40 after the step at levels 1, 2 and 3; an override starts from what the effort means; the engine's level
/// turns the step off; and a target size goes to a lossy effort only when no lossless one reaches it.
#[cfg(feature = "jpeg")]
#[test]
fn the_lossy_efforts_are_the_three_levels() {
    // 600 pixels over an inch.
    let pdf = page(image(600, 600, "DeviceRGB", 8, "", &photo(600, 600, 3)), 72, "", "", &[]);
    let base = || Options::default().threads(1);
    let at = |o: Options| gpress::optimize(&pdf, &o).expect("optimize").data;
    let lossless: Vec<Vec<u8>> = [0, 20, 40].map(|effort| at(base().effort(effort))).into();
    for out in &lossless {
        assert!(image_of(out).3 == photo(600, 600, 3), "a lossless effort keeps every sample");
    }
    for (effort, level) in [(60u8, 1u8), (80, 2), (100, 3)] {
        let out = at(base().effort(effort));
        assert!(out == at(base().effort(40).lossy(Some(Lossy::level(level)))), "effort {effort}");
        assert!(out.len() < lossless[2].len() / 4, "effort {effort}");
    }
    // Effort 60 with photos at 150 pixels an inch is effort 80; quality alone on a lossless effort writes the
    // photo as a JPEG of its own size.
    assert!(at(base().effort(60).lossy_overrides(150, 0, 0)) == at(base().effort(80)));
    let (w, h, jpeg, _) = image_of(&at(base().effort(40).lossy_overrides(0, 0, 85)));
    assert_eq!((w, h, jpeg), (600, 600, true));
    assert!(at(base().effort(100).engine_effort(Some(100))) == lossless[2]);
    // The smallest lossless size is reached without loss; one byte less takes effort 60.
    let least = lossless.iter().map(Vec::len).min().unwrap() as u64;
    let p = gpress::optimize_to_size(&pdf, least, &base()).unwrap();
    assert!(p.reached && p.effort.level() <= 40 && p.result.data.len() as u64 == least);
    let p = gpress::optimize_to_size(&pdf, least - 1, &base()).unwrap();
    assert_eq!((p.effort.level(), p.reached), (60, true));
    assert!(p.result.data == at(base().effort(60)));
}

/// The rules of the step are settings, each reached by its name (`lossy_` and the name in the step's table).
/// Each at its default, set by hand, gives the bytes of options that never set one; each at either end of its
/// range gives a file that reads back and is not larger than the lossless output; and a rule that is moved
/// does what its name says.
#[cfg(feature = "jpeg")]
#[test]
fn the_rules_are_settings_and_their_defaults_change_nothing() {
    // A photo of 300 by 300 at 600 pixels an inch (object 4) and line art of 600 by 600 at 1,200 (object 6).
    let art = image(600, 600, "DeviceGray", 8, "", &line_art(600, 600));
    let shot = image(300, 300, "DeviceRGB", 8, "", &photo(300, 300, 3));
    let pdf = page(shot, 36, "/Im1 6 0 R", "q 36 0 0 36 100 20 cm /Im1 Do Q", &[art]);
    let base = |effort: u8| Options::default().threads(1).effort(effort);
    let at = |o: Options| gpress::optimize(&pdf, &o).expect("optimize").data;
    let set = |mut o: Options, rules: &str| {
        o.set_rules(rules).expect(rules);
        o
    };
    let with = |effort: u8, rules: &str| at(set(base(effort), rules));
    // The size of an image (object `n`) of an output, and whether it is a JPEG.
    let image_at = |out: &[u8], n: u32| {
        let doc = gpress::pdf::xref::open(out).expect("readable");
        let Obj::Stream(s) = doc.load(n).expect("image object").1 else { panic!("not a stream") };
        let jpeg = chain(&s.dict).expect("chain").iter().any(|st| st.name == b"DCTDecode");
        (s.dict.get(b"Width").and_then(|v| v.as_int()).unwrap(), jpeg)
    };
    let rules: Vec<_> = gpress::LossyRules::RULES.iter().map(|r| (format!("lossy_{}", r.0), r.1, r.2, r.3)).collect();
    let lossless = at(base(40));
    let defaults = rules.iter().map(|r| format!("{}={}", r.0, r.1)).collect::<Vec<_>>().join(",");
    for effort in [60, 80, 100] {
        let plain = at(base(effort));
        assert!(plain.len() < lossless.len(), "effort {effort}");
        assert!(with(effort, &defaults) == plain, "effort {effort}");
    }
    // The defaults at effort 100: the photo a JPEG at 150 pixels an inch, the line art at 300.
    let plain = at(base(100));
    assert_eq!((image_at(&plain, 4), image_at(&plain, 6)), ((75, true), (150, false)));
    // The lowest and the highest value of every rule, and each rule alone at its default.
    for (name, default, lowest, highest) in &rules {
        assert!(with(100, &format!("{name}={default}")) == plain, "{name}");
        for end in [lowest, highest] {
            let out = with(100, &format!("{name}={end}"));
            assert!(out.len() <= lossless.len(), "{name} at {end}: {} of {}", out.len(), lossless.len());
            let ((w4, jpeg4), (w6, jpeg6)) = (image_at(&out, 4), image_at(&out, 6));
            assert!((1..=300).contains(&w4) && (1..=600).contains(&w6) && !jpeg6, "{name} at {end}");
            if jpeg4 {
                let pic = gpress::jpeg::decode(&image_of(&out).3, 1 << 30, gpress::clock::Deadline::NONE);
                assert_eq!(pic.map(|p| p.width as i64).ok(), Some(w4), "{name} at {end}");
            } else {
                assert_eq!(image_of(&out).3.len() as i64, w4 * w4 * 3, "{name} at {end}");
            }
        }
    }
    // A rule moved. The floors go down only as far as their ranges (72 and 150), and only when asked.
    let low = |rules: &str| {
        let out = at(set(base(40).lossy(Some(Lossy { photo_ppi: 1, art_ppi: 1, ..Lossy::level(3) })), rules));
        (image_at(&out, 4).0, image_at(&out, 6).0)
    };
    assert_eq!(low(""), (75, 150));
    assert_eq!(low("lossy_photo_floor=72,lossy_art_floor=150"), (36, 75));
    assert_eq!(low("lossy_photo_floor=100,lossy_art_floor=200"), (50, 100));
    // An image of at most `min_pixels` pixels is not touched: the photo has 90,000.
    assert_eq!(image_at(&with(100, "lossy_min_pixels=90000"), 4), (300, false));
    assert_eq!(image_at(&with(100, "lossy_min_pixels=89999"), 4), (75, true));
    // A walk that may read one operator, or make one draw, measures nothing: the lossless output.
    assert!(with(100, "lossy_walk_operators=1") == lossless && with(100, "lossy_walk_draws=1") == lossless);
    // A floor of 99 dB no JPEG meets, over the image or in its worst block: the photo stays as it is, the line
    // art is still resampled.
    for rule in ["lossy_min_psnr=99", "lossy_min_block_psnr=99"] {
        let strict = with(100, rule);
        assert_eq!((image_at(&strict, 4), image_at(&strict, 6)), ((300, false), (150, false)), "{rule}");
    }
    // A margin of 500 percent: neither is shown at five times its floor, so neither is resampled, and the photo
    // becomes a JPEG of its own size.
    let wide = with(100, "lossy_resample_margin=500");
    assert_eq!((image_at(&wide, 4), image_at(&wide, 6)), ((300, true), (600, false)));
    // The line art resampled is a third of its lossless size: with a saving of 80 percent asked it stays.
    let keen = with(100, "lossy_min_saving=80");
    assert_eq!((image_at(&keen, 4), image_at(&keen, 6)), ((75, true), (600, false)));
    // The quality of effort 100 is 75: a least quality of 95 is a larger JPEG, a most of 60 a smaller one.
    let size = |out: &[u8]| image_of(out).3.len();
    assert!(size(&with(100, "lossy_quality_min=95")) > size(&plain));
    assert!(size(&with(100, "lossy_quality_max=60")) < size(&plain));
    // The photo an inch square is 1.07 percent of its page. With `scan_cover` at 1 that is a scan, which has
    // the line-art floor of 300 pixels an inch: its own resolution, so it keeps its size.
    let pdf = page(image(300, 300, "DeviceRGB", 8, "", &photo(300, 300, 3)), 72, "", "", &[]);
    let plain = gpress::optimize(&pdf, &base(100)).unwrap().data;
    let scan = gpress::optimize(&pdf, &set(base(100), "lossy_scan_cover=1")).unwrap().data;
    assert_eq!((image_at(&plain, 4), image_at(&scan, 4)), ((150, true), (300, true)));

    // The smallest images `min_pixels` lets in, in odd shapes and blank ones too, with every floor at its
    // lowest and any JPEG taken: still a file that reads back and is not larger than the lossless one.
    let loose = set(
        base(40).lossy(Some(Lossy { photo_ppi: 1, art_ppi: 1, ..Lossy::level(3) })),
        "lossy_min_pixels=10000,lossy_photo_floor=72,lossy_art_floor=150,lossy_resample_margin=100,\
         lossy_quality_min=1,lossy_min_psnr=1,lossy_min_block_psnr=1",
    );
    for (w, h) in [(101, 100), (64, 200), (400, 26), (3, 4000)] {
        let mut kinds = vec![
            ("DeviceRGB", photo(w, h, 3)),
            ("DeviceGray", photo(w, h, 1)),
            ("DeviceRGB", vec![255; w * h * 3]),
            ("DeviceGray", vec![0; w * h]),
        ];
        if w >= 20 {
            kinds.push(("DeviceGray", line_art(w, h)));
        }
        for (k, (cs, px)) in kinds.into_iter().enumerate() {
            // Shown a point square: thousands of pixels an inch.
            let pdf = page(image(w, h, cs, 8, "", &px), 1, "", "", &[]);
            let lossless = gpress::optimize(&pdf, &base(40)).expect("optimize").data;
            let out = gpress::optimize(&pdf, &loose).expect("optimize").data;
            assert!(out.len() <= lossless.len(), "{w} by {h}, {k}: {} of {}", out.len(), lossless.len());
            let (tw, th, jpeg, bytes) = image_of(&out);
            assert!((1..=w as i64).contains(&tw) && (1..=h as i64).contains(&th), "{w} by {h}, {k}");
            if jpeg {
                let pic = gpress::jpeg::decode(&bytes, 1 << 30, gpress::clock::Deadline::NONE).expect("decodes");
                assert_eq!((pic.width as i64, pic.height as i64), (tw, th), "{w} by {h}, {k}");
            } else {
                assert_eq!(bytes.len() as i64, tw * th * if cs == "DeviceRGB" { 3 } else { 1 }, "{w} by {h}, {k}");
            }
        }
    }
}

/// The command line takes rules by name, `--rule NAME=VALUE`, before or after the file names, once with commas
/// or again: the output is the library's with those rules set. A name that is no rule, or a value outside the
/// rule's range, is an error (exit 5) and no file.
#[cfg(feature = "jpeg")]
#[test]
fn the_rule_flag_sets_the_library_rules() {
    let dir = std::env::temp_dir().join(format!("gpress-lossy-rules-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (i, o) = (dir.join("in.pdf"), dir.join("out.pdf"));
    let pdf = page(image(300, 300, "DeviceRGB", 8, "", &photo(300, 300, 3)), 36, "", "", &[]);
    std::fs::write(&i, &pdf).unwrap();
    let cli = |before: &[&str], after: &[&str]| {
        let _ = std::fs::remove_file(&o);
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_gpress"));
        let st = c.arg("optimize").args(before).arg(&i).arg(&o).args(["--threads", "1", "--effort", "100"]).args(after);
        (st.output().unwrap().status.code(), std::fs::read(&o).ok())
    };
    let lib = |rules: &str| {
        let mut opt = Options::default().threads(1).effort(100);
        opt.set_rules(rules).expect(rules);
        Some(gpress::optimize(&pdf, &opt).unwrap().data)
    };
    let plain = lib("");
    assert_eq!(cli(&[], &[]), (Some(0), plain.clone()));
    // Every rule by its name, at its default and at the ends of its range.
    for (name, default, lowest, highest) in gpress::Options::rules() {
        assert_eq!(cli(&["--rule", &format!("{name}={default}")], &[]), (Some(0), plain.clone()), "{name}");
        let ends = format!("{name}={lowest},{name}={highest}");
        assert_eq!(cli(&[], &["--rule", &ends]), (Some(0), lib(&format!("{name}={highest}"))), "{name}");
        if let Some(under) = lowest.checked_sub(1) {
            assert_eq!(cli(&["--rule", &format!("{name}={under}")], &[]), (Some(5), None), "{name}");
        }
    }
    // Rules that change this file's output: before and after the file names, with a comma and with the flag again.
    let kept = lib("lossy_min_pixels=90000");
    assert!(kept != plain);
    assert_eq!(cli(&["--rule", "lossy_min_pixels=90000"], &[]), (Some(0), kept.clone()));
    assert_eq!(cli(&[], &["--rule", "lossy_min_pixels=90000"]), (Some(0), kept));
    let strict = lib("lossy_min_psnr=99,lossy_quality_tries=1");
    assert_eq!(cli(&["--rule", "lossy_min_psnr=99,lossy_quality_tries=1"], &[]), (Some(0), strict.clone()));
    assert_eq!(cli(&["--rule", "lossy_min_psnr=99"], &["--rule", "lossy_quality_tries=1"]), (Some(0), strict));
    // What is not a rule and its value: an error, and no file.
    for bad in ["lossy_min_pixels", "lossy_min_pixels=", "lossy_min_pixels=x", "min_pixels=90000", "lossy_nothing=1"] {
        assert_eq!(cli(&["--rule", bad], &[]), (Some(5), None), "{bad}");
    }
    assert_eq!(cli(&[], &["--rule"]).0, Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}
