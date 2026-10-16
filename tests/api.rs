//! The library boundary: every knob of `gpress::Options` reaches the engine and changes what it does,
//! proven one knob at a time on the mixed test document (a JPEG, a fax image, a Type 1 font, a zlib
//! content stream; plain and encrypted). The output never depends on the thread count or on a time budget
//! that doesn't run out. A build without the archive or the JPEG step runs the tests that don't need it.

mod common;

use common::*;
#[cfg(feature = "archive")]
use gpress::Compressed;
use gpress::{compress, expand, optimize, Error, Options, Outcome, Report};
use std::time::Duration;

fn base() -> Options {
    Options::default().threads(1)
}

fn report(r: &gpress::Optimized) -> &Report {
    match &r.outcome {
        Outcome::Smaller(s) => s,
        Outcome::Untouched(why) => panic!("untouched: {why}"),
    }
}

fn untouched<'a>(r: &'a gpress::Optimized, input: &[u8]) -> &'a str {
    assert_eq!(r.data, input, "an untouched file is the input itself");
    match &r.outcome {
        Outcome::Untouched(why) => why,
        Outcome::Smaller(s) => panic!("made smaller: {s:?}"),
    }
}

#[cfg(feature = "archive")]
fn pack(input: &[u8], opt: &Options) -> Compressed {
    let c = compress(input, opt).unwrap();
    assert_eq!(expand(&c.data, opt).unwrap(), input, "every archive restores exactly");
    c
}

#[test]
fn effort_changes_the_work_and_every_level_restores() {
    let pdf = plain_pdf();
    let quick = optimize(&pdf, &base().effort(0)).unwrap();
    let full = optimize(&pdf, &base().effort(100)).unwrap();
    assert!(report(&quick).streams_reencoded > 0 && report(&full).streams_reencoded > 0);
    assert_ne!(quick.data, full.data, "effort 0 and 100 use different Flate settings");
    assert!(full.data.len() < pdf.len() && quick.data.len() < pdf.len());
    // The archive: below effort 50, data that looks compressed already (a stream of random bytes here) is
    // coded plainly, so the bytes differ; both restore.
    #[cfg(feature = "archive")]
    {
        let mut seed = 0x9E37_79B9u32;
        let random: Vec<u8> = (0..100_000).map(|_| next(&mut seed) as u8).collect();
        let mut objs = objects();
        objs.push((12, "/Type /XObject /Subtype /Image /Width 400 /Height 250 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode".into(), Some(random)));
        let doc = rebuild(&objs);
        let a = pack(&doc, &base().effort(0));
        let b = pack(&doc, &base().effort(100));
        assert_ne!(a.data, b.data);
        assert!(b.data.len() < doc.len());
    }
}

#[test]
fn threads_never_change_the_output() {
    let pdf = plain_pdf();
    let one = optimize(&pdf, &base().threads(1)).unwrap().data;
    let two = optimize(&pdf, &base().threads(2)).unwrap().data;
    let auto = optimize(&pdf, &base().threads(0)).unwrap().data;
    assert!(one == two && one == auto);
    #[cfg(feature = "archive")]
    assert_eq!(pack(&pdf, &base().threads(1)).data, pack(&pdf, &base().threads(2)).data);
    assert!(matches!(optimize(&pdf, &base().threads(65)), Err(Error::InvalidOptions(_))));
    assert!(optimize(&pdf, &base().threads(64)).is_ok());
}

#[test]
fn max_input_refuses_before_reading() {
    let pdf = plain_pdf();
    let opt = base().max_input(pdf.len() - 1);
    let e = Error::TooLarge { size: pdf.len(), limit: pdf.len() - 1 };
    assert_eq!(optimize(&pdf, &opt).unwrap_err(), e);
    assert_eq!(compress(&pdf, &opt).unwrap_err(), e);
    assert_eq!(expand(&pdf, &opt).unwrap_err(), e);
    assert!(optimize(&pdf, &base().max_input(pdf.len())).is_ok());
}

