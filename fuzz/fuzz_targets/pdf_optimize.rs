#![no_main]
// Arbitrary bytes into the PDF reader and writer: a smaller file, the input untouched or a clean error,
// never a panic. The library's guard turns a panic into Error::Internal, which is the bug this target
// hunts, so that result fails the run too.
//
// The first byte picks the effort (0, 15 or 40) and how many of the rewrite's rules are set (0 to 4); each
// rule then takes two bytes, its place in `RewriteRules::RULES` and where its value lies in the rule's range
// (0 the lowest, 255 the highest). The rest is the PDF.
use gpress::pdf::rules::RewriteRules;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&first, rest)) = data.split_first() else { return };
    let effort = [0, 15, 40][usize::from(first / 5) % 3];
    let Some((picks, data)) = rest.split_at_checked(2 * usize::from(first % 5)) else { return };
    let mut opt = gpress::Options::default().effort(effort).max_input(1 << 22).max_stream(1 << 22).threads(1);
    for pick in picks.chunks(2) {
        let (name, _, lowest, highest) = RewriteRules::RULES[usize::from(pick[0]) % RewriteRules::RULES.len()];
        assert!(opt.rewrite_rules.set(name, lowest + (highest - lowest) * u64::from(pick[1]) / 255));
    }
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
