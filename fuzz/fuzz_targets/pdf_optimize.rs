#![no_main]
// Arbitrary bytes into the PDF reader and writer: a smaller file, the input untouched or a clean error,
// never a panic. The library's guard turns a panic into Error::Internal, which is the bug this target
// hunts, so that result fails the run too.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let opt = gpress::Options::default().effort(0).max_input(1 << 22).max_stream(1 << 22).threads(1);
    // Lossless, with the lossy image step at its strongest level, and with that step's rules at their loosest
    // (1 counts as the least a rule can be): smaller images, the lowest floors, any JPEG kept.
    let mut loose = opt.clone().lossy(Some(gpress::Lossy { photo_ppi: 1, art_ppi: 1, ..gpress::Lossy::level(3) }));
    for name in ["min_pixels", "photo_floor", "art_floor", "resample_margin", "quality_min", "min_psnr", "scan_cover"] {
        loose.lossy_rules.set(name, 1);
    }
    for opt in [opt.clone(), opt.lossy(Some(gpress::Lossy::level(3))), loose] {
        match gpress::optimize(data, &opt) {
            Ok(r) => {
                if let gpress::Outcome::Untouched(_) = r.outcome {
                    assert_eq!(r.data, data);
                }
            }
            Err(gpress::Error::Internal(why)) => panic!("a panic reached the guard: {why}"),
            Err(_) => {}
        }
    }
});
