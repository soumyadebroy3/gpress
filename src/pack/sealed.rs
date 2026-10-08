//! Encrypted streams in the gpress archive. In a file the empty password opens, each encrypted stream is stored
//! decrypted, so the usual plan applies to it (zlib rebuild, Lepton, the model), and is encrypted
//! again on restore. A stream is stored this way only when encrypting its decrypted bytes again gives exactly
//! the stored bytes (RC4 always does; AES keeps the stream's initialization vector and needs standard padding).
//!
//! The decoder restores the file front to back and can't read the trailer first, so the pack carries what it
//! needs: the file key once (part T_CRYPT_KEY; it isn't secret, since the empty password opens the file) and,
//! before each such stream, its method, its object number and generation when its key needs them (usually
//! "those of the `N G obj` just before"), and its initialization vector (part T_CRYPT).

use super::*;
use crate::pdf::crypt::{self, Method};
use crate::pdf::object::is_regular;
use crate::pdf::xref;

/// What encrypting a stream's payload again needs, besides the file key.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Sealed {
    pub method: Method,
    pub num: u32,
    pub gen: u16,
    /// AES: the stream's initialization vector (its first 16 bytes).
    pub iv: [u8; 16],
}

/// For files the empty password opens: every raw stream that decrypts, and encrypts back to exactly its stored
/// bytes, becomes its settings followed by a plan for its decrypted payload; the file key comes first. Other
/// files keep their plans unchanged (so they encode exactly as before).
pub(super) fn crypt_plans(d: &[u8], plans: Vec<Plan>, opt: &PackOptions) -> Vec<Plan> {
    if find(d, b"/Encrypt", 0).is_none() {
        return plans;
    }
    let doc = match xref::open(d) {
        Ok(doc) => doc,
        Err(_) => return plans,
    };
    let c = match &doc.crypt {
        Some(Ok(c)) => c,
        _ => return plans,
    };
    let mut out = Vec::with_capacity(plans.len() + 8);
    let mut key_sent = false;
    let objects = ObjIndex::new();
    for p in plans {
        if let Plan::Raw(a, b) = p {
            if let Some((s, dict, plain)) = decrypt(c, d, a, b) {
                if !key_sent {
                    out.push(Plan::CryptKey(c.key.clone()));
                    key_sent = true;
                }
                out.push(Plan::Crypt(s));
                out.push(plan_bytes(plain, dict, a, &file_resolver(d, &objects), opt));
                continue;
            }
        }
        out.push(p);
    }
    out
}

/// The stream payload `d[a..b]` decrypted, with the stream's dictionary, if it is encrypted and encrypting it
/// again gives the same bytes.
fn decrypt(c: &crypt::Crypt, d: &[u8], a: usize, b: usize) -> Option<(Sealed, Dict, Vec<u8>)> {
    let (num, gen, dict) = stream_header(&d[..a])?;
    let method = c.stream_method(num, &dict).ok()?;
    if method == Method::Identity || c.is_own(num) {
        return None;
    }
    let data = &d[a..b];
    let plain = crypt::unseal(&c.key, method, num, gen, data).ok()?;
    // Nothing to gain from an empty payload (and the decoder refuses one).
    if plain.is_empty() {
        return None;
    }
    let mut iv = [0u8; 16];
    if method != Method::Rc4 {
        iv.copy_from_slice(&data[..16]);
    }
    let s = Sealed { method, num, gen, iv };
    if seal_payload(&s, &c.key, &plain)? != data {
        return None;
    }
    Some((s, dict, plain))
}

/// The plan for a decrypted payload that starts at byte `at` of the file: the same choices `plan` makes for
/// a stream's bytes (`stream_plan`, then a fax part for CCITT data), or the bytes as they are.
fn plan_bytes(raw: Vec<u8>, dict: Dict, at: usize, resolve: &dyn Fn(&Obj) -> Option<Obj>, opt: &PackOptions) -> Plan {
    if let Some(p) = stream_plan(&raw, opt) {
        return p;
    }
    match fax_parse(&raw, dict, resolve) {
        Some(parsed) => Plan::Fax { parsed: Box::new(parsed), at, raw: None },
        None => Plan::Plain(raw),
    }
}

