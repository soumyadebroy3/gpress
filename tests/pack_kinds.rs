//! The gpress archive of documents with a part of every kind: syntax, raw streams, zlib streams stored
//! decoded (text, data, images with PNG and TIFF predictors, rows that aren't whole), Type 1 fonts as they
//! are and behind Flate, a fax image, a JPEG (Lepton, in a build with the existing library) and encrypted
//! files. Each restores exactly, and its bytes are pinned: a change meant to keep the format (memory, speed)
//! must give the very same archives.

// Only in a build with the archive (Cargo.toml, [features]). The document with a part of every kind is made from
// the mixed test document's parts (`common`), which need the fax and encryption features too; the image tests
// run without them.
#![cfg(feature = "archive")]

#[cfg(all(feature = "fax", feature = "encryption"))]
mod common;

#[cfg(all(feature = "fax", feature = "encryption"))]
use common::*;
use gpress::flate::zlibexact::{zlib_stream, Params};
use gpress::pack::{pack_with, sha256, unpack, PackOptions};

/// A zlib stream of `d`; small memory levels make small Deflate blocks.
fn z(d: &[u8], mem_level: u8) -> Vec<u8> {
    zlib_stream(d, Params { level: 6, window_bits: 15, mem_level, filtered: false })
}

/// The test files' random numbers, as `common::next` makes them, in a build without the mixed test document.
#[cfg(not(all(feature = "fax", feature = "encryption")))]
fn next(seed: &mut u32) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed
}

#[cfg(all(feature = "fax", feature = "encryption"))]
fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut s = seed;
    (0..n).map(|_| next(&mut s) as u8).collect()
}

