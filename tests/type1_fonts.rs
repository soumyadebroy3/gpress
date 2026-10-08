//! Type 1 fonts in the peyaji format: opened (both encryption layers removed), restored byte for byte, and
//! left as they are when they can't be written back exactly. Damaged packs are refused, never restored wrong.

use engine::flate::zlibexact::{zlib_stream, Params};
use engine::pack::type1::encrypt;
use engine::pack::{pack, pack_with, unpack, PackOptions};

fn next(seed: &mut u32) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed
}

/// How the encrypted part is written.
#[derive(Clone, Copy, PartialEq)]
enum Form {
    Binary,
    /// 64 upper-case digits per line.
    Hex,
    /// Hex with one line longer than the others: can't be written back from a line length.
    UnevenHex,
}

/// A Type 1 font program with `glyphs` charstrings of curves: (bytes, /Length1, /Length2, /Length3).
fn type1_font(glyphs: usize, form: Form) -> (Vec<u8>, usize, usize, usize) {
    let mut seed = 2024u32;
    let mut charstring = |k: usize| -> Vec<u8> {
        // lenIV random bytes, "20 width hsbw", curves of small numbers, "closepath endchar".
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
    private.extend_from_slice(b"end\nend\nreadonly put\nnoaccess put\ndup/FontName get exch definefont pop\nmark currentfile closefile\n");
    encrypt(&mut private, 55665);
    let clear = b"%!PS-AdobeFont-1.0: TestFont 001.000\n11 dict begin\n/FontName /TestFont def\n/PaintType 0 def\n/FontType 1 def\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/Encoding StandardEncoding def\n/FontBBox {0 -250 1000 750} readonly def\ncurrentdict end\ncurrentfile eexec\n".to_vec();
    let encrypted = match form {
        Form::Binary => private,
        Form::Hex | Form::UnevenHex => {
            let digits: String = private.iter().map(|b| format!("{b:02X}")).collect();
            let mut lines: Vec<&str> = digits.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap()).collect();
            let joined;
            if form == Form::UnevenHex {
                joined = format!("{}{}", lines[3], lines[4]);
                lines.splice(3..5, [joined.as_str()]);
            }
            lines.iter().map(|l| format!("{l}\n")).collect::<String>().into_bytes()
        }
    };
    let mut trailer = Vec::new();
    for _ in 0..8 {
        trailer.extend_from_slice(&[b'0'; 64]);
        trailer.push(b'\n');
    }
    trailer.extend_from_slice(b"cleartomark\n");
    let (l1, l2, l3) = (clear.len(), encrypted.len(), trailer.len());
    ([clear, encrypted, trailer].concat(), l1, l2, l3)
}

