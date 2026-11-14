//! The archive's rules (`ArchiveRules`, src/pack/rules.rs), driven by their own table: each rule set by hand to
//! its default is the same as unset; at its lowest and at its highest value every archive restores byte for
//! byte, is never larger than the file stored as it is, and is version 10 exactly when a shared rule differs from
//! its default; expand takes the shared rules from the archive, never from its caller; a damaged profile is
//! refused before anything is sized by it.

// Only in a build with the archive (Cargo.toml, [features]). The document with a part of every kind is the mixed
// test document (`common`), which needs the fax and encryption features too.
#![cfg(feature = "archive")]

#[cfg(all(feature = "fax", feature = "encryption"))]
mod common;

use gpress::pack::{pack_with, unpack, unpack_with, PackOptions, UnpackError, UnpackOptions, VERSION, VERSION_PROFILE};
use gpress::ArchiveRules;

const PLAIN_PDF: &[u8] = include_bytes!("data/format-v9.pdf");
const PLAIN_GPZ: &[u8] = include_bytes!("data/format-v9.gpz");

fn pack_by(input: &[u8], rules: &ArchiveRules, effort: u8, threads: usize) -> Vec<u8> {
    pack_with(input, &PackOptions { effort, threads, rules: rules.clone(), ..PackOptions::default() }).0
}

fn row(name: &str) -> usize {
    ArchiveRules::RULES.iter().position(|r| r.0 == name).unwrap()
}

#[test]
fn every_rule_has_a_sound_range_and_its_default_is_unset() {
    let mut by_hand = ArchiveRules::default();
    for (i, &(name, def, lo, hi)) in ArchiveRules::RULES.iter().enumerate() {
        assert!(ArchiveRules::RULES[..i].iter().all(|r| r.0 != name), "{name} twice");
        assert!(lo <= def && def <= hi, "{name}");
        assert_eq!(ArchiveRules::default().get(name), Some(def), "{name}");
        assert!(by_hand.set(name, def), "{name}");
        // Out of range: refused, and nothing changes.
        let mut r = ArchiveRules::default();
        assert!(lo == 0 || !r.set(name, lo - 1), "{name}");
        assert!(!r.set(name, hi + 1), "{name}");
        assert_eq!(r, ArchiveRules::default(), "{name}");
        assert!(r.set(name, lo) && r.get(name) == Some(lo) && r.set(name, hi) && r.get(name) == Some(hi), "{name}");
    }
    assert!(!by_hand.set("no_such_rule", 0) && by_hand.get("no_such_rule").is_none());
    // Every rule set by hand to its default: the same rules, and the archive the first build of version 9 wrote.
    assert_eq!(by_hand, ArchiveRules::default());
    assert!(pack_by(PLAIN_PDF, &by_hand, 100, 1) == PLAIN_GPZ);
    assert!(ArchiveRules::SHARED < 256 && ArchiveRules::SHARED < ArchiveRules::RULES.len());
}

/// A profile names a rule by its row, so the shared rows are part of the format: a new shared rule goes after
/// the last one, and none is ever moved or removed.
#[test]
fn the_shared_rows_keep_their_order() {
    let shared: Vec<&str> = ArchiveRules::RULES[..ArchiveRules::SHARED].iter().map(|r| r.0).collect();
    #[rustfmt::skip]
    let want = [
        "mixer_rate", "mixer_shift", "mixer_start", "mixer_bias", "apm_bits", "apm_round", "apm1_rate", "apm2_rate",
        "blend_apm1", "blend_apm2", "state_count", "state_discount", "state_limit", "match_min", "match_check",
        "match_bits", "match_cap", "match_long", "match_limit", "repeat_min", "repeat_limit", "expect_cap",
        "expect_limit", "table_bits_min", "table_bits_max", "table_shift", "prime_rounds", "hash_bytes",
        "font_window", "fax_bits_min", "fax_bits_max", "fax_bits_add", "fax_small_limit", "fax_mixer_shift",
        "fax_mixer_start", "fax_apm_bits", "fax_apm_shift", "fax_limit", "fax_hashed_limit", "fax_blend_apm1",
        "fax_blend_apm2", "image_step_1", "image_step_2", "image_step_3", "image_step_4", "image_step_5",
        "image_step_6", "image_step_7", "word_bytes", "expect_text", "fax_white_shortcut",
    ];
    assert_eq!(shared, want);
}

