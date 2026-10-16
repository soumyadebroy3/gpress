//! Our decoder against streams made by CPython's zlib (every level and strategy, two window sizes), and our fast
//! encoder against zlib on the same inputs.

use gpress::flate::{self, zlib};
use std::path::Path;

#[test]
fn decodes_every_zlib_vector() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let list = std::fs::read_to_string(dir.join("vectors.txt")).unwrap();
    let mut n = 0;
    for line in list.lines() {
        let (z, raw) = line.split_once('\t').unwrap();
        let zb = std::fs::read(dir.join(z)).unwrap();
        let want = std::fs::read(dir.join(raw)).unwrap();
        let got = zlib::unwrap(&zb, 1 << 30).unwrap_or_else(|e| panic!("{z}: {e:?}"));
        assert_eq!(got.data, want, "{z}");
        assert_eq!(got.trailing, 0, "{z}");
        n += 1;
    }
    assert_eq!(n, 240);
}

#[test]
fn refuses_damaged_streams() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let zb = std::fs::read(dir.join("text-9-def-15.z")).unwrap();
    // Truncated anywhere: must be an error, never a silent partial result.
    for cut in [3, zb.len() / 2, zb.len() - 5, zb.len() - 1] {
        assert!(zlib::unwrap(&zb[..cut], 1 << 30).is_err(), "cut at {cut}");
    }
    // Wrong checksum.
    let mut bad = zb.clone();
    let k = bad.len() - 1;
    bad[k] ^= 1;
    assert!(zlib::unwrap(&bad, 1 << 30).is_err());
    // Output limit.
    assert!(zlib::unwrap(&zb, 100).is_err());
}

/// The fast encoder (efforts 0 to 19) on the inputs of the vectors above, at every level: our decoder, checked
/// against CPython's zlib by `decodes_every_zlib_vector`, gives each input back, and the output is never larger
/// than zlib's own at level 1, nor from level 8 on larger than zlib's at level 9.
#[test]
fn the_fast_encoder_against_zlib_on_the_same_inputs() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    for name in ["empty", "one", "random", "text", "zeros", "mixed"] {
        let raw = std::fs::read(dir.join(format!("{name}.raw"))).unwrap();
        let zlib1 = std::fs::read(dir.join(format!("{name}-1-def-15.z"))).unwrap().len();
        let zlib9 = std::fs::read(dir.join(format!("{name}-9-def-15.z"))).unwrap().len();
        for level in 1..=flate::fast::LEVELS {
            let z = flate::deflate_zlib(&raw, &flate::Options { fast: level, ..flate::Options::quick() });
            let back = zlib::unwrap(&z, raw.len() + 1).unwrap_or_else(|e| panic!("{name}, level {level}: {e:?}"));
            assert!(back.data == raw && back.trailing == 0, "{name}, level {level}");
            assert!(z.len() <= zlib1, "{name}, level {level}: {} against zlib -1's {zlib1}", z.len());
            if level >= 8 {
                assert!(z.len() <= zlib9, "{name}, level {level}: {} against zlib -9's {zlib9}", z.len());
            }
        }
    }
}

/// The Flate streams of every PDF in tests/data that are not encrypted (page content, a grey and an RGB image,
/// a long mostly-zero stream, a cross-reference stream: 6 in all), decoded and encoded again by the fast
/// encoder at every level: each gives its data back.
#[test]
fn the_fast_encoder_round_trips_the_test_pdfs_streams() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut pdfs: Vec<_> = [dir.clone(), dir.join("enc")]
        .iter()
        .flat_map(|d| std::fs::read_dir(d).unwrap().map(|e| e.unwrap().path()))
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    pdfs.sort();
    assert!(pdfs.len() >= 20, "{} PDFs", pdfs.len());
    let mut n = 0;
    for path in &pdfs {
        let pdf = path.display();
        let d = std::fs::read(path).unwrap();
        let mut at = 0;
        while let Some(k) = d[at..].windows(6).position(|w| w == b"stream") {
            at += k + 6;
            let start = at + if d.get(at) == Some(&b'\r') { 2 } else { 1 };
            let Ok(u) = zlib::unwrap(&d[start.min(d.len())..], 1 << 24) else { continue };
            if u.data.is_empty() {
                continue;
            }
            for level in 1..=flate::fast::LEVELS {
                let z = flate::deflate_zlib(&u.data, &flate::Options { fast: level, ..flate::Options::quick() });
                let back = zlib::unwrap(&z, u.data.len() + 1).unwrap_or_else(|e| panic!("{pdf}, level {level}: {e:?}"));
                assert!(back.data == u.data, "{pdf}, level {level}");
            }
            n += 1;
        }
    }
    assert!(n >= 6, "{n} streams");
}
