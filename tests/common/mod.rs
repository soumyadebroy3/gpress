//! The mixed test document shared by the integration tests: an encrypted PDF (RC4 or AES) holding a JPEG,
//! a CCITT fax image, a Type 1 font and a zlib content stream, plus readers for what came out. Making it
//! needs the `fax` and `encryption` features; without the archive its zlib streams are stored blocks (`flate`).

#![allow(dead_code)]

mod flate;

pub use flate::zlib;
use gpress::fax::ccitt::{self, row_changes, Gap, Image, Layout, Rule, Scheme};
#[cfg(feature = "archive")]
use gpress::pack::type1::encrypt;
use gpress::pdf::crypt::{self, Method};
use gpress::pdf::object::Obj;
use gpress::pdf::xref;

/// Type 1 encryption in place (`key`: 55665 for eexec, 4330 for charstrings), as the archive's
/// `gpress::pack::type1::encrypt` does it, for a build without the archive.
#[cfg(not(feature = "archive"))]
fn encrypt(buf: &mut [u8], key: u16) {
    let mut r = key;
    for b in buf.iter_mut() {
        let c = *b ^ (r >> 8) as u8;
        *b = c;
        r = (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
    }
}

pub fn next(seed: &mut u32) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed
}

/// A Type 1 font program with binary eexec: (bytes, /Length1, /Length2, /Length3), as in type1_fonts.rs.
pub fn type1_font(glyphs: usize) -> (Vec<u8>, usize, usize, usize) {
    let mut seed = 2024u32;
    let mut charstring = |k: usize| -> Vec<u8> {
        let mut cs = vec![0u8; 4];
        cs.extend_from_slice(&[159, 247, (k % 200) as u8, 13]);
        for _ in 0..5 + k % 10 {
            for _ in 0..6 {
                cs.push((135 + next(&mut seed) % 8) as u8);
            }
            cs.push(8);
        }
        cs.extend_from_slice(&[9, 14]);
        encrypt(&mut cs, 4330);
        cs
    };
    let mut private = b"\0\0\0\0dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n/ND{noaccess def}executeonly def\n/NP{noaccess put}executeonly def\n/BlueValues [-22 0 683 705 431 448] def\n/StdHW [31] def\n/StdVW [69] def\n/MinFeature{16 16}def\n/password 5839 def\n/Subrs 4 array\n".to_vec();
    for i in 0..4 {
        let cs = charstring(i);
        private.extend_from_slice(format!("dup {i} {} RD ", cs.len()).as_bytes());
        private.extend_from_slice(&cs);
        private.extend_from_slice(b" NP\n");
    }
    private.extend_from_slice(format!("ND\n2 index /CharStrings {glyphs} dict dup begin\n").as_bytes());
    for g in 0..glyphs {
        let cs = charstring(g + 4);
        private.extend_from_slice(format!("/g{g} {} RD ", cs.len()).as_bytes());
        private.extend_from_slice(&cs);
        private.extend_from_slice(b" ND\n");
    }
    private.extend_from_slice(
        b"end\nend\nreadonly put\nnoaccess put\ndup/FontName get exch definefont pop\nmark currentfile closefile\n",
    );
    encrypt(&mut private, 55665);
    let clear = b"%!PS-AdobeFont-1.0: TestFont 001.000\n11 dict begin\n/FontName /TestFont def\n/PaintType 0 def\n/FontType 1 def\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/Encoding StandardEncoding def\n/FontBBox {0 -250 1000 750} readonly def\ncurrentdict end\ncurrentfile eexec\n".to_vec();
    let mut trailer = Vec::new();
    for _ in 0..8 {
        trailer.extend_from_slice(&[b'0'; 64]);
        trailer.push(b'\n');
    }
    trailer.extend_from_slice(b"cleartomark\n");
    let (l1, l2, l3) = (clear.len(), private.len(), trailer.len());
    ([clear, private, trailer].concat(), l1, l2, l3)
}