/// A one-page PDF using the font; `flate`: the font program behind /FlateDecode (made by zlib), else stored
/// as it is.
fn pdf_with_font(font: &(Vec<u8>, usize, usize, usize), flate: bool) -> Vec<u8> {
    let (program, l1, l2, l3) = font;
    let content = b"BT /F1 24 Tf 72 700 Td (Type 1 test) Tj ET\n";
    let (data, filter) = if flate {
        (zlib_stream(program, Params { level: 9, window_bits: 15, mem_level: 8, filtered: false }), " /Filter /FlateDecode")
    } else {
        (program.clone(), "")
    };
    let mut d = Vec::new();
    d.extend_from_slice(b"%PDF-1.4\n");
    d.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    d.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    d.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 6 0 R >>\nendobj\n");
    d.extend_from_slice(b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /TestFont /FontDescriptor 5 0 R >>\nendobj\n");
    d.extend_from_slice(b"5 0 obj\n<< /Type /FontDescriptor /FontName /TestFont /Flags 4 /FontBBox [0 -250 1000 750] /ItalicAngle 0 /Ascent 750 /Descent -250 /CapHeight 700 /StemV 69 /FontFile 7 0 R >>\nendobj\n");
    d.extend_from_slice(format!("6 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    d.extend_from_slice(content);
    d.extend_from_slice(b"\nendstream\nendobj\n");
    d.extend_from_slice(format!("7 0 obj\n<< /Length {} /Length1 {l1} /Length2 {l2} /Length3 {l3}{filter} >>\nstream\n", data.len()).as_bytes());
    d.extend_from_slice(&data);
    d.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Size 8 /Root 1 0 R >>\n%%EOF\n");
    d
}

#[test]
fn opens_type1_fonts_and_restores_them_exactly() {
    for (form, flate) in [(Form::Binary, true), (Form::Binary, false), (Form::Hex, true), (Form::Hex, false)] {
        let font = type1_font(150, form);
        let pdf = pdf_with_font(&font, flate);
        let (packed, stats) = pack(&pdf);
        assert_eq!(stats.type1, 1, "{stats:?}");
        assert!(!stats.fallback && !stats.stored, "{stats:?}");
        assert_eq!(unpack(&packed).unwrap(), pdf);
        // Encrypted, the private part is random bytes (or digits for random bytes); opened, it is small.
        let encrypted = if form == Form::Binary { font.2 } else { font.2 / 2 };
        assert!(packed.len() * 2 < encrypted, "{flate}: {} -> {}", pdf.len(), packed.len());
    }
}

#[test]
fn fonts_are_opened_at_every_effort() {
    // A font big enough that its encrypted part makes the whole program look like random bytes, as real
    // fonts do (about 130 KB here). Below effort 50 such data is coded plainly, but a font is opened first:
    // opened it is text and charstrings, so the pack is as small as at effort 100 (which codes nothing
    // plainly). Before this rule, issue2462.pdf packed 3.3 times larger at effort 0 than at 100.
    let font = type1_font(1500, Form::Binary);
    for flate in [false, true] {
        let pdf = pdf_with_font(&font, flate);
        let (full, st100) = pack_with(&pdf, &PackOptions { effort: 100, threads: 1, ..PackOptions::default() });
        assert_eq!(st100.type1, 1, "{st100:?}");
        for effort in [0u8, 49] {
            let (quick, st) = pack_with(&pdf, &PackOptions { effort, threads: 1, ..PackOptions::default() });
            assert_eq!(st.type1, 1, "effort {effort}: {st:?}");
            assert_eq!(st.flat_bytes, 0, "effort {effort}: {st:?}");
            assert!(quick.len() * 100 <= full.len() * 103, "effort {effort}: {} bytes, effort 100: {}", quick.len(), full.len());
            assert_eq!(unpack(&quick).unwrap(), pdf);
        }
    }
}

#[test]
fn leaves_fonts_alone_when_they_cant_be_written_back() {
    // Hex with uneven lines: the digits decrypt, but no line length writes them back the same way.
    let font = type1_font(60, Form::UnevenHex);
    for flate in [true, false] {
        let pdf = pdf_with_font(&font, flate);
        let (packed, stats) = pack(&pdf);
        assert_eq!(stats.type1, 0, "{stats:?}");
        assert_eq!(unpack(&packed).unwrap(), pdf);
    }
    // Random bytes announced as a Type 1 font: kept as they are.
    let mut seed = 77u32;
    let random: Vec<u8> = (0..20000).map(|_| next(&mut seed) as u8).collect();
    let pdf = pdf_with_font(&(random, 3000, 15000, 2000), true);
    let (packed, stats) = pack(&pdf);
    assert_eq!(stats.type1, 0, "{stats:?}");
    assert_eq!(unpack(&packed).unwrap(), pdf);
    // A font cut short inside its private part (its lengths now wrong): opened as far as it goes.
    let binary = type1_font(60, Form::Binary);
    let pdf = pdf_with_font(&(binary.0[..binary.1 + 100].to_vec(), binary.1, binary.2, binary.3), true);
    let (packed, stats) = pack(&pdf);
    assert_eq!(stats.type1, 1, "{stats:?}");
    assert_eq!(unpack(&packed).unwrap(), pdf);
}

#[test]
fn refuses_earlier_format_versions() {
    // Versions 4 and 5 coded Type 1 fonts and fax images differently (or not at all), and 6 had the preflate
    // part; nothing before 7 was shipped, so their decoders are gone.
    let pdf = pdf_with_font(&type1_font(10, Form::Binary), true);
    let (packed, _) = pack(&pdf);
    assert_eq!(packed[4], engine::pack::VERSION);
    assert_eq!(engine::pack::VERSION, 7);
    for v in [1u8, 3, 4, 5, 6, 8] {
        let mut bad = packed.clone();
        bad[4] = v;
        assert!(unpack(&bad).is_err(), "version {v}");
    }
}

#[test]
fn refuses_damaged_files_with_type1_fonts() {
    for form in [Form::Binary, Form::Hex] {
        let pdf = pdf_with_font(&type1_font(40, form), true);
        let (packed, stats) = pack(&pdf);
        assert_eq!(stats.type1, 1);
        // One bit changed in a sample of places (every unpack primes a new model, so not everywhere), and
        // the file cut short.
        let mut cases: Vec<Vec<u8>> = Vec::new();
        for i in (25..packed.len()).filter(|i| i % 9 == 0) {
            let mut b = packed.clone();
            b[i] ^= 1 << (i % 8);
            cases.push(b);
        }
        for cut in (0..packed.len()).step_by(37) {
            cases.push(packed[..cut].to_vec());
        }
        for (k, c) in cases.iter().enumerate() {
            if let Ok(out) = unpack(c) {
                assert_eq!(out, pdf, "case {k}: a damaged file restored to something else");
            }
        }
    }
}