/// Every rule at its lowest and at its highest value, alone, on the mixed document (a page, a Type 1 font, a
/// JPEG, a fax image, a zlib content stream) with a stream of random bytes added, at effort 40, below
/// `plain_below`, so that the plain-coding rules decide something too; the decryption rules on the document
/// encrypted.
#[cfg(all(feature = "fax", feature = "encryption"))]
#[test]
fn every_rule_at_its_ends_restores_and_changes_the_archive() {
    let mut doc = common::plain_pdf();
    let mut seed = 7;
    let noise: Vec<u8> = (0..40_000).map(|_| common::next(&mut seed) as u8).collect();
    doc.extend_from_slice(format!("11 0 obj\n<< /Length {} >>\nstream\n", noise.len()).as_bytes());
    doc.extend_from_slice(&noise);
    doc.extend_from_slice(b"\nendstream\nendobj\n");
    let docs = [doc, common::encrypted_pdf(false)];
    let base: Vec<Vec<u8>> = docs.iter().map(|d| pack_by(d, &ArchiveRules::default(), 40, 1)).collect();
    let check = |i: usize| -> Option<&'static str> {
        let (name, def, lo, hi) = ArchiveRules::RULES[i];
        let mut changed = false;
        for v in [lo, hi].into_iter().filter(|&v| v != def) {
            let mut r = ArchiveRules::default();
            assert!(r.set(name, v));
            // The decryption rules decide something only for an encrypted file.
            let doc = name.starts_with("decrypt_") as usize;
            for (d, b) in docs.iter().zip(&base).skip(doc).take(1) {
                let a = pack_by(d, &r, 40, 1);
                let want = if i < ArchiveRules::SHARED { VERSION_PROFILE } else { VERSION };
                // Version 2 is the file stored as it is (`min_saving` at its highest).
                assert!(a[4] == want || a[4] == 2, "{name} = {v}: version {}", a[4]);
                assert!(a.len() <= d.len() + 32, "{name} = {v}: {} bytes for {}", a.len(), d.len());
                assert!(unpack(&a).unwrap() == *d, "{name} = {v}");
                assert!(pack_by(d, &r, 40, 4) == a, "{name} = {v}: not the same bytes at 4 threads");
                changed |= a != *b;
            }
        }
        (!changed).then_some(name)
    };
    let mut unchanged: Vec<&str> = std::thread::scope(|s| {
        let check = &check;
        let workers: Vec<_> = (0..4)
            .map(|w| s.spawn(move || (w..ArchiveRules::RULES.len()).step_by(4).filter_map(check).collect::<Vec<_>>()))
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    unchanged.sort_unstable();
    // `deadline_step` never changes the output. The others decide nothing on these files: `decrypt_recheck` and
    // `decrypt_tie` choose between two packs that are far apart here; no fax stream is left partly undecoded
    // (`fax_max_leftover`); the font's /Length1 finds its encrypted part at once, and that part decrypts to
    // plain text (`font_tries`, `font_text_bytes`); the random stream is smaller than the 16 windows
    // (`plain_windows`); zlib's "filtered" strategy made no stream (`zlib_filtered`). Without Lepton its rules
    // decide nothing; with it, the JPEG of the document is a stream of its own (not behind Flate) and small
    // enough for one partition, and as Lepton takes it, only the random stream is judged for plain coding, which
    // looks random at any distance (`plain_distances`). `each_packing_choice_can_change_the_archive` gives each
    // of these but `deadline_step` and the two decryption rules an input where it does.
    #[rustfmt::skip]
    let mut want = vec![
        "deadline_step", "decrypt_recheck", "decrypt_tie", "fax_max_leftover", "font_text_bytes", "font_tries",
        "lepton_min_saving", "lepton_partitions", "plain_windows", "zlib_filtered",
    ];
    if cfg!(feature = "existing") {
        want.push("plain_distances");
    } else {
        want.extend(["lepton_min_bytes", "lepton_min_gain", "lepton_plain"]);
    }
    want.sort_unstable();
    assert_eq!(unchanged, want);
}

/// A one-stream document: `data` as object 1, with `dict` in its dictionary.
#[cfg(all(feature = "fax", feature = "encryption"))]
fn one_stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut d = format!("%PDF-1.4\n1 0 obj\n<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    d.extend_from_slice(data);
    d.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Size 2 >>\n%%EOF\n");
    d
}

/// The rules of the packing side that the mixed document leaves unchanged, each on an input where one value
/// changes the archive (and both archives restore). `deadline_step` never changes it; `decrypt_recheck` and
/// `decrypt_tie` only decide whether a second pack, without decryption, is made and which of two equally long
/// ones is kept, and no small input was found where that second pack is the one kept.
#[cfg(all(feature = "fax", feature = "encryption"))]
#[test]
fn each_packing_choice_can_change_the_archive() {
    use gpress::flate::zlibexact::{zlib_stream, Params};
    use gpress::pack::type1::{decrypt, encrypt};
    let zlib =
        |data: &[u8], filtered: bool| zlib_stream(data, Params { level: 6, window_bits: 15, mem_level: 8, filtered });
    let mut seed = 5;
    let mut noise = |n: usize| -> Vec<u8> { (0..n).map(|_| common::next(&mut seed) as u8).collect() };
    let mut cases: Vec<(&str, u64, Vec<u8>)> = Vec::new();
    // A content stream that only zlib's "filtered" strategy makes.
    let text: Vec<u8> = (0..400)
        .flat_map(|i| {
            format!("BT /F{} {} Tf {} {} Td (w{}) Tj ET\n", i % 7, 9 + i % 5, 72 + i % 13, 700 - i, i * 37 % 101)
                .into_bytes()
        })
        .collect();
    cases.push(("zlib_filtered", 0, one_stream("/Filter /FlateDecode", &zlib(&text, true))));
    // A font whose /Length1 is 0: found only by looking for "eexec".
    let (font, _, l2, l3) = common::type1_font(150);
    cases.push(("font_tries", 0, one_stream(&format!("/Length1 0 /Length2 {l2} /Length3 {l3}"), &font)));
    // A font whose encrypted part starts with Latin-1 text.
    let (font, l1, l2, l3) = common::type1_font(150);
    let mut private = font[l1..l1 + l2].to_vec();
    decrypt(&mut private, 55665);
    let mut p = [&private[..4], b"%\xe9t\xe9 \xe0 l\xe0 \xe9t\xe9 \xe0 l\xe0 \xe9t\xe9\n", &private[4..]].concat();
    encrypt(&mut p, 55665);
    let font = [&font[..l1], &p, &font[l1 + l2..]].concat();
    cases.push(("font_text_bytes", 1, one_stream(&format!("/Length1 {l1} /Length2 {} /Length3 {l3}", p.len()), &font)));
    // A fax stream with a tenth of random bytes after its image.
    let mut fax = common::fax_data(&common::scan(600, 400));
    fax.extend(noise(fax.len() / 10));
    let dict = "/Type /XObject /Subtype /Image /Width 600 /Height 400 /ColorSpace /DeviceGray /BitsPerComponent 1 \
        /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 600 >>";
    cases.push(("fax_max_leftover", 0, one_stream(dict, &fax)));
    // Random bytes, a ramp, random bytes: the ramp lies between the first and the last window.
    let data = [noise(100_000), (0..100_000u32).map(|i| i as u8).collect(), noise(100_000)].concat();
    cases.push(("plain_windows", 2, one_stream("", &data)));
    #[cfg(feature = "existing")]
    {
        // A small JPEG behind Flate, which Lepton shrinks less than 10% below the Flate bytes.
        cases.push(("lepton_min_saving", 0, one_stream("/Filter /FlateDecode", &zlib(common::JPEG, false))));
        // A JPEG with over 125,000 bytes of scan, which Lepton codes in two pieces when it may.
        let dict = "/Type /XObject /Subtype /Image /Width 75 /Height 4096 /ColorSpace /DeviceRGB /BitsPerComponent 8 \
            /Filter /DCTDecode";
        cases.push(("lepton_partitions", 1, one_stream(dict, &tall_jpeg(64))));
    }
    for (name, v, doc) in &cases {
        let base = pack_by(doc, &ArchiveRules::default(), 40, 1);
        let mut r = ArchiveRules::default();
        assert!(r.set(name, *v));
        let a = pack_by(doc, &r, 40, 1);
        assert!(unpack(&base).unwrap() == *doc && unpack(&a).unwrap() == *doc, "{name}");
        assert!(a != base, "{name} = {v} changed nothing");
    }
}

/// restart5.jpg (75 x 53, a restart marker after each row of 16-pixel blocks) with its four rows of blocks
/// repeated `copies` times: a JPEG of 75 x 64 * `copies` pixels, about 2.3 KB of scan a copy.
#[cfg(all(feature = "existing", feature = "fax", feature = "encryption"))]
fn tall_jpeg(copies: usize) -> Vec<u8> {
    let j: &[u8] = include_bytes!("data/jpeg/restart5.jpg");
    let (mut out, mut i) = (j[..2].to_vec(), 2);
    loop {
        let (marker, len) = (j[i + 1], u16::from_be_bytes([j[i + 2], j[i + 3]]) as usize);
        let at = out.len();
        out.extend_from_slice(&j[i..i + 2 + len]);
        if marker == 0xC0 {
            out[at + 5..at + 7].copy_from_slice(&(64 * copies as u16).to_be_bytes());
        }
        i += 2 + len;
        if marker == 0xDA {
            break;
        }
    }
    // The scan, without its end marker, split at its restart markers.
    let scan = &j[i..j.len() - 2];
    let mut rows = Vec::new();
    let mut start = 0;
    for k in 0..scan.len() - 1 {
        if scan[k] == 0xFF && (0xD0..=0xD7).contains(&scan[k + 1]) {
            rows.push(&scan[start..k]);
            start = k + 2;
        }
    }
    rows.push(&scan[start..]);
    assert_eq!(rows.len(), 4);
    let n = 4 * copies;
    for r in 0..n {
        out.extend_from_slice(rows[r % 4]);
        if r + 1 < n {
            out.extend_from_slice(&[0xFF, 0xD0 + (r % 8) as u8]);
        }
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// Every shared rule at its lowest at once, and every one at its highest (the largest profile: 160 MiB of
/// context tables, 16 MiB of match table), restore; the same rules give the same bytes at any thread count. Some
/// repeated text is added, so that even the lowest values make an archive smaller than the file stored as it is.
#[test]
fn all_shared_rules_at_their_ends_at_once() {
    let input = [PLAIN_PDF, &b"BT /F1 12 Tf 72 700 Td (Line of a page) Tj ET\n".repeat(700)].concat();
    for end in [2, 3] {
        let mut r = ArchiveRules::default();
        for rule in &ArchiveRules::RULES[..ArchiveRules::SHARED] {
            let v = if end == 2 { rule.2 } else { rule.3 };
            assert!(r.set(rule.0, v));
        }
        let a = pack_by(&input, &r, 100, 1);
        assert_eq!(a[4], VERSION_PROFILE);
        assert!(unpack(&a).unwrap() == input);
        assert!(pack_by(&input, &r, 100, 4) == a);
    }
}

/// The index just past the base-128 number at `i`.
fn past_varint(a: &[u8], mut i: usize) -> usize {
    while a[i] & 0x80 != 0 {
        i += 1;
    }
    i + 1
}

#[test]
fn expand_takes_the_shared_rules_from_the_archive() {
    let mut r = ArchiveRules::default();
    assert!(r.set("mixer_rate", 5) && r.set("match_min", 4) && r.set("hash_bytes", 32) && r.set("plain_below", 0));
    let a = pack_by(PLAIN_PDF, &r, 100, 1);
    assert_eq!(a[4], VERSION_PROFILE);
    // The profile: the count, then each changed shared rule as its row and its value, rows rising (a rule of
    // the packing side, `plain_below`, is not in it), then 32 bytes of the checksum.
    let p = past_varint(&a, past_varint(&a, 5));
    let profile = [3, row("mixer_rate") as u8, 5, row("match_min") as u8, 4, row("hash_bytes") as u8, 32];
    assert_eq!(a[p..p + 7], profile);
    let v9 = pack_by(PLAIN_PDF, &ArchiveRules::default(), 100, 1);
    assert_eq!(v9[4], VERSION);
    // Whatever rules the caller gives, each archive restores by its own.
    let mut other = r.clone();
    assert!(other.set("mixer_rate", 7) && other.set("table_bits_max", 8));
    for caller in [ArchiveRules::default(), r, other] {
        let opt = UnpackOptions { rules: caller, ..UnpackOptions::default() };
        assert_eq!(unpack_with(&a, &opt).unwrap(), PLAIN_PDF);
        assert_eq!(unpack_with(&v9, &opt).unwrap(), PLAIN_PDF);
    }
}

#[test]
fn damaged_profiles_are_refused() {
    let mut r = ArchiveRules::default();
    assert!(r.set("mixer_rate", 5) && r.set("match_min", 4) && r.set("hash_bytes", 32));
    let a = pack_by(PLAIN_PDF, &r, 100, 1);
    let p = past_varint(&a, past_varint(&a, 5));
    let (m, n, h) = (row("mixer_rate") as u8, row("match_min") as u8, row("hash_bytes") as u8);
    assert_eq!(a[p..p + 7], [3, m, 5, n, 4, h, 32]);
    let with = |profile: &[u8]| [&a[..p], profile, &a[p + 7..]].concat();
    let shared = ArchiveRules::SHARED as u8;
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("no entry", with(&[0])),
        ("cut in the count", a[..p].to_vec()),
        ("cut in an entry", a[..p + 4].to_vec()),
        ("a value below its range", with(&[3, m, 0, n, 4, h, 32])),
        ("a value above its range", with(&[3, m, 9, n, 4, h, 32])),
        (
            "a value far above its range",
            with(&[3, m, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, n, 4, h, 32]),
        ),
        ("a value that never ends", with(&[3, m, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01])),
        ("a default", with(&[3, m, 6, n, 4, h, 32])),
        ("a row twice", with(&[3, m, 5, m, 5, h, 32])),
        ("rows not rising", with(&[3, n, 4, m, 5, h, 32])),
        ("a rule of the packing side", with(&[3, m, 5, n, 4, shared, 0])),
        ("no such rule", with(&[3, m, 5, n, 4, 255, 32])),
    ];
    for (why, b) in &bad {
        assert_eq!(unpack(b), Err(UnpackError::Damaged("bad profile")), "{why}");
    }
    // A profile the checksum doesn't cover: a version-9 archive read as version 10, and the other way round.
    let v9 = pack_by(PLAIN_PDF, &ArchiveRules::default(), 100, 1);
    for (mut b, v) in [(v9, VERSION_PROFILE), (a.clone(), VERSION)] {
        b[4] = v;
        assert!(unpack(&b).is_err(), "version {v}");
    }
    // A profile that is whole but not this archive's, or that reads into the checksum: refused, never a wrong
    // file.
    assert!(unpack(&with(&[3, m, 4, n, 4, h, 32])).is_err());
    assert!(unpack(&with(&[4, m, 5, n, 4, h, 32])).is_err());
    assert_eq!(unpack(&a).unwrap(), PLAIN_PDF);
}