#[test]
fn max_stream_keeps_the_streams_over_it() {
    let pdf = plain_pdf();
    let all = optimize(&pdf, &base()).unwrap();
    let small = optimize(&pdf, &base().max_stream(100)).unwrap();
    let (a, s) = (report(&all), report(&small));
    assert_eq!(a.streams, s.streams);
    assert!(s.streams_kept > a.streams_kept, "{a:?} / {s:?}");
    assert!(s.streams_reencoded < a.streams_reencoded);
    assert_eq!(s.jpegs_retabled, 0, "a JPEG over the limit keeps its bytes");
}

#[test]
#[cfg(feature = "archive")]
fn max_stream_archive_stores_the_streams_over_it_as_they_are() {
    let pdf = plain_pdf();
    let all = pack(&pdf, &base());
    let small = pack(&pdf, &base().max_stream_archive(100));
    assert!(all.streams_rebuilt > 0 && all.rebuilt_bytes > 0, "{all:?}");
    assert_eq!(small.streams_rebuilt, 0, "{small:?}");
    assert_eq!(small.rebuilt_bytes, 0);
    assert!(small.data.len() > all.data.len());
}

#[test]
#[cfg(feature = "archive")]
fn max_len_refuses_a_declared_length_before_decoding() {
    let pdf = plain_pdf();
    let c = pack(&pdf, &base());
    let declared = pdf.len() as u64;
    let e = expand(&c.data, &base().max_len(declared - 1)).unwrap_err();
    assert_eq!(e, Error::DeclaredTooLarge { declared, limit: declared - 1 });
    assert_eq!(e.code(), 7);
    assert_eq!(expand(&c.data, &base().max_len(declared)).unwrap(), pdf);
    // A crafted header declaring 1 GiB over nothing: refused at once under a limit, as damaged without one.
    let mut crafted = c.data[..5].to_vec();
    crafted.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x04]);
    crafted.extend_from_slice(&[0u8; 40]);
    assert!(matches!(expand(&crafted, &base().max_len(1 << 20)), Err(Error::DeclaredTooLarge { .. })));
    assert!(matches!(expand(&crafted, &base()), Err(Error::Damaged(_))));
}

#[test]
#[cfg(feature = "archive")]
fn max_decoded_bounds_both_sides() {
    // The mixed document's streams decode to about 70 KB in all. A budget below that stores some of
    // them as they are (in file order; the archive is larger and still restores); the header declares the
    // total, and expand refuses an archive declaring more than its own budget before decoding anything.
    let pdf = plain_pdf();
    let all = pack(&pdf, &base());
    assert!(all.declared_decoded > 10_000 && all.declared_decoded <= Options::default().max_decoded, "{all:?}");
    let some = pack(&pdf, &base().max_decoded(all.declared_decoded / 2));
    assert!(
        some.declared_decoded < all.declared_decoded && some.declared_decoded <= all.declared_decoded / 2,
        "{some:?}"
    );
    assert!(some.streams_rebuilt < all.streams_rebuilt && some.data.len() > all.data.len());
    let none = pack(&pdf, &base().max_decoded(0));
    assert_eq!((none.declared_decoded, none.streams_rebuilt, none.streams_lepton), (0, 0, 0));
    assert!(none.data.len() > some.data.len());
    let e = expand(&all.data, &base().max_decoded(all.declared_decoded - 1)).unwrap_err();
    assert_eq!(e, Error::DecodedTooLarge { declared: all.declared_decoded, limit: all.declared_decoded - 1 });
    assert_eq!(e.code(), 9);
    assert_eq!(expand(&all.data, &base().max_decoded(all.declared_decoded)).unwrap(), pdf);
    assert_eq!(expand(&some.data, &base().max_decoded(some.declared_decoded)).unwrap(), pdf);
    assert_eq!(expand(&none.data, &base().max_decoded(0)).unwrap(), pdf);
    // The same bytes with two threads.
    assert_eq!(compress(&pdf, &base().max_decoded(all.declared_decoded / 2).threads(2)).unwrap().data, some.data);
    // The defaults: 256 MiB of decoded bytes, and a restored length no larger than the default input.
    let d = Options::default();
    assert_eq!((d.max_decoded, d.max_len, d.max_input as u64), (256 << 20, 1 << 30, 1 << 30));
}