/// A smooth picture with a little noise: `w` x `h` pixels of `bpp` bytes.
fn picture(w: usize, h: usize, bpp: usize) -> Vec<u8> {
    let mut s = 99u32;
    (0..h * w * bpp)
        .map(|i| ((i % (w * bpp)) / bpp * 3 + i / (w * bpp) * 2 + i % bpp * 40) as u8 ^ (next(&mut s) % 4) as u8)
        .collect()
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// PNG predictor rows of `px` (rows of `row` bytes, `bpp` bytes per pixel), row r filtered with type `types(r)`.
fn png_rows(px: &[u8], row: usize, bpp: usize, types: impl Fn(usize) -> u8) -> Vec<u8> {
    let mut out = Vec::new();
    for r in 0..px.len() / row {
        let ft = types(r);
        out.push(ft);
        for i in 0..row {
            let x = px[r * row + i];
            let a = if i >= bpp { px[r * row + i - bpp] } else { 0 };
            let b = if r > 0 { px[(r - 1) * row + i] } else { 0 };
            let c = if r > 0 && i >= bpp { px[(r - 1) * row + i - bpp] } else { 0 };
            out.push(match ft {
                0 => x,
                1 => x.wrapping_sub(a),
                2 => x.wrapping_sub(b),
                3 => x.wrapping_sub(((a as u16 + b as u16) / 2) as u8),
                _ => x.wrapping_sub(paeth(a, b, c)),
            });
        }
    }
    out
}

/// TIFF predictor 2 on 8-bit samples: each sample less the one `bpp` before it in its row.
fn tiff_rows(px: &[u8], row: usize, bpp: usize) -> Vec<u8> {
    let mut out = px.to_vec();
    for r in out.chunks_mut(row) {
        for i in (bpp..r.len()).rev() {
            r[i] = r[i].wrapping_sub(r[i - bpp]);
        }
    }
    out
}

/// An image dictionary: `w` x `h` pixels, 8 bits per sample, `cs` the colour space, then `rest`.
fn image(w: usize, h: usize, cs: &str, rest: &str) -> String {
    format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /{cs} /BitsPerComponent 8 {rest}")
}

/// Flate with PNG predictor rows of `cols` pixels of `colors` samples.
fn png(cols: usize, colors: usize) -> String {
    format!("/Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors {colors} /Columns {cols} >>")
}

const FLATE: &str = "/Filter /FlateDecode";
const TIFF: &str = "/Filter /FlateDecode /DecodeParms << /Predictor 2 /Colors 3 /Columns 40 >>";

/// A document with a part of every kind the archive has (see the module notes). The zlib streams are made
/// with small memory levels, so they have many Deflate blocks and image rows cross block ends.
#[cfg(all(feature = "fax", feature = "encryption"))]
fn every_kind() -> Vec<u8> {
    let (font, l1, l2, l3) = type1_font(40);
    let lengths = format!("/Length1 {l1} /Length2 {l2} /Length3 {l3}");
    let mut text = Vec::new();
    for i in 0..300 {
        text.extend_from_slice(
            format!("BT /F1 12 Tf 72 {} Td (Line {i} of every kind) Tj ET\n", 740 - i % 60 * 11).as_bytes(),
        );
    }
    let counter: Vec<u8> = (0..20_000u32).flat_map(|i| (i * 7).to_le_bytes()).collect();
    let (w, h) = (40, 30);
    let rgb = picture(w, h, 3);
    let wide = picture(400, 12, 3);
    let gray = picture(w, h, 1);
    // A row type that isn't one, and rows that aren't whole: such data is coded as it is.
    let mut bad_type = png_rows(&gray, w, 1, |r| (r % 5) as u8);
    bad_type[(w + 1) * 7] = 5;
    let mut not_whole = png_rows(&gray, w, 1, |_| 1);
    not_whole.pop();
    let mut tailed = z(&text[..5000], 8);
    tailed.extend_from_slice(b"\r\n\r\n");
    let fax = format!(
        "/Subtype /Image /Width {FAX_W} /Height {FAX_H} /BitsPerComponent 1 /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns {FAX_W} >>"
    );
    let objs: Vec<(String, Vec<u8>)> = vec![
        ("".into(), text[..3000].to_vec()),
        (FLATE.into(), z(&text, 1)),
        (FLATE.into(), z(&counter, 2)),
        (FLATE.into(), tailed),
        (FLATE.into(), z(b"", 8)),
        (image(w, h, "DeviceRGB", &png(w, 3)), z(&png_rows(&rgb, w * 3, 3, |r| (r % 5) as u8), 1)),
        (image(400, 12, "DeviceRGB", &png(400, 3)), z(&png_rows(&wide, 1200, 3, |r| (4 - r % 5) as u8), 1)),
        (image(w, h, "DeviceGray", &png(w, 1)), z(&bad_type, 1)),
        (image(w, h, "DeviceGray", &png(w, 1)), z(&not_whole, 1)),
        (image(w, 0, "DeviceGray", &png(w, 1)), z(b"", 8)),
        (image(w, h, "DeviceRGB", TIFF), z(&tiff_rows(&rgb, w * 3, 3), 2)),
        (image(w, h, "DeviceRGB", TIFF), z(&tiff_rows(&rgb, w * 3, 3)[..w * 3 * h - 5], 2)),
        (image(64, 64, "DeviceGray", FLATE), z(&noise(64 * 64, 5), 1)),
        (image(w, h, "DeviceGray", FLATE), z(&gray, 3)),
        (lengths.clone(), font.clone()),
        (format!("{lengths} {FLATE}"), z(&font, 1)),
        (fax, fax_data(&scan(FAX_W, FAX_H))),
        (image(75, 53, "DeviceRGB", "/Filter /DCTDecode"), JPEG.to_vec()),
        // Lepton doesn't beat Flate by the 10% it must on this small JPEG: the stream is rebuilt.
        (image(75, 53, "DeviceRGB", "/Filter [/FlateDecode /DCTDecode]"), z(JPEG, 8)),
    ];
    let mut d = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    d.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    for (k, (dict, data)) in objs.iter().enumerate() {
        d.extend_from_slice(format!("{} 0 obj\n<< {dict} /Length {} >>\nstream\n", k + 3, data.len()).as_bytes());
        d.extend_from_slice(data);
        d.extend_from_slice(b"\nendstream\nendobj\n");
    }
    d.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
    d
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The archives' SHA-256 as format version 9 first wrote them (version 8's held from before the encoder
/// stopped building its restored copy): (document, effort, hash), for a build without and with the existing
/// library.
#[cfg(all(feature = "fax", feature = "encryption"))]
const PINNED: [(&str, u8, &str); 4] = [
    ("every kind", 100, "a597ee7d45ac7d1293cfeae8355ca4e155f61cc175f480e06840b22a2cdfb1f2"),
    ("every kind", 0, "45a8a5e19dcc83a166386aa4c66b79b1646eb28804e297adab9831fc6f5fa6d6"),
    ("encrypted RC4", 100, "27d5ff3f4b887912399e247e4cf84ec0adcb1dba538f4fda3b11c47e2e549201"),
    ("encrypted AES", 100, "e401c9add2299a3040dd278227473f267856867a84883c92436a39b7b0ef0ece"),
];
#[cfg(all(feature = "fax", feature = "encryption"))]
const PINNED_EXISTING: [(&str, u8, &str); 4] = [
    ("every kind", 100, "bee400708657889f173d3c5582695695c24898697c971626a1f64ca2b5b3731c"),
    ("every kind", 0, "9cdf819b2ad260595b16ad21c2b2c7bc9b8c43eaf398c73d3375a405b6e16b4f"),
    ("encrypted RC4", 100, "695c4fd91d12ff492e93ee798bce5a97c7f74dba79918f84a6e260e900137450"),
    ("encrypted AES", 100, "edd621f69b24de30368c3d1b412c0da3b43f19a03212d5fc16d521400ad5df6c"),
];

#[test]
#[cfg(all(feature = "fax", feature = "encryption"))]
fn every_part_kind_restores_and_packs_to_the_pinned_bytes() {
    let pinned = if cfg!(feature = "existing") { PINNED_EXISTING } else { PINNED };
    for (name, effort, want) in pinned {
        let pdf = match name {
            "every kind" => every_kind(),
            "encrypted RC4" => encrypted_pdf(false),
            _ => encrypted_pdf(true),
        };
        let opt = PackOptions { effort, threads: 1, ..PackOptions::default() };
        let (packed, st) = pack_with(&pdf, &opt);
        assert!(!st.fallback && !st.stored, "{name} {effort}: {st:?}");
        if name == "every kind" {
            let leptons = if cfg!(feature = "existing") { 1 } else { 0 };
            assert!(st.type1 == 2 && st.fax == 1 && st.leptons == leptons, "{st:?}");
            assert!(st.unpredicted_images == 4 && st.rebuilt == 15, "{st:?}");
        } else {
            assert!(st.decrypted >= 4 && st.type1 == 1 && st.fax == 1, "{name}: {st:?}");
        }
        assert_eq!(unpack(&packed).unwrap(), pdf, "{name} {effort}");
        if effort == 100 {
            let (two, _) = pack_with(&pdf, &PackOptions { threads: 2, ..opt });
            assert!(two == packed, "{name}: two threads give the same bytes");
        }
        assert_eq!(hex(&sha256(&packed)), want, "{name} at effort {effort}: {} bytes", packed.len());
    }
}

/// Images whose rows the encoder works out as the stream is inflated again (see `every_kind` for the
/// rest): one long row, 16-bit samples (two bytes a pixel), four colours, and the same for TIFF.
fn images() -> Vec<u8> {
    let long = picture(5000, 1, 3);
    let deep = picture(30, 20, 2);
    let cmyk = picture(25, 18, 4);
    let objs: Vec<(String, Vec<u8>)> = vec![
        (image(5000, 1, "DeviceRGB", &png(5000, 3)), z(&png_rows(&long, 15000, 3, |_| 4), 1)),
        (
            format!("/Type /XObject /Subtype /Image /Width 30 /Height 20 /ColorSpace /DeviceGray /BitsPerComponent 16 {FLATE} /DecodeParms << /Predictor 15 /BitsPerComponent 16 /Columns 30 >>"),
            z(&png_rows(&deep, 60, 2, |r| (r % 5) as u8), 1),
        ),
        (image(25, 18, "DeviceCMYK", &png(25, 4)), z(&png_rows(&cmyk, 100, 4, |r| (3 * r % 5) as u8), 2)),
        (
            image(25, 18, "DeviceCMYK", "/Filter /FlateDecode /DecodeParms << /Predictor 2 /Colors 4 /Columns 25 >>"),
            z(&tiff_rows(&cmyk, 100, 4), 1),
        ),
        (image(5000, 1, "DeviceRGB", &TIFF.replace("40", "5000")), z(&tiff_rows(&long, 15000, 3), 1)),
    ];
    stream_objects(&objs)
}

/// A catalog and then a stream object for each (dictionary, data).
fn stream_objects(objs: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut d = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    for (k, (dict, data)) in objs.iter().enumerate() {
        d.extend_from_slice(format!("{} 0 obj\n<< {dict} /Length {} >>\nstream\n", k + 2, data.len()).as_bytes());
        d.extend_from_slice(data);
        d.extend_from_slice(b"\nendstream\nendobj\n");
    }
    d.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
    d
}

/// As format version 9 first wrote them (version 8's held from before the encoder took rebuilt streams in
/// pieces), the same with or without the existing library: (effort, hash).
const PINNED_IMAGES: [(u8, &str); 2] = [
    (100, "3c279949f4bbd23d6664b1e311b7b91c4d1d464a98fdd7f51060e95f94294241"),
    (0, "3c279949f4bbd23d6664b1e311b7b91c4d1d464a98fdd7f51060e95f94294241"),
];

#[test]
fn images_made_row_by_row_pack_to_the_pinned_bytes() {
    let pdf = images();
    for (effort, want) in PINNED_IMAGES {
        let (packed, st) = pack_with(&pdf, &PackOptions { effort, threads: 1, ..PackOptions::default() });
        assert!(st.rebuilt == 5 && st.unpredicted_images == 5 && !st.fallback && !st.stored, "{st:?}");
        assert_eq!(unpack(&packed).unwrap(), pdf);
        assert_eq!(hex(&sha256(&packed)), want, "effort {effort}: {} bytes", packed.len());
    }
}

/// PNG rows the encoder codes in batches of about 64 KiB: rows of one byte (many to a batch), of 3,000 bytes (a
/// batch ends inside the image) and of 70,000 bytes (one to a batch), so the row above the first row of a
/// batch, which the filter types 2 to 4 read, is the one read back from what was coded.
fn png_batches() -> Vec<u8> {
    let types = |r: usize| ((r + 1) % 5) as u8;
    let objs = vec![
        (image(1, 70_000, "DeviceGray", &png(1, 1)), z(&png_rows(&picture(1, 70_000, 1), 1, 1, types), 1)),
        (image(1000, 50, "DeviceRGB", &png(1000, 3)), z(&png_rows(&picture(1000, 50, 3), 3000, 3, types), 8)),
        (image(70_000, 3, "DeviceGray", &png(70_000, 1)), z(&png_rows(&picture(70_000, 3, 1), 70_000, 1, types), 1)),
    ];
    stream_objects(&objs)
}

/// As format version 9 first wrote it (version 8's held from before the encoder coded PNG rows in batches, and
/// was the same with base-bin-199).
const PINNED_BATCHES: &str = "af0e606f45f36a1427cc8f0ee2e70a5ea7a55fe4cdd0743f11c737cdde776789";

#[test]
fn png_rows_coded_in_batches_pack_to_the_pinned_bytes() {
    let pdf = png_batches();
    let (packed, st) = pack_with(&pdf, &PackOptions { threads: 1, ..PackOptions::default() });
    assert!(st.rebuilt == 3 && st.unpredicted_images == 3 && !st.fallback && !st.stored, "{st:?}");
    assert_eq!(unpack(&packed).unwrap(), pdf);
    assert_eq!(hex(&sha256(&packed)), PINNED_BATCHES, "{} bytes", packed.len());
}
