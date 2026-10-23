//! Every feature at once: an encrypted PDF (RC4 and AES) holding a JPEG, a CCITT fax image, a Type 1 font and
//! a zlib content stream. The normal PDF keeps the encryption and still gets optimal JPEG tables, JBIG2 and
//! our Flate; the gpress archive stores the streams decrypted, opens the font and codes the fax image, and
//! restores every byte. Both give the same bytes for any thread count. A build without the archive runs the
//! normal-PDF tests.

// Only in a build with the jpeg, fax and encryption features (Cargo.toml, [features]).
#![cfg(all(feature = "jpeg", feature = "fax", feature = "encryption"))]

mod common;

use common::*;
#[cfg(feature = "archive")]
use gpress::pack::{pack_with, unpack, PackOptions};
use gpress::pdf::object::Obj;
use gpress::pdf::rewrite::{optimize, Options, Outcome};
use gpress::pdf::xref;

/// Bits per byte of `x`'s byte histogram (the pack's measure of compressed-looking data).
fn entropy(x: &[u8]) -> f64 {
    let mut h = [0u64; 256];
    for &b in x {
        h[b as usize] += 1;
    }
    h.iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / x.len() as f64;
            -p * p.log2()
        })
        .sum()
}

#[test]
fn the_font_looks_random() {
    // The pack codes data with 7.9 bits per byte or more plainly below effort 50 (see `PackOptions`), unless
    // it is a font it opens first; the test font must be such data for the effort-0 checks to mean anything.
    let (font, _, _, _) = type1_font(FONT_GLYPHS);
    assert!(entropy(&font) >= 7.9, "{}", entropy(&font));
}

fn settings(effort: u8, threads: usize) -> Options {
    Options { threads, ..Options::effort(effort) }
}

#[test]
fn the_document_opens_decrypted() {
    for aes in [false, true] {
        let pdf = encrypted_pdf(aes);
        let doc = xref::open(&pdf).unwrap();
        let c = doc.crypt.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(c.describe(), if aes { "R4/AESV2" } else { "R3/RC4" });
        assert_eq!(stream(&doc, 7).data, JPEG, "the JPEG reads back decrypted");
        assert_eq!(
            xref::decode_fully(&stream(&doc, 6).dict, &stream(&doc, 6).data, 1 << 20).unwrap(),
            type1_font(FONT_GLYPHS).0
        );
        match doc.load(11).unwrap().1 {
            Obj::Dict(d) => assert_eq!(d.get(b"Title"), Some(&Obj::Str(b"Mixed test".to_vec()))),
            o => panic!("{o:?}"),
        }
    }
}

#[test]
fn normal_pdf_keeps_the_encryption_and_applies_every_step() {
    for aes in [false, true] {
        let pdf = encrypted_pdf(aes);
        let r = optimize(&pdf, &settings(100, 1));
        let stats = match r.outcome {
            Outcome::Smaller(s) => s,
            Outcome::Untouched(why) => panic!("untouched: {why}"),
            Outcome::TimedOut => panic!("timed out"),
        };
        assert_eq!(stats.encryption, if aes { "R4/AESV2" } else { "R3/RC4" });
        // The font program is encrypted bytes, which Flate can't shrink; the rest loses at least a quarter.
        let rest = pdf.len() - type1_font(FONT_GLYPHS).0.len();
        assert!(pdf.len() - r.data.len() > rest / 4, "{} -> {} (without the font: {rest})", pdf.len(), r.data.len());
        // The same encryption dictionary, /ID and key; the objects read back decrypted.
        let (a, b) = (xref::open(&pdf).unwrap(), xref::open(&r.data).unwrap());
        assert_eq!(a.crypt.as_ref().unwrap().as_ref().unwrap(), b.crypt.as_ref().unwrap().as_ref().unwrap());
        // The JPEG: other bytes (optimal tables), the same coded symbols.
        let (j0, j1) = (jpeg_bytes(&stream(&a, 7)), jpeg_bytes(&stream(&b, 7)));
        assert_ne!(j0, j1);
        assert!(j1.len() < j0.len());
        assert!(gpress::jpeg::equivalent(&j0, &j1));
        // The fax image: JBIG2 with the same pixels.
        let (f0, f1) = (stream(&a, 8), stream(&b, 8));
        assert_eq!(f1.dict.name(b"Filter"), Some(&b"JBIG2Decode"[..]), "{:?}", f1.dict);
        let img = gpress::fax::jbig2::read_pdf_stream(&f1.data, false, usize::MAX).unwrap();
        assert_eq!(img, scan(FAX_W, FAX_H));
        assert!(f1.data.len() * 100 <= f0.data.len() * 95);
        // The font program and the content: the same bytes behind our Flate.
        for n in [6, 9] {
            let (s0, s1) = (stream(&a, n), stream(&b, n));
            assert_eq!(
                xref::decode_fully(&s0.dict, &s0.data, 1 << 20),
                xref::decode_fully(&s1.dict, &s1.data, 1 << 20)
            );
            assert!(s1.data.len() <= s0.data.len());
        }
        // The same bytes with two threads, at every effort tried.
        for effort in [0, 50, 100] {
            let one = optimize(&pdf, &settings(effort, 1)).data;
            let two = optimize(&pdf, &settings(effort, 2)).data;
            assert_eq!(one, two, "effort {effort}");
            assert!(matches!(optimize(&pdf, &settings(effort, 1)).outcome, Outcome::Smaller(_)));
        }
    }
}