/// A page of "text": rows of letter-sized blobs on white, like a scan (as in fax.rs).
pub fn scan(w: usize, h: usize) -> Image {
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

/// The scan as Group 4 data.
pub fn fax_data(img: &Image) -> Vec<u8> {
    let gaps = (0..img.rows()).map(|_| Gap { fill: 0, eol: false, tag: false }).collect();
    let end = vec![Gap { fill: 0, eol: true, tag: false }; 2];
    ccitt::encode(img, &Layout { scheme: Scheme::G4, rule: Rule::Standard, gaps, end, pad: 0, trailing: Vec::new() })
}

pub const JPEG: &[u8] = include_bytes!("../data/jpeg/base444.jpg");
/// Enough glyphs (about 65 KB encrypted) for the font program to look like random bytes, as real fonts do:
/// below effort 50 the pack would code such data plainly, were the font not opened first (checked by
/// `the_font_looks_random`).
pub const FONT_GLYPHS: usize = 700;
pub const FAX_W: usize = 600;
pub const FAX_H: usize = 400;

/// The streams and dictionaries of the test document, unencrypted: (object number, dictionary, stream data).
pub fn objects() -> Vec<(u32, String, Option<Vec<u8>>)> {
    let (font, l1, l2, l3) = type1_font(FONT_GLYPHS);
    let z = |d: &[u8]| zlib(d, 6);
    let fax = fax_data(&scan(FAX_W, FAX_H));
    let mut content = Vec::new();
    for i in 0..60 {
        content.extend_from_slice(
            format!("BT /F1 12 Tf 72 {} Td (Line {i} of the mixed test page) Tj ET\n", 740 - i * 11).as_bytes(),
        );
    }
    content.extend_from_slice(b"q 150 0 0 106 400 600 cm /Im1 Do Q\nq 300 0 0 200 60 60 cm /Im2 Do Q\n");
    vec![
        (1, "/Type /Catalog /Pages 2 0 R".into(), None),
        (2, "/Type /Pages /Kids [3 0 R] /Count 1".into(), None),
        (3, "/Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> /XObject << /Im1 7 0 R /Im2 8 0 R >> >> /Contents 9 0 R".into(), None),
        (4, "/Type /Font /Subtype /Type1 /BaseFont /TestFont /FontDescriptor 5 0 R".into(), None),
        (5, "/Type /FontDescriptor /FontName /TestFont /Flags 4 /FontBBox [0 -250 1000 750] /ItalicAngle 0 /Ascent 750 /Descent -250 /CapHeight 700 /StemV 69 /FontFile 6 0 R".into(), None),
        (6, format!("/Length1 {l1} /Length2 {l2} /Length3 {l3} /Filter /FlateDecode"), Some(z(&font))),
        (7, "/Type /XObject /Subtype /Image /Width 75 /Height 53 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode".into(), Some(JPEG.to_vec())),
        (8, format!("/Type /XObject /Subtype /Image /Width {FAX_W} /Height {FAX_H} /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns {FAX_W} >>"), Some(fax)),
        (9, "/Filter /FlateDecode".into(), Some(z(&content))),
    ]
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// The document encrypted with the standard handler and the empty user password: `aes` for /R 4 with
/// AESV2, else /R 3 with 128-bit RC4. The strings (the title) and every stream are encrypted with the
/// engine's primitives; the key derivation is the one checked against qpdf in the crypt tests.
pub fn encrypted_pdf(aes: bool) -> Vec<u8> {
    let (r, method) = if aes { (4, Method::Aes128) } else { (3, Method::Rc4) };
    let o = [0x11u8; 32];
    let id0 = [0x42u8; 16];
    let p: u32 = 0xFFFF_FFFC;
    let key = crypt::key_r4(r, 16, &o, p, &id0, true);
    // Revisions 3 and 4 compare the first 16 bytes of /U; the other 16 are padding.
    let mut u = crypt::user_value_r4(r, &key, &id0);
    u.resize(32, 0);
    let iv = |n: u32| [n as u8; 16];
    let title = crypt::seal(&key, method, 11, 0, &iv(11), b"Mixed test").unwrap();
    let mut objs: Vec<(u32, Vec<u8>)> = Vec::new();
    for (n, dict, data) in objects() {
        let body = match data {
            Some(d) => {
                let sealed = crypt::seal(&key, method, n, 0, &iv(n), &d).unwrap();
                let mut b = format!("<< {dict} /Length {} >>\nstream\n", sealed.len()).into_bytes();
                b.extend_from_slice(&sealed);
                b.extend_from_slice(b"\nendstream");
                b
            }
            None => format!("<< {dict} >>").into_bytes(),
        };
        objs.push((n, body));
    }
    let enc = if aes {
        format!("<< /Filter /Standard /V 4 /R 4 /Length 128 /P {} /O <{}> /U <{}> /CF << /StdCF << /CFM /AESV2 /AuthEvent /DocOpen /Length 16 >> >> /StmF /StdCF /StrF /StdCF >>", p as i32, hex(&o), hex(&u))
    } else {
        format!("<< /Filter /Standard /V 2 /R 3 /Length 128 /P {} /O <{}> /U <{}> >>", p as i32, hex(&o), hex(&u))
    };
    objs.push((10, enc.into_bytes()));
    objs.push((
        11,
        format!(
            "<< /Title <{}> /Producer <{}> >>",
            hex(&title),
            hex(&crypt::seal(&key, method, 11, 0, &iv(11), b"peyaji").unwrap())
        )
        .into_bytes(),
    ));
    let mut d = b"%PDF-1.6\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offs = Vec::new();
    for (n, body) in &objs {
        offs.push(d.len());
        d.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        d.extend_from_slice(body);
        d.extend_from_slice(b"\nendobj\n");
    }
    let x = d.len();
    d.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in &offs {
        d.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    d.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 11 0 R /Encrypt 10 0 R /ID [<{}> <{}>] >>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1,
            hex(&id0),
            hex(&id0)
        )
        .as_bytes(),
    );
    d
}

pub fn stream(doc: &xref::Doc, n: u32) -> gpress::pdf::object::Stream {
    match doc.load(n).unwrap().1 {
        Obj::Stream(s) => *s,
        o => panic!("object {n} is {o:?}, not a stream"),
    }
}

/// A stream's bytes for the image codec `codec` (its last filter) with any general filters in front of it
/// (our Flate, say) undone.
pub fn codec_bytes(s: &gpress::pdf::object::Stream, codec: &[u8]) -> Vec<u8> {
    use gpress::pdf::filters::{chain, decode_stage, is_general};
    let stages = chain(&s.dict).unwrap();
    let k = stages.iter().take_while(|st| is_general(&st.name)).count();
    assert!(stages[k..].len() == 1 && stages[k].name == codec, "{:?}", s.dict);
    let mut data = s.data.clone();
    for st in &stages[..k] {
        data = decode_stage(st, &data, 1 << 20).unwrap();
    }
    data
}

pub fn jpeg_bytes(s: &gpress::pdf::object::Stream) -> Vec<u8> {
    codec_bytes(s, b"DCTDecode")
}

/// The same document unencrypted (a classic cross-reference table).
pub fn plain_pdf() -> Vec<u8> {
    let mut objs: Vec<(u32, Vec<u8>)> = Vec::new();
    for (n, dict, data) in objects() {
        let body = match data {
            Some(d) => {
                let mut b = format!("<< {dict} /Length {} >>\nstream\n", d.len()).into_bytes();
                b.extend_from_slice(&d);
                b.extend_from_slice(b"\nendstream");
                b
            }
            None => format!("<< {dict} >>").into_bytes(),
        };
        objs.push((n, body));
    }
    objs.push((11, b"<< /Title (Mixed test) /Producer (gpress tests) >>".to_vec()));
    let mut d = b"%PDF-1.6\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offs = Vec::new();
    for (n, body) in &objs {
        offs.push(d.len());
        d.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        d.extend_from_slice(body);
        d.extend_from_slice(b"\nendobj\n");
    }
    let x = d.len();
    d.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in &offs {
        d.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    d.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R /Info 11 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    d
}