#[test]
fn objects_per_stream_sets_the_object_streams() {
    let pdf = plain_pdf();
    let many = optimize(&pdf, &base().objects_per_stream(1)).unwrap();
    let few = optimize(&pdf, &base().objects_per_stream(200)).unwrap();
    assert!(report(&many).object_streams > report(&few).object_streams, "{:?} / {:?}", report(&many), report(&few));
    assert!(matches!(optimize(&pdf, &base().objects_per_stream(0)), Err(Error::InvalidOptions(_))));
}

#[test]
fn respect_conformance_leaves_pdfa_alone_unless_told_otherwise() {
    // The mixed document claiming PDF/A in its XMP metadata.
    let xmp = b"<?xpacket begin='' id='W5M0MpCehiHzreSzNTczkc9d'?><x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description rdf:about='' xmlns:pdfaid='http://www.aiim.org/pdfa/ns/id/' pdfaid:part='1' pdfaid:conformance='B'/></rdf:RDF></x:xmpmeta><?xpacket end='w'?>";
    let mut objs = objects();
    objs[0].1 = "/Type /Catalog /Pages 2 0 R /Metadata 12 0 R".into();
    objs.push((12, "/Type /Metadata /Subtype /XML".into(), Some(xmp.to_vec())));
    let pdfa = rebuild(&objs);
    let kept = optimize(&pdfa, &base()).unwrap();
    assert_eq!(untouched(&kept, &pdfa), "PDF/A");
    let rewritten = optimize(&pdfa, &base().respect_conformance(false)).unwrap();
    assert!(report(&rewritten).streams_reencoded > 0);
    assert!(rewritten.data.len() < pdfa.len());
}

#[test]
#[cfg(feature = "jpeg")]
fn flate_over_jpeg_is_a_choice() {
    // A JPEG with a 4 KB comment of one letter: Flate in front of it saves far more than 1%.
    let mut jpeg = JPEG[..2].to_vec();
    jpeg.extend_from_slice(&[0xFF, 0xFE, 0x0F, 0xA2]);
    jpeg.extend_from_slice(&[b'A'; 4000]);
    jpeg.extend_from_slice(&JPEG[2..]);
    // The mixed document with that JPEG in place of its own.
    let mut objs = objects();
    assert_eq!(objs[6].0, 7, "object 7 is the JPEG");
    objs[6].2 = Some(jpeg.clone());
    let doc = rebuild(&objs);
    let with = optimize(&doc, &base()).unwrap();
    let without = optimize(&doc, &base().flate_over_jpeg(false)).unwrap();
    assert!(with.data.len() < without.data.len(), "{} / {}", with.data.len(), without.data.len());
    assert_eq!(report(&with).jpegs_retabled, 1);
    assert_eq!(report(&without).jpegs_retabled, 1);
    let s = stream(&gpress::pdf::xref::open(&without.data).unwrap(), 7);
    assert_eq!(s.dict.name(b"Filter"), Some(&b"DCTDecode"[..]), "{:?}", s.dict);
}

/// A classic PDF from (object number, dictionary, stream data) triples.
fn rebuild(objs: &[(u32, String, Option<Vec<u8>>)]) -> Vec<u8> {
    let mut d = b"%PDF-1.6\n".to_vec();
    let mut offs = Vec::new();
    for (n, dict, data) in objs {
        offs.push((*n, d.len()));
        d.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        match data {
            Some(x) => {
                d.extend_from_slice(format!("<< {dict} /Length {} >>\nstream\n", x.len()).as_bytes());
                d.extend_from_slice(x);
                d.extend_from_slice(b"\nendstream");
            }
            None => d.extend_from_slice(format!("<< {dict} >>").as_bytes()),
        }
        d.extend_from_slice(b"\nendobj\n");
    }
    let x = d.len();
    let size = objs.iter().map(|o| o.0).max().unwrap() + 1;
    d.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offs.iter().find(|o| o.0 == n) {
            Some((_, off)) => d.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => d.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    d.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes());
    d
}

