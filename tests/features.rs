//! Builds without a part of the engine (the Cargo features `archive`, `jpeg`, `fax` and `encryption`; see
//! `Cargo.toml`). Each test runs in the builds it is about: with a feature left out, a call that needs it
//! returns `Error::NeedsFeature` naming it, an `optimize` step that needs it is skipped (the output is the one
//! the step's option switched off gives), an encrypted file is left untouched with the reason, and everything
//! else still works. The tests for the builds with the feature show that the same files do use it.
//!
//! The fixtures in tests/data/features were made by a build with every feature: `fax.pdf`, a page whose CCITT
//! image becomes JBIG2, and `fax.gpz`, its archive (with a fax part), both at effort 0; `r3_rc4_128.gpz`, the
//! archive of tests/data/enc/r3_rc4_128.pdf at effort 0 (its three streams stored decrypted).

use gpress::{compress, expand, is_archive, optimize, Error, Feature, Optimized, Options, Outcome, Report};

const FAX_PDF: &[u8] = include_bytes!("data/features/fax.pdf");
const FAX_GPZ: &[u8] = include_bytes!("data/features/fax.gpz");
const ENC_PDF: &[u8] = include_bytes!("data/enc/r3_rc4_128.pdf");
const ENC_GPZ: &[u8] = include_bytes!("data/features/r3_rc4_128.gpz");
const PLAIN_PDF: &[u8] = include_bytes!("data/format-v9.pdf");
const JPEG: &[u8] = include_bytes!("data/jpeg/base444.jpg");

fn quick() -> Options {
    Options::new().effort(0).threads(1)
}

fn report(r: &Optimized) -> &Report {
    match &r.outcome {
        Outcome::Smaller(s) => s,
        Outcome::Untouched(why) => panic!("left untouched: {why}"),
    }
}

