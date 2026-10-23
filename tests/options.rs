//! The knobs of `pdf::rewrite::Options` that change what the safety gate and the stream candidates do:
//! `respect_conformance` and `flate_over_jpeg`, each tried with its non-default value.

use gpress::pdf::object::Obj;
use gpress::pdf::rewrite::{optimize, Options, Outcome};
use gpress::pdf::xref;

#[cfg(feature = "jpeg")]
const JPEG: &[u8] = include_bytes!("data/jpeg/base444.jpg");

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut b = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    b.extend_from_slice(data);
    b.extend_from_slice(b"\nendstream");
    b
}

/// A classic PDF from the bodies of objects 1, 2, ... (without "obj"/"endobj").
fn pdf(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut d = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offs.push(d.len());
        d.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        d.extend_from_slice(body);
        d.extend_from_slice(b"\nendobj\n");
    }
    let x = d.len();
    d.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offs {
        d.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    d.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    d
}

fn content() -> Vec<u8> {
    b"BT /F1 12 Tf 10 10 Td (hi) Tj ET\n".repeat(50)
}

fn load_stream(doc: &xref::Doc, n: u32) -> gpress::pdf::object::Stream {
    match doc.load(n).unwrap().1 {
        Obj::Stream(s) => *s,
        o => panic!("object {n} is {o:?}, not a stream"),
    }
}

#[test]
fn conformance_is_respected_unless_told_otherwise() {
    // A PDF/A file (XMP metadata claiming pdfaid:part) and a signed one.
    let xmp = b"<?xpacket begin='' id='W5M0MpCehiHzreSzNTczkc9d'?><x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description rdf:about='' xmlns:pdfaid='http://www.aiim.org/pdfa/ns/id/' pdfaid:part='1' pdfaid:conformance='B'/></rdf:RDF></x:xmpmeta><?xpacket end='w'?>";
    let pdfa = pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        stream("", &content()),
        stream("/Type /Metadata /Subtype /XML", xmp),
    ]);
    let signed = pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] /SigFlags 3 >> >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Annots [5 0 R] >>".to_vec(),
        stream("", &content()),
        b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Sig1) /Rect [0 0 0 0] /P 3 0 R /V 6 0 R >>".to_vec(),
        b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 100 200 100] /Contents <00> >>".to_vec(),
    ]);
    for (doc, why) in [(&pdfa, "PDF/A"), (&signed, "digitally signed")] {
        let r = optimize(doc, &Options::effort(100));
        assert!(matches!(&r.outcome, Outcome::Untouched(w) if w == why), "{:?}", r.outcome);
        assert_eq!(r.data, *doc);
        let r = optimize(doc, &Options { respect_conformance: false, ..Options::effort(100) });
        assert!(matches!(r.outcome, Outcome::Smaller(_)), "{:?}", r.outcome);
        assert!(r.data.len() < doc.len());
        // Every object is still there by value (the metadata stream is kept as it is). Numbers the input
        // doesn't use are the output's own (its object and cross-reference streams).
        let (before, out) = (xref::open(doc).unwrap(), xref::open(&r.data).unwrap());
        for n in 1..=6 {
            match before.load(n) {
                Ok((_, Obj::Stream(s))) => {
                    let s2 = load_stream(&out, n);
                    assert_eq!(
                        xref::decode_fully(&s.dict, &s.data, 1 << 20),
                        xref::decode_fully(&s2.dict, &s2.data, 1 << 20)
                    );
                }
                Ok((_, o)) => assert_eq!(out.load(n).unwrap().1, o),
                Err(_) => {}
            }
        }
    }
}

#[test]
#[cfg(feature = "jpeg")]
fn flate_goes_in_front_of_a_jpeg_only_when_asked() {
    // A JPEG with a 4 KB comment of one letter: our Flate shrinks it far more than 1%, so with the default it
    // goes behind /FlateDecode; without `flate_over_jpeg` the chain stays a bare /DCTDecode, with the JPEG's
    // tables optimised either way.
    let mut jpeg = JPEG[..2].to_vec();
    jpeg.extend_from_slice(&[0xFF, 0xFE, 0x0F, 0xA2]);
    jpeg.extend_from_slice(&[b'A'; 4000]);
    jpeg.extend_from_slice(&JPEG[2..]);
    let doc = pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        stream("", b"q 75 0 0 53 10 10 cm /Im1 Do Q\n"),
        stream("/Type /XObject /Subtype /Image /Width 75 /Height 53 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode", &jpeg),
    ]);
    let with = optimize(&doc, &Options::effort(100));
    let without = optimize(&doc, &Options { flate_over_jpeg: false, ..Options::effort(100) });
    assert!(matches!(with.outcome, Outcome::Smaller(_)) && matches!(without.outcome, Outcome::Smaller(_)));
    assert!(with.data.len() < without.data.len());
    let s = load_stream(&xref::open(&with.data).unwrap(), 5);
    assert_eq!(
        s.dict.get(b"Filter"),
        Some(&Obj::Array(vec![Obj::Name(b"FlateDecode".to_vec()), Obj::Name(b"DCTDecode".to_vec())])),
        "{:?}",
        s.dict
    );
    let s = load_stream(&xref::open(&without.data).unwrap(), 5);
    assert_eq!(s.dict.get(b"Filter"), Some(&Obj::Name(b"DCTDecode".to_vec())), "{:?}", s.dict);
    assert!(s.data.len() < jpeg.len() && s.data.starts_with(&[0xFF, 0xD8]));
    assert!(gpress::jpeg::equivalent(&jpeg, &s.data));
}
