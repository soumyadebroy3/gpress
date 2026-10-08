#![no_main]
// Arbitrary bytes into the PDF reader and writer: a smaller file, the input untouched or a clean error,
// never a panic. The library's guard turns a panic into Error::Internal, which is the bug this target
// hunts, so that result fails the run too.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let opt = gpress::Options::default().effort(0).max_input(1 << 22).max_stream(1 << 22).threads(1);
    match gpress::optimize(data, &opt) {
        Ok(r) => {
            if let gpress::Outcome::Untouched(_) = r.outcome {
                assert_eq!(r.data, data);
            }
        }
        Err(gpress::Error::Internal(why)) => panic!("a panic reached the guard: {why}"),
        Err(_) => {}
    }
});