/// A one-page PDF drawing one image (object 4) with the given dictionary entries and data.
fn image_pdf(image_dict: &str, data: &[u8]) -> Vec<u8> {
    let content = b"q 75 0 0 53 10 10 cm /Im0 Do Q\n";
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offs.push(out.len());
        out.extend_from_slice(body);
    };
    obj(&mut out, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    obj(&mut out, b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    obj(&mut out, b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>\nendobj\n");
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

fn jpeg_pdf() -> Vec<u8> {
    image_pdf("/Width 75 /Height 53 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode", JPEG)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn every_build_knows_its_features_and_optimizes_a_plain_pdf() {
    for f in Feature::ALL {
        let on = match f {
            Feature::Archive => cfg!(feature = "archive"),
            Feature::Jpeg => cfg!(feature = "jpeg"),
            Feature::Fax => cfg!(feature = "fax"),
            Feature::Encryption => cfg!(feature = "encryption"),
            Feature::Existing => cfg!(feature = "existing"),
        };
        assert_eq!(f.enabled(), on, "{f}");
    }
    // The archive format's constants are the same with or without the archive.
    assert_eq!(gpress::api::ARCHIVE_VERSION, 9);
    assert_eq!(Options::default().max_decoded, 256 << 20);
    assert!(is_archive(FAX_GPZ) && is_archive(ENC_GPZ) && !is_archive(FAX_PDF));
    // A PDF that needs no part a build may leave out comes out the same way in every build.
    let r = optimize(PLAIN_PDF, &quick()).unwrap();
    assert!(r.data.len() < PLAIN_PDF.len());
    let s = report(&r);
    assert!(s.streams_reencoded > 0 && s.encryption.is_empty(), "{s:?}");
    // The error names the feature, with its own code.
    let e = Error::NeedsFeature(Feature::Fax);
    assert_eq!(e.code(), 10);
    assert!(e.to_string().contains("`fax`"), "{e}");
}

/// Without the archive, compress and expand need it: the options and the input are checked first, as in
/// every build, then `NeedsFeature(Archive)`. Optimize is not touched by it.
#[test]
#[cfg(not(feature = "archive"))]
fn without_the_archive_compress_and_expand_say_they_need_it() {
    let opt = Options::default();
    for r in [compress(FAX_PDF, &opt).map(|c| c.data), expand(FAX_GPZ, &opt), expand(b"not an archive", &opt)] {
        let e = r.unwrap_err();
        assert_eq!(e, Error::NeedsFeature(Feature::Archive));
        assert_eq!(e.code(), 10);
        assert!(e.to_string().contains("`archive`"), "{e}");
    }
    assert!(matches!(compress(b"x", &Options::new().threads(65)), Err(Error::InvalidOptions(_))));
    assert_eq!(expand(&[0u8; 11], &Options::new().max_input(10)), Err(Error::TooLarge { size: 11, limit: 10 }));
    report(&optimize(FAX_PDF, &quick()).unwrap());
}

/// Without the JPEG step, no JPEG gets new tables: the output is the one `jpeg_tables` off gives, with the JPEG's
/// bytes as they were.
#[test]
#[cfg(not(feature = "jpeg"))]
fn without_jpeg_the_table_step_is_skipped() {
    let pdf = jpeg_pdf();
    for flate_over_jpeg in [false, true] {
        let opt = quick().flate_over_jpeg(flate_over_jpeg);
        let on = optimize(&pdf, &opt).unwrap();
        let off = optimize(&pdf, &opt.clone().jpeg_tables(false)).unwrap();
        assert_eq!(report(&on).jpegs_retabled, 0);
        assert_eq!(on.data, off.data);
        if !flate_over_jpeg {
            assert!(contains(&on.data, JPEG));
        }
    }
}

#[test]
#[cfg(feature = "jpeg")]
fn with_jpeg_the_same_file_gets_new_tables() {
    let r = optimize(&jpeg_pdf(), &quick().flate_over_jpeg(false)).unwrap();
    assert_eq!(report(&r).jpegs_retabled, 1);
    assert!(!contains(&r.data, JPEG));
}

/// Without the fax codecs, no CCITT image becomes JBIG2 (the output is the one `jbig2` off gives); the archive
/// stores fax streams as they are, and an archive with fax parts, made by a build with them, needs them.
#[test]
#[cfg(not(feature = "fax"))]
fn without_fax_the_jbig2_step_is_skipped_and_fax_parts_need_it() {
    let on = optimize(FAX_PDF, &quick()).unwrap();
    let off = optimize(FAX_PDF, &quick().jbig2(false)).unwrap();
    assert_eq!(report(&on).fax_to_jbig2, 0);
    assert_eq!(on.data, off.data);
    assert!(!contains(&on.data, b"JBIG2Decode"));
    #[cfg(feature = "archive")]
    {
        let e = expand(FAX_GPZ, &Options::default()).unwrap_err();
        assert_eq!(e, Error::NeedsFeature(Feature::Fax));
        assert!(e.to_string().contains("`fax`"), "{e}");
        let c = compress(FAX_PDF, &quick()).unwrap();
        assert_eq!(c.fax_images, 0);
        assert_eq!(expand(&c.data, &Options::default()).unwrap(), FAX_PDF);
    }
}

#[test]
#[cfg(feature = "fax")]
fn with_fax_the_same_files_use_it() {
    let r = optimize(FAX_PDF, &quick()).unwrap();
    assert_eq!(report(&r).fax_to_jbig2, 1);
    assert!(contains(&r.data, b"JBIG2Decode"));
    #[cfg(feature = "archive")]
    {
        assert_eq!(expand(FAX_GPZ, &Options::default()).unwrap(), FAX_PDF);
        assert_eq!(compress(FAX_PDF, &quick()).unwrap().fax_images, 1);
    }
}

/// Without encryption, an encrypted file is left untouched, with a reason that names the feature; the archive
/// stores its streams as they are, and an archive with decrypted streams, made by a build with it, needs it.
#[test]
#[cfg(not(feature = "encryption"))]
fn without_encryption_encrypted_files_are_left_untouched_and_say_why() {
    let r = optimize(ENC_PDF, &quick()).unwrap();
    match &r.outcome {
        Outcome::Untouched(why) => assert!(why.starts_with("encrypted: ") && why.contains("`encryption`"), "{why}"),
        Outcome::Smaller(s) => panic!("rewritten: {s:?}"),
    }
    assert_eq!(r.data, ENC_PDF);
    #[cfg(feature = "archive")]
    {
        let e = expand(ENC_GPZ, &Options::default()).unwrap_err();
        assert_eq!(e, Error::NeedsFeature(Feature::Encryption));
        assert!(e.to_string().contains("`encryption`"), "{e}");
        let c = compress(ENC_PDF, &quick()).unwrap();
        assert_eq!(c.streams_decrypted, 0);
        assert_eq!(expand(&c.data, &Options::default()).unwrap(), ENC_PDF);
    }
}

#[test]
#[cfg(feature = "encryption")]
fn with_encryption_the_same_files_use_it() {
    let r = optimize(ENC_PDF, &quick()).unwrap();
    assert_eq!(report(&r).encryption, "R3/RC4");
    #[cfg(feature = "archive")]
    {
        assert_eq!(expand(ENC_GPZ, &Options::default()).unwrap(), ENC_PDF);
        assert_eq!(compress(ENC_PDF, &quick()).unwrap().streams_decrypted, 3);
    }
}