/// `N G obj <<dictionary>> stream` just before a stream's payload, which starts at the end of `d`: the object
/// number, generation and dictionary. Looks back at most 16 KiB (the decoder runs this for every encrypted
/// stream, so its cost stays bounded); None when it isn't found. The decoder calls it on what it has restored
/// so far, which is the same bytes.
pub(super) fn stream_header(d: &[u8]) -> Option<(u32, u16, Dict)> {
    let k = if d.ends_with(b"stream\r\n") {
        d.len() - 8
    } else if d.ends_with(b"stream\n") {
        d.len() - 7
    } else {
        return None;
    };
    let lo = k.saturating_sub(16 << 10);
    let mut end = k;
    // The nearest "obj" whose dictionary runs exactly up to the keyword (a few tries: "obj" may appear
    // inside the dictionary, or end an earlier "endobj").
    for _ in 0..4 {
        let o = lo + d[lo..end].windows(3).rposition(|w| w == b"obj")?;
        end = o;
        let mut lx = Lexer::new(&d[..k], o + 3);
        let dict = match lx.parse_obj() {
            Ok(Obj::Dict(dd)) => dd,
            _ => continue,
        };
        lx.skip_ws();
        if lx.pos != k {
            continue;
        }
        if let Some((num, gen)) = header_numbers(&d[..o]) {
            return Some((num, gen, dict));
        }
    }
    None
}

/// The "N G " before an "obj" keyword that starts at the end of `d`.
fn header_numbers(d: &[u8]) -> Option<(u32, u16)> {
    let white = |c: u8| matches!(c, 0 | 9 | 10 | 12 | 13 | 32);
    let mut i = d.len();
    // Backwards: white space and the generation, then white space and the object number.
    let mut fields = [0u64; 2];
    for f in fields.iter_mut() {
        let s = i;
        while i > 0 && white(d[i - 1]) {
            i -= 1;
        }
        if i == s {
            return None;
        }
        let e = i;
        while i > 0 && d[i - 1].is_ascii_digit() && e - i < 10 {
            i -= 1;
        }
        if i == e {
            return None;
        }
        *f = std::str::from_utf8(&d[i..e]).ok()?.parse().ok()?;
    }
    // The number starts a token.
    if i > 0 && is_regular(d[i - 1]) {
        return None;
    }
    let (gen, num) = (fields[0], fields[1]);
    if gen > u16::MAX as u64 || num > u32::MAX as u64 {
        return None;
    }
    Some((num as u32, gen as u16))
}

/// The payload encrypted again.
pub(super) fn seal_payload(s: &Sealed, key: &[u8], plain: &[u8]) -> Option<Vec<u8>> {
    crypt::seal(key, s.method, s.num, s.gen, &s.iv, plain).ok()
}

/// Codes the file key (part T_CRYPT_KEY): its length, then its bytes. None if the file is damaged.
pub(super) fn code_key<C: Coder>(io: &mut Io<C>, key_in: Option<&[u8]>) -> Option<Vec<u8>> {
    let n = io.byte(key_in.map(|k| k.len() as u8)) as usize;
    if !(5..=16).contains(&n) && n != 32 {
        return None;
    }
    io.m.set_kind(Kind::Raw, Geom::default(), 0);
    io.m.set_flat(true);
    let mut key = Vec::with_capacity(n);
    io.bytes(key_in, n, &mut key);
    io.m.set_flat(false);
    if io.damaged || key.len() != n {
        return None;
    }
    Some(key)
}

