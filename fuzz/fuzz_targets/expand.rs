#![no_main]
// Any bytes into expand(): must return Ok(original-looking data) or Err, never panic, never hang, never
// allocate past the limits. A hostile archive must not give back a "restored" file at all. The library's
// guard turns a panic into Error::Internal, which is the bug this target hunts, so that result fails too.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let opt = gpress::Options::default().max_input(1 << 24).max_stream(1 << 24);
    if let Err(gpress::Error::Internal(why)) = gpress::expand(data, &opt) {
        panic!("a panic reached the guard: {why}");
    }
});