#[test]
#[cfg(feature = "archive")]
fn archive_stores_the_streams_decrypted_opened_and_coded() {
    for aes in [false, true] {
        let pdf = encrypted_pdf(aes);
        let opt = PackOptions { threads: 1, ..PackOptions::default() };
        let (packed, st) = pack_with(&pdf, &opt);
        assert!(st.decrypted >= 4, "{st:?}");
        assert_eq!(st.type1, 1, "{st:?}");
        assert_eq!(st.fax, 1, "{st:?}");
        assert!(st.rebuilt >= 2 && !st.fallback && !st.stored, "{st:?}");
        assert!(packed.len() * 2 < pdf.len(), "{} -> {}", pdf.len(), packed.len());
        assert_eq!(unpack(&packed).unwrap(), pdf);
        let (two, _) = pack_with(&pdf, &PackOptions { threads: 2, ..opt.clone() });
        assert_eq!(two, packed, "two threads give the same bytes");
        let (quick, qst) = pack_with(&pdf, &PackOptions { effort: 0, ..opt.clone() });
        assert!(qst.fax == 1 && qst.type1 == 1 && qst.decrypted >= 4, "{qst:?}");
        // Effort 0 codes compressed-looking data plainly, but opens the font first (it looks random only while
        // encrypted), so the pack is about as small.
        assert!(
            quick.len() * 100 <= packed.len() * 105,
            "effort 0: {} bytes, effort 100: {}",
            quick.len(),
            packed.len()
        );
        assert_eq!(unpack(&quick).unwrap(), pdf);
        // Without decryption the streams stay as stored, so neither the font nor the fax image is opened.
        let (plain, pst) = pack_with(&pdf, &PackOptions { decrypt: false, ..opt.clone() });
        assert!(pst.decrypted == 0 && pst.type1 == 0 && pst.fax == 0, "{pst:?}");
        assert!(plain.len() > packed.len());
        assert_eq!(unpack(&plain).unwrap(), pdf);
        // Damaged packs are refused or restore exactly, never anything else.
        for i in (25..packed.len()).step_by(97) {
            let mut bad = packed.clone();
            bad[i] ^= 1 << (i % 8);
            if let Ok(out) = unpack(&bad) {
                assert_eq!(out, pdf, "byte {i}: a damaged file restored to something else");
            }
        }
    }
}

#[test]
fn each_step_has_its_switch() {
    let pdf = encrypted_pdf(false);
    let base = settings(100, 1);
    // Encrypted files are left alone.
    let r = optimize(&pdf, &Options { rewrite_encrypted: false, ..base.clone() });
    assert!(matches!(&r.outcome, Outcome::Untouched(why) if why.starts_with("encrypted")), "{:?}", r.outcome);
    assert_eq!(r.data, pdf);
    // Without JBIG2 the fax image keeps its CCITT bytes; without the JPEG tables the JPEG keeps its bytes (both
    // behind our Flate, or not, as the sizes decide). The file is still made smaller and reads back.
    let r = optimize(&pdf, &Options { jbig2: false, jpeg_tables: false, ..base.clone() });
    assert!(matches!(r.outcome, Outcome::Smaller(_)), "{:?}", r.outcome);
    let (a, b) = (xref::open(&pdf).unwrap(), xref::open(&r.data).unwrap());
    let fax = codec_bytes(&stream(&a, 8), b"CCITTFaxDecode");
    assert_eq!(codec_bytes(&stream(&b, 8), b"CCITTFaxDecode"), fax);
    assert_eq!(jpeg_bytes(&stream(&b, 7)), jpeg_bytes(&stream(&a, 7)));
    // Each alone: the other step still runs.
    let r = optimize(&pdf, &Options { jbig2: false, ..base.clone() });
    let b = xref::open(&r.data).unwrap();
    assert_eq!(codec_bytes(&stream(&b, 8), b"CCITTFaxDecode"), fax);
    assert_ne!(jpeg_bytes(&stream(&b, 7)), jpeg_bytes(&stream(&a, 7)));
    let r = optimize(&pdf, &Options { jpeg_tables: false, ..base.clone() });
    let b = xref::open(&r.data).unwrap();
    assert_eq!(stream(&b, 8).dict.name(b"Filter"), Some(&b"JBIG2Decode"[..]));
    assert_eq!(jpeg_bytes(&stream(&b, 7)), jpeg_bytes(&stream(&a, 7)));
}

/// With PEYAJI_TEST_OUT set to a folder, writes the two documents and their normal-PDF outputs there for
/// the independent checks (verify_pdf.py, verify_render.py, qpdf).
#[test]
fn writes_the_files_for_independent_checks() {
    let Some(dir) = std::env::var_os("PEYAJI_TEST_OUT") else { return };
    let dir = std::path::PathBuf::from(dir);
    for (aes, name) in [(false, "mixed_rc4"), (true, "mixed_aes")] {
        let pdf = encrypted_pdf(aes);
        std::fs::write(dir.join(format!("{name}.pdf")), &pdf).unwrap();
        let r = optimize(&pdf, &settings(100, 2));
        assert!(matches!(r.outcome, Outcome::Smaller(_)));
        std::fs::write(dir.join(format!("{name}.opt.pdf")), &r.data).unwrap();
    }
}
