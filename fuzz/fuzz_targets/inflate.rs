#![no_main]
// Our Flate decoder against arbitrary bytes: an error or a result, never a panic. When it decodes, the
// result must round-trip through our encoder and back, at the quick settings and under rules and an effort
// taken from the start of the input.
use gpress::flate::{deflate_zlib, zlib, Options};
use gpress::FlateRules;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (opt, data) = settings(data);
    if let Ok(u) = zlib::unwrap(data, 1 << 24) {
        round_trip(&u.data, &Options::quick());
        // At most 64 KiB: the slowest values in range do many times the default work.
        round_trip(&u.data[..u.data.len().min(1 << 16)], &opt);
    }
});

/// The start of the input: a count of rules (up to 7), then for each a byte that picks the rule and four
/// for its value (anywhere in its range), then a byte for the effort (above 100: the quick settings).
fn settings(data: &[u8]) -> (Options, &[u8]) {
    let (&count, mut rest) = data.split_first().unwrap_or((&0, &[]));
    let mut rules = FlateRules::default();
    for _ in 0..count % 8 {
        let Some((pick, after)) = rest.split_first_chunk::<5>() else { break };
        let (name, _, lowest, highest) = FlateRules::RULES[pick[0] as usize % FlateRules::RULES.len()];
        let value = u32::from_le_bytes([pick[1], pick[2], pick[3], pick[4]]) as u64;
        assert!(rules.set(name, lowest + value % (highest - lowest + 1)));
        rest = after;
    }
    let (&effort, rest) = rest.split_first().unwrap_or((&0, &[]));
    let opt = if effort > 100 { Options::quick_with(&rules) } else { Options::effort_with(effort, &rules) };
    (opt, rest)
}

fn round_trip(data: &[u8], opt: &Options) {
    let z = deflate_zlib(data, opt);
    let back = zlib::unwrap(&z, 1 << 24).expect("our own output must decode");
    assert_eq!(back.data, data);
}
