//! Encrypted PDFs (files in tests/data/enc, made by make.py there).
//!
//! Files qpdf encrypted with the empty user password, in every standard mode, come out smaller with exactly
//! the same encryption (the key qpdf derives, the same dictionary and /ID) and the same decrypted objects, and
//! restore exactly from the peyaji format. Files that need a password, files readers disagree on and damaged
//! files are left untouched, and still pack and restore exactly.

use engine::flate;
use engine::pack::{pack, unpack};
use engine::pdf::object::{Dict, Obj};
use engine::pdf::rewrite::{optimize, Options, Outcome};
use engine::pdf::xref::{self, decode_fully, Doc};
use std::collections::BTreeMap;

fn file(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/enc/{name}.pdf", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The file key qpdf derives for a fixture (keys.txt).
fn qpdf_key(name: &str) -> Option<String> {
    let t = std::fs::read_to_string(format!("{}/tests/data/enc/keys.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
    t.lines().find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(' ')).map(|k| k.to_string()))
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// A fixture with `from` replaced by `to` (the same length, so every offset stays right).
fn edited(name: &str, from: &str, to: &str) -> Vec<u8> {
    replaced(file(name), from, to)
}

fn replaced(mut d: Vec<u8>, from: &str, to: &str) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let i = find(&d, from.as_bytes()).unwrap_or_else(|| panic!("{from}"));
    d[i..i + from.len()].copy_from_slice(to.as_bytes());
    d
}

/// A fixture with one hex digit changed: the first one after `key` (e.g. "/U <").
fn one_digit_changed(name: &str, key: &str) -> Vec<u8> {
    let mut d = file(name);
    let i = find(&d, key.as_bytes()).unwrap_or_else(|| panic!("{name}: {key}")) + key.len();
    d[i] = if d[i] == b'0' { b'1' } else { b'0' };
    d
}

/// Where the data of stream object `num` lies in a fixture (written "N 0 obj", "stream" and a line feed).
fn stream_data(d: &[u8], num: u32) -> (usize, usize) {
    let doc = xref::open(d).unwrap();
    let off = match doc.entries.get(&num) {
        Some(&xref::Entry::Offset(off, _)) => off,
        e => panic!("{e:?}"),
    };
    let (_, _, o, _) = engine::pdf::parse::parse_indirect(d, off, &|l| l.as_int().map(|i| i as usize)).unwrap();
    let len = match o {
        Obj::Stream(s) => s.data.len(),
        _ => panic!("object {num} is not a stream"),
    };
    (off + find(&d[off..], b"stream\n").unwrap() + 7, len)
}

fn opts() -> Options {
    Options { flate: flate::Options::quick(), ..Options::default() }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Everything reachable from the trailer as a reader sees it: decrypted, streams decoded through their filters
/// and compared without /Length, /Filter and /DecodeParms.
fn values(doc: &Doc) -> BTreeMap<u32, Obj> {
    let mut out = BTreeMap::new();
    let mut todo = Vec::new();
    for (k, v) in &doc.trailer.0 {
        if !matches!(k.as_slice(), b"Prev" | b"XRefStm" | b"Size" | b"Encrypt" | b"Index" | b"W" | b"Type" | b"Filter" | b"DecodeParms" | b"Length") {
            v.refs(&mut todo);
        }
    }
    while let Some((n, _)) = todo.pop() {
        if out.contains_key(&n) {
            continue;
        }
        let o = doc.load(n).unwrap().1;
        let mut refs = Vec::new();
        let o = match o {
            Obj::Stream(s) => {
                let data = decode_fully(&s.dict, &s.data, 1 << 24).unwrap();
                let d = Dict(s.dict.0.into_iter().filter(|(k, _)| !matches!(k.as_slice(), b"Length" | b"Filter" | b"DecodeParms")).collect());
                let o = Obj::Array(vec![Obj::Dict(d), Obj::Str(data)]);
                o.refs(&mut refs);
                o
            }
            o => {
                o.refs(&mut refs);
                o
            }
        };
        todo.extend(refs);
        out.insert(n, o);
    }
    out
}

const HANDLED: [(&str, &str); 11] = [
    ("r2_rc4_40", "R2/RC4"),
    ("r3_rc4_128", "R3/RC4"),
    ("r4_rc4_128", "R4/RC4"),
    ("r4_aes_128", "R4/AESV2"),
    ("r4_aes_cleartext_md", "R4/AESV2"),
    ("r5_aes_256", "R5/AESV3"),
    ("r6_aes_256", "R6/AESV3"),
    ("r6_restricted", "R6/AESV3"),
    ("r6_objstm", "R6/AESV3"),
    ("rc4_40_plain_title", "R2/RC4"),
    ("v4_rc4_ok", "R4/RC4"),
];

#[test]
fn every_standard_mode_is_rewritten_with_the_same_encryption() {
    for (name, how) in HANDLED {
        let input = file(name);
        let r = optimize(&input, &opts());
        match &r.outcome {
            Outcome::Smaller(s) => assert_eq!(s.encryption, how, "{name}"),
            Outcome::Untouched(why) => panic!("{name}: untouched: {why}"),
        }
        let a = xref::open(&input).unwrap();
        let b = xref::open(&r.data).unwrap();
        let (ca, cb) = match (&a.crypt, &b.crypt) {
            (Some(Ok(ca)), Some(Ok(cb))) => (ca, cb),
            x => panic!("{name}: {x:?}"),
        };
        // The same handler: file key (the one qpdf derives), dictionary, methods and permissions; the same /ID.
        assert_eq!(ca, cb, "{name}");
        if let Some(k) = qpdf_key(name) {
            assert_eq!(hex(&ca.key), k, "{name}");
        }
        assert_eq!(a.trailer.get(b"ID"), b.trailer.get(b"ID"), "{name}");
        // The same objects, decrypted.
        assert_eq!(values(&a), values(&b), "{name}");
        // Strings and object streams really are encrypted in the output.
        assert!(find(&r.data, b"A note").is_none() && find(&r.data, b"Hello encrypted").is_none(), "{name}");
        // Metadata left unencrypted stays readable as it was; encrypted metadata is compressed now.
        let clear_xmp = |d: &[u8]| find(d, b"<x:xmpmeta").is_some();
        assert_eq!(clear_xmp(&input), clear_xmp(&r.data), "{name}");
        // The same output every time (initialization vectors are derived, not random).
        assert_eq!(optimize(&input, &opts()).data, r.data, "{name}");
    }
}

#[test]
fn files_that_need_a_password_or_that_readers_disagree_on_are_left_untouched() {
    let r4 = "r4_aes_128";
    let r6 = "r6_aes_256";
    // Object 7 of r4_aes_128 is the page content: a change in its last byte but 16 changes the last decrypted
    // byte (CBC), which breaks the AES padding.
    let bad_aes = {
        let mut d = file(r4);
        let (p, len) = stream_data(&d, 7);
        d[p + len - 17] ^= 0x80;
        d
    };
    // The same stream with a crypt filter of its own.
    let own_filter = {
        let d = file(r4);
        let (p, _) = stream_data(&d, 7);
        let i = d[..p].windows(20).rposition(|w| w == b"/Filter /FlateDecode").unwrap();
        let mut d = d;
        d[i..i + 20].copy_from_slice(b"/Filter /Crypt      ");
        d
    };
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("needs a password", file("needs_password"), "needs a password"),
        ("raw CR in an encrypted string", file("raw_cr"), "raw carriage return"),
        ("raw CR LF in an encrypted string", file("raw_crlf"), "raw carriage return"),
        ("a crypt filter named Identity", file("identity_cf"), "named Identity"),
        ("/CFM /None", file("cfm_none"), "unsupported crypt filter"),
        ("different filters for streams and strings", file("mixed_filters"), "different filters"),
        ("no key length anywhere (V4)", file("v4_aes_no_length"), "key length given nowhere"),
        (
            "no key length anywhere (V5)",
            replaced(edited(r6, "/Filter /Standard /Length 256 /O", "/Filter /Standard             /O"), "/AESV3 /Length 32 >>", "/AESV3            >>"),
            "key length given nowhere",
        ),
        ("a changed /U", one_digit_changed(r4, "/U <"), "needs a password"),
        ("a changed /U (R6)", one_digit_changed(r6, "/U <"), "needs a password"),
        ("a changed /Perms", one_digit_changed(r6, "/Perms <"), "/Perms"),
        ("a changed /P", edited(r4, "/P -4 ", "/P -8 "), "needs a password"),
        ("a changed /CFM", edited(r4, "/CFM /AESV2", "/CFM /AESV3"), "unsupported crypt filter"),
        ("a changed /V", edited(r4, "/V 4 >>", "/V 5 >>"), "unsupported /V 5 /R 4"),
        ("no /ID", edited("r2_rc4_40", "/ID [", "/XD ["), "/ID"),
        ("a stream with its own crypt filter", own_filter, "own crypt filter"),
        ("damaged AES data", bad_aes, "bad AES padding"),
        ("a signed file (its signature stored unencrypted)", file("signed_rc4"), "digitally signed"),
    ];
    for (what, input, why) in cases {
        let r = optimize(&input, &opts());
        match &r.outcome {
            Outcome::Untouched(w) => assert!(w.contains(why), "{what}: {w}"),
            Outcome::Smaller(_) => panic!("{what}: rewritten"),
        }
        assert_eq!(r.data, input, "{what}");
        // The peyaji format restores them exactly too.
        let (packed, _) = pack(&input);
        assert_eq!(unpack(&packed).unwrap(), input, "{what}");
    }
    // The key length may be given in either place.
    for (from, to) in [("/Filter /Standard /Length 256 /O", "/Filter /Standard             /O"), ("/AESV3 /Length 32 >>", "/AESV3            >>")] {
        let input = edited(r6, from, to);
        assert!(matches!(optimize(&input, &opts()).outcome, Outcome::Smaller(_)), "{from}");
    }
}

#[test]
fn peyaji_restores_encrypted_files_exactly_and_refuses_damaged_packs() {
    let mut total = (0, 0);
    for (name, _) in HANDLED {
        let input = file(name);
        let (packed, stats) = pack(&input);
        assert_eq!(unpack(&packed).unwrap(), input, "{name}");
        // The streams of qpdf's files are stored decrypted (that is where the gain is).
        if name.starts_with('r') && name.as_bytes()[1].is_ascii_digit() {
            assert!(stats.decrypted > 0 && !stats.fallback, "{name}: {stats:?}");
        }
        total.0 += input.len();
        total.1 += packed.len();
    }
    assert!(total.1 * 2 < total.0, "{total:?}");
    // Damaged packs of an encrypted file: an error, or the original exactly; never anything else. The file key
    // and the first stream's settings are coded near the start.
    for (name, bit, every, cut_every) in [("r4_aes_128", 0, 9, 13), ("r2_rc4_40", 7, 0, 29)] {
        let input = file(name);
        let (packed, stats) = pack(&input);
        assert!(stats.decrypted > 0, "{name}");
        let mut cases: Vec<Vec<u8>> = Vec::new();
        for i in (25..packed.len()).filter(|i| *i < 110 || (every > 0 && i % every == 0)) {
            let mut b = packed.clone();
            b[i] ^= 1 << if i < 110 { bit } else { 5 };
            cases.push(b);
        }
        for cut in (0..packed.len()).step_by(cut_every) {
            cases.push(packed[..cut].to_vec());
        }
        for (k, c) in cases.iter().enumerate() {
            if let Ok(out) = unpack(c) {
                assert_eq!(&out, &input, "{name}: damaged case {k} restored to something else");
            }
        }
    }
}

#[test]
fn unencrypted_files_with_a_raw_carriage_return_in_a_string_are_left_untouched() {
    // Readers disagree on the value (qpdf reads a line feed; pdf.js, PDFium and Poppler keep the carriage
    // return), so a rewrite would change it for some of them. An escaped line end is fine.
    let base = file("base");
    for (title, untouched) in [(&b"(Encrypted\rtest)"[..], true), (b"(Encrypted\\\ntst)", false), (b"(Encrypted test)", false)] {
        let mut d = base.clone();
        let i = find(&d, b"(Encrypted test)").unwrap();
        assert_eq!(title.len(), 16);
        d[i..i + 16].copy_from_slice(title);
        let r = optimize(&d, &opts());
        assert_eq!(matches!(r.outcome, Outcome::Untouched(_)), untouched, "{}", String::from_utf8_lossy(title));
    }
}
