#![no_main]
// Arbitrary bytes into the PDF reader and writer: a smaller file, the input untouched or a clean error,
// never a panic. The library's guard turns a panic into Error::Internal, which is the bug this target
// hunts, so that result fails the run too.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let opt = gpress::Options::default().effort(0).max_input(1 << 22).max_stream(1 << 22).threads(1);
    // Lossless, with the lossy image step at its strongest level, and with every floor asked lower and a value
    // for each rule taken from the input's last bytes: 0 leaves the default, any other byte is a point of the
    // rule's range, 1 its lowest value and 255 its highest.
    // The rules count at the engine level the first byte picks: the fast path, the bridge to 20 or the budgets.
    let level = [0, 15, 40][usize::from(data.first().copied().unwrap_or(0)) % 3];
    let mut ruled = opt.clone().engine_effort(Some(level));
    ruled = ruled.lossy(Some(gpress::Lossy { photo_ppi: 1, art_ppi: 1, ..gpress::Lossy::level(3) }));
    for ((name, _, lowest, highest), &b) in gpress::Options::rules().iter().zip(data.iter().rev()) {
        if b > 0 {
            let value = lowest + ((highest - lowest) as u128 * (b as u128 - 1) / 254) as u64;
            ruled.set_rule(name, value).expect("a value of the rule's range");
        }
    }
    for opt in [opt.clone(), opt.lossy(Some(gpress::Lossy::level(3))), ruled] {
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