/// Codes an encrypted stream's settings (part T_CRYPT). `out` is the file restored so far (it ends just before
/// the payload). None if the file is damaged.
pub(super) fn code_settings<C: Coder>(io: &mut Io<C>, s_in: Option<&Sealed>, out: &[u8], key: Option<&[u8]>) -> Option<Sealed> {
    let code = |m: Method| match m {
        Method::Rc4 => 1u8,
        Method::Aes128 => 2,
        Method::Aes256 => 3,
        Method::Identity => 0,
    };
    let method = match io.byte(s_in.map(|s| code(s.method))) {
        1 => Method::Rc4,
        2 => Method::Aes128,
        3 => Method::Aes256,
        _ => return None,
    };
    let key = key?;
    let key_fits = match method {
        Method::Rc4 => (5..=16).contains(&key.len()),
        Method::Aes128 => key.len() == 16,
        _ => key.len() == 32,
    };
    if !key_fits {
        return None;
    }
    let (mut num, mut gen) = (0u32, 0u16);
    if method != Method::Aes256 {
        // The object number and generation, usually those of the "N G obj" just before (flag 1).
        let found = stream_header(out).map(|h| (h.0, h.1));
        match io.byte(s_in.map(|s| (found == Some((s.num, s.gen))) as u8)) {
            1 => {
                let f = found?;
                num = f.0;
                gen = f.1;
            }
            0 => {
                let n = io.varint(s_in.map(|s| s.num as u64));
                let g = io.varint(s_in.map(|s| s.gen as u64));
                if n > u32::MAX as u64 || g > u16::MAX as u64 {
                    return None;
                }
                num = n as u32;
                gen = g as u16;
            }
            _ => return None,
        }
    }
    let mut iv = [0u8; 16];
    if method != Method::Rc4 {
        io.m.set_kind(Kind::Raw, Geom::default(), 0);
        io.m.set_flat(true);
        let mut v = Vec::with_capacity(16);
        io.bytes(s_in.map(|s| &s.iv[..]), 16, &mut v);
        io.m.set_flat(false);
        if io.damaged || v.len() != 16 {
            return None;
        }
        iv.copy_from_slice(&v);
    }
    Some(Sealed { method, num, gen, iv })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_object_before_a_stream() {
        // "obj" inside the dictionary twice (a name and a string) and in the "endobj" before it.
        let d = b"%PDF-1.4\n7 0 obj\n<< /Length 5 >>\nendobj\n12 3 obj\n<< /Name /obj /S (obj) /Length 4 >>\nstream\r\n";
        let (n, g, dict) = stream_header(d).unwrap();
        assert_eq!((n, g), (12, 3));
        assert_eq!(dict.get(b"Length"), Some(&Obj::Int(4)));
        assert_eq!(stream_header(b"1 0 obj<</Length 3>>stream\n").map(|h| (h.0, h.1)), Some((1, 0)));
        // Not a stream start, no numbers, a number glued to a word, a dictionary that doesn't reach the keyword.
        for bad in [&b"1 0 obj<</Length 3>>streamx"[..], b"obj<</Length 3>>stream\n", b"x1 0 obj<<>>stream\n", b"1 0 obj<<>> 5 stream\n", b""] {
            assert!(stream_header(bad).is_none(), "{}", String::from_utf8_lossy(bad));
        }
        // The header is looked for in the last 16 KiB only.
        let mut far = b"1 0 obj\n<< /Length 3 /Pad (".to_vec();
        far.extend(std::iter::repeat_n(b'x', 20000));
        far.extend_from_slice(b") >>\nstream\n");
        assert!(stream_header(&far).is_none());
    }

    /// A pack of the given parts, coded as the encoder codes them (the encoder stops where the decoder would
    /// refuse, so the pack ends there).
    fn pack_parts(d: &[u8], plans: Vec<Plan>) -> Vec<u8> {
        let mut stats = PackStats::default();
        let (_, enc) = run_plans(Encoder::new(), Some(d), Some(plans), d.len(), &PackOptions::default(), &mut stats);
        let mut out = header(VERSION, d);
        out.extend_from_slice(&enc.finish());
        out
    }

    #[test]
    fn decryption_is_kept_only_when_it_pays() {
        // One tiny AES stream: the key and its initialization vector cost more than decrypting it saves, so the
        // pack without decryption is kept.
        let tiny = include_bytes!("../../tests/data/enc/aes_tiny_stream.pdf");
        let (with, with_stats) = pack_pass(tiny, &PackOptions::default());
        let (packed, stats) = pack(tiny);
        assert_eq!((with_stats.decrypted, stats.decrypted), (1, 0));
        assert!(packed.len() < with.len(), "{} {}", packed.len(), with.len());
        assert_eq!(unpack(&packed).unwrap(), tiny);
        // Larger encrypted streams: decrypted, and at least a quarter smaller than without.
        let r4 = include_bytes!("../../tests/data/enc/r4_aes_128.pdf");
        let (packed, stats) = pack(r4);
        let (plain, _) = pack_pass(r4, &PackOptions { decrypt: false, ..PackOptions::default() });
        assert!(stats.decrypted == 3 && packed.len() * 4 < plain.len() * 3, "{} {}", packed.len(), plain.len());
    }

    fn rc4_settings() -> Plan {
        Plan::Crypt(Sealed { method: Method::Rc4, num: 1, gen: 0, iv: [0; 16] })
    }

    #[test]
    fn honest_crypt_parts_restore() {
        let d = b"%PDF-1.4\n1 0 obj\n<< /Length 5 >>\nstream\nHELLO\nendstream\nendobj\n%%EOF\n";
        let a = d.windows(6).position(|w| w == b"HELLO\n").unwrap();
        let key = vec![7u8; 16];
        let s = Sealed { method: Method::Rc4, num: 1, gen: 0, iv: [0; 16] };
        // The file with its stream encrypted (RC4 keeps the length).
        let mut x = d[..a].to_vec();
        x.extend_from_slice(&seal_payload(&s, &key, b"HELLO").unwrap());
        x.extend_from_slice(&d[a + 5..]);
        let plans = vec![Plan::Syntax(0, a), Plan::CryptKey(key), Plan::Crypt(s), Plan::Plain(b"HELLO".to_vec()), Plan::Syntax(a + 5, x.len())];
        let packed = pack_parts(&x, plans);
        assert_eq!(unpack(&packed).unwrap(), x);
    }

    #[test]
    fn crypt_parts_that_make_no_progress_are_refused() {
        // A long run of syntax, then a stream whose header is far away (the slowest case for the decoder).
        let mut d = b"%PDF-1.4\n%".to_vec();
        d.extend(std::iter::repeat_n(b'x', 70000));
        d.extend_from_slice(b"\n<<>>stream\n");
        let a = d.len();
        d.extend_from_slice(b"\nendstream\n%%EOF\n");
        let tail = Plan::Syntax(a, d.len());
        let key = || Plan::CryptKey(vec![7u8; 16]);
        let mut cases: Vec<Vec<Plan>> = Vec::new();
        // Settings followed by empty payloads, many times (each pair restored nothing before this fix).
        let mut v = vec![Plan::Syntax(0, a), key()];
        for _ in 0..100_000 {
            v.push(rc4_settings());
            v.push(Plan::Plain(Vec::new()));
        }
        v.push(Plan::Syntax(a, d.len()));
        cases.push(v);
        // Empty syntax parts, many times (the same loop without encryption).
        let mut v = vec![Plan::Syntax(0, a)];
        v.extend((0..100_000).map(|_| Plan::Syntax(a, a)));
        v.push(Plan::Syntax(a, d.len()));
        cases.push(v);
        // Two keys; settings not right after syntax; a key not followed by settings; settings at the end.
        cases.push(vec![Plan::Syntax(0, a), key(), rc4_settings(), Plan::Plain(b"x".to_vec()), Plan::Syntax(a, a + 1), key(), rc4_settings(), Plan::Plain(b"y".to_vec()), tail]);
        cases.push(vec![Plan::Syntax(0, a), key(), rc4_settings(), Plan::Plain(b"x".to_vec()), Plan::Syntax(a, a + 1), Plan::Raw(a, a), rc4_settings(), Plan::Plain(b"y".to_vec())]);
        cases.push(vec![Plan::Syntax(0, a), key(), Plan::Syntax(a, d.len())]);
        cases.push(vec![Plan::Syntax(0, d.len()), key(), rc4_settings()]);
        // Settings without a key.
        cases.push(vec![Plan::Syntax(0, a), rc4_settings(), Plan::Plain(b"x".to_vec()), Plan::Syntax(a, d.len())]);
        for (k, plans) in cases.into_iter().enumerate() {
            let t = std::time::Instant::now();
            let packed = pack_parts(&d, plans);
            assert!(unpack(&packed).is_err(), "case {k}");
            assert!(t.elapsed().as_secs() < 20, "case {k} took {:?}", t.elapsed());
        }
    }
}