#[test]
#[cfg(feature = "jpeg")]
fn jpeg_tables_and_jbig2_each_have_a_switch_and_a_count() {
    let pdf = plain_pdf();
    let all = optimize(&pdf, &base()).unwrap();
    assert_eq!((report(&all).jpegs_retabled, report(&all).fax_to_jbig2), (1, 1), "{:?}", report(&all));
    let no_jpeg = optimize(&pdf, &base().jpeg_tables(false)).unwrap();
    assert_eq!((report(&no_jpeg).jpegs_retabled, report(&no_jpeg).fax_to_jbig2), (0, 1));
    let no_fax = optimize(&pdf, &base().jbig2(false)).unwrap();
    assert_eq!((report(&no_fax).jpegs_retabled, report(&no_fax).fax_to_jbig2), (1, 0));
    // The JPEG and the fax image keep their bytes when their step is off.
    let (a, b) = (gpress::pdf::xref::open(&pdf).unwrap(), gpress::pdf::xref::open(&no_jpeg.data).unwrap());
    assert_eq!(jpeg_bytes(&stream(&b, 7)), jpeg_bytes(&stream(&a, 7)));
    let b = gpress::pdf::xref::open(&no_fax.data).unwrap();
    assert_eq!(codec_bytes(&stream(&b, 8), b"CCITTFaxDecode"), codec_bytes(&stream(&a, 8), b"CCITTFaxDecode"));
    assert!(no_jpeg.data.len() > all.data.len() && no_fax.data.len() > all.data.len());
}

#[test]
fn rewrite_encrypted_and_decrypt_each_have_a_switch() {
    let pdf = encrypted_pdf(false);
    let kept = optimize(&pdf, &base().rewrite_encrypted(false)).unwrap();
    assert!(untouched(&kept, &pdf).starts_with("encrypted"));
    let rewritten = optimize(&pdf, &base()).unwrap();
    assert_eq!(report(&rewritten).encryption, "R3/RC4");
    assert!(rewritten.data.len() < pdf.len());
    #[cfg(feature = "archive")]
    {
        let decrypted = pack(&pdf, &base());
        let as_stored = pack(&pdf, &base().decrypt(false));
        assert!(decrypted.streams_decrypted > 0, "{decrypted:?}");
        assert_eq!(as_stored.streams_decrypted, 0, "{as_stored:?}");
        assert!(decrypted.data.len() < as_stored.data.len());
        assert!(decrypted.type1_fonts == 1 && decrypted.fax_images == 1, "{decrypted:?}");
    }
}

#[test]
#[cfg(feature = "archive")]
fn use_existing_libraries_is_a_switch_when_the_build_has_them() {
    let pdf = plain_pdf();
    let with = pack(&pdf, &base());
    let without = pack(&pdf, &base().use_existing_libraries(false));
    assert_eq!(without.streams_lepton, 0);
    if Options::has_existing_libraries() {
        assert_eq!(with.streams_lepton, 1, "{with:?}");
        assert_ne!(with.data, without.data);
        assert!(expand(&with.data, &base()).is_ok());
    } else {
        assert_eq!(with.streams_lepton, 0);
        assert_eq!(with.data, without.data);
    }
}

