#![no_main]
// Arbitrary bytes into the PDF reader and writer: a smaller file or the input untouched, never a panic.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let opt = gpress::Options::default().effort(0).max_input(1 << 22).max_stream(1 << 22).threads(1);
    if let Ok(r) = gpress::optimize(data, &opt) {
        if let gpress::Outcome::Untouched(_) = r.outcome {
            assert_eq!(r.data, data);
        }
    }
});
