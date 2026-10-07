#![no_main]
// Our Flate decoder against arbitrary bytes: an error or a result, never a panic. When it decodes, the
// result must round-trip through our encoder and back.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(u) = gpress::flate::zlib::unwrap(data, 1 << 24) {
        let z = gpress::flate::deflate_zlib(&u.data, &gpress::flate::Options::quick());
        let back = gpress::flate::zlib::unwrap(&z, 1 << 24).expect("our own output must decode");
        assert_eq!(back.data, u.data);
    }
});