#[test]
fn a_time_budget_stops_the_work_and_changes_no_output() {
    let pdf = plain_pdf();
    let tiny = base().time_budget(Some(Duration::from_nanos(1)));
    let e = optimize(&pdf, &tiny).unwrap_err();
    assert!(matches!(e, Error::TimeBudget(_)), "{e}");
    assert_eq!(e.code(), 6);
    // A budget that doesn't run out: the same bytes as without one.
    let ample = base().time_budget(Some(Duration::from_secs(600)));
    assert_eq!(optimize(&pdf, &ample).unwrap().data, optimize(&pdf, &base()).unwrap().data);
    assert!(matches!(optimize(&pdf, &base().time_budget(Some(Duration::ZERO))), Err(Error::InvalidOptions(_))));
    // The archive the same way, and expand too: a budget that has passed gives the error and no file, an
    // ample one the same bytes.
    #[cfg(feature = "archive")]
    {
        let e = compress(&pdf, &tiny).unwrap_err();
        assert!(matches!(e, Error::TimeBudget(_)), "{e}");
        assert_eq!(compress(&pdf, &ample).unwrap().data, compress(&pdf, &base()).unwrap().data);
        let c = compress(&pdf, &base()).unwrap();
        let e = expand(&c.data, &tiny).unwrap_err();
        assert!(matches!(e, Error::TimeBudget(_)), "{e}");
        assert_eq!(expand(&c.data, &ample).unwrap(), pdf);
    }
}

/// Cut-off files and the bare header: every prefix of a small PDF, and of the mixed document in steps, goes
/// through all three operations without reaching the panic guard (`Error::Internal` is a bug, not a refusal).
/// An 8-byte "%PDF-1.7" once panicked in the search for "startxref".
#[test]
fn every_prefix_is_handled_without_a_bug() {
    let opt = base().effort(0);
    let small = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\nxref\n0 3\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n110\n%%EOF\n";
    let mixed = plain_pdf();
    let mut cases: Vec<&[u8]> = (0..=small.len()).map(|n| &small[..n]).collect();
    cases.extend((0..=mixed.len()).step_by(1009).map(|n| &mixed[..n]));
    // Cut just before, inside and just after every stream keyword of the mixed document.
    for (i, w) in mixed.windows(9).enumerate() {
        if w == b"endstream" || w.starts_with(b"stream\n") || w.starts_with(b"stream\r") {
            cases.extend([i.saturating_sub(1), i + 3, i + 9].iter().map(|&n| &mixed[..n.min(mixed.len())]));
        }
    }
    cases.push(&mixed[mixed.len() - 8..]);
    for input in cases {
        for (what, r) in [
            ("optimize", optimize(input, &opt).map(|_| ())),
            ("compress", compress(input, &opt).map(|_| ())),
            ("expand", expand(input, &opt).map(|_| ())),
        ] {
            if let Err(Error::Internal(why)) = r {
                panic!("{what} of a {}-byte prefix hit a bug: {why}", input.len());
            }
        }
    }
    // The 8-byte header alone, and every length of header-looking input up to 16 bytes.
    for n in 1..=16 {
        let input = &b"%PDF-1.7\n%%EOF\n\n"[..n];
        assert!(!matches!(optimize(input, &opt), Err(Error::Internal(_))), "{n} bytes");
        assert!(!matches!(compress(input, &opt), Err(Error::Internal(_))), "{n} bytes");
    }
}

#[test]
#[cfg(feature = "archive")]
fn a_damaged_archive_is_an_error_with_a_message_and_never_a_file() {
    let pdf = plain_pdf();
    let c = pack(&pdf, &base());
    for (k, bit) in [(c.data.len() / 3, 1u8), (c.data.len() / 2, 16), (c.data.len() - 2, 128)] {
        let mut bad = c.data.clone();
        bad[k] ^= bit;
        // Damaged, or (a flipped tag) a part this build can't read, or (byte 4) another version: an error
        // with a message, never a file.
        match expand(&bad, &base()) {
            Ok(out) => assert_eq!(out, pdf, "a damaged archive restored to something else"),
            Err(e) => {
                assert!(
                    matches!(e, Error::Damaged(_) | Error::NeedsExistingLibrary | Error::UnsupportedVersion(_)),
                    "{e}"
                );
                assert!(!e.to_string().is_empty());
            }
        }
    }
    assert!(matches!(expand(&c.data[..c.data.len() / 2], &base()), Err(Error::Damaged(_))));
}
