//! Our decoder against streams made by CPython's zlib (every level and strategy, two window sizes).

use engine::flate::zlib;
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
