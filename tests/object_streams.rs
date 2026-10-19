//! Object streams of a file to optimize or pack: each decodes to at most the stream limit, is decoded once and
//! let go as soon as its objects are read, the ones held at once and all of them together are bounded, and each
//! object is read within its own bytes. Files outside these bounds are left as they are; others come out as
//! before.

use gpress::flate::zlibexact::{zlib_stream, Params};
use gpress::pack::{pack_with, unpack, PackOptions};
use gpress::pdf::object::Obj;
use gpress::pdf::rewrite::{optimize, Optimized, Options, Outcome};
use gpress::pdf::xref::{self, Limits};

fn z(d: &[u8]) -> Vec<u8> {
    zlib_stream(d, Params { level: 6, window_bits: 15, mem_level: 8, filtered: false })
}

/// The decoded bytes of an object stream holding `objs` (object number, body) one per line, padded with spaces
/// to `len` bytes; and its /First.
fn objstm_raw(objs: &[(u32, Vec<u8>)], len: usize) -> (Vec<u8>, usize) {
    let mut head = String::new();
    let mut body = Vec::new();
    for (n, b) in objs {
        if !body.is_empty() {
            body.push(b'\n');
        }
        head.push_str(&format!("{n} {} ", body.len()));
        body.extend_from_slice(b);
    }
    let first = head.len();
    let mut raw = head.into_bytes();
    raw.extend_from_slice(&body);
    if raw.len() < len {
        raw.resize(len, b' ');
    }
    (raw, first)
}

/// A PDF with a cross-reference stream: `plain` objects at offsets, object streams (number, decoded bytes, /N,
/// /First) behind two Flate stages, and `rows` placing objects in them (object number, object stream, index).
fn build(plain: &[(u32, Vec<u8>)], objstms: &[(u32, Vec<u8>, usize, usize)], rows: &[(u32, u32, u32)]) -> Vec<u8> {
    let mut d = b"%PDF-1.7\n".to_vec();
    let mut at: Vec<(u32, u8, u32, u32)> = Vec::new();
    for (n, b) in plain {
        at.push((*n, 1, d.len() as u32, 0));
        d.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        d.extend_from_slice(b);
        d.extend_from_slice(b"\nendobj\n");
    }
    for (sn, raw, count, first) in objstms {
        let data = z(&z(raw));
        at.push((*sn, 1, d.len() as u32, 0));
        d.extend_from_slice(
            format!(
                "{sn} 0 obj\n<< /Type /ObjStm /N {count} /First {first} /Filter [/FlateDecode /FlateDecode] \
                 /Length {:>10} >>\nstream\n",
                data.len()
            )
            .as_bytes(),
        );
        d.extend_from_slice(&data);
        d.extend_from_slice(b"\nendstream\nendobj\n");
    }
    for &(n, sn, i) in rows {
        at.push((n, 2, sn, i));
    }
    let xn = at.iter().map(|r| r.0).max().unwrap() + 1;
    at.push((xn, 1, d.len() as u32, 0));
    let x = d.len();
    let mut e = Vec::new();
    for n in 0..=xn {
        let (t, a, b) =
            at.iter().find(|r| r.0 == n).map_or((0, 0, if n == 0 { 65535 } else { 0 }), |r| (r.1, r.2, r.3));
        e.push(t);
        e.extend_from_slice(&a.to_be_bytes());
        e.extend_from_slice(&(b as u16).to_be_bytes());
    }
    let ze = z(&e);
    d.extend_from_slice(
        format!(
            "{xn} 0 obj\n<< /Type /XRef /Size {} /W [1 4 2] /Root 1 0 R /Filter /FlateDecode /Length {} >>\nstream\n",
            xn + 1,
            ze.len()
        )
        .as_bytes(),
    );
    d.extend_from_slice(&ze);
    d.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    d
}

const PAGES: &[u8] = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
const PAGE: &[u8] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>";

/// The catalog (referring to `refs` too), the pages and a page, and with `pad` an image of that many bytes that
/// don't compress, so that the file is larger.
fn catalog(refs: impl Iterator<Item = u32>, pad: usize) -> Vec<(u32, Vec<u8>)> {
    let refs: Vec<String> = refs.map(|n| format!("{n} 0 R")).collect();
    let mut plain = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R /C [{}] >>", refs.join(" ")).into_bytes()),
        (2, PAGES.to_vec()),
        (3, PAGE.to_vec()),
    ];
    if pad > 0 {
        let mut x = 0x2545_f491_u32;
        let noise: Vec<u8> = (0..pad)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let mut s = format!("<< /Type /XObject /Subtype /Image /Width {pad} /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {pad} >>\nstream\n").into_bytes();
        s.extend_from_slice(&noise);
        s.extend_from_slice(b"\nendstream");
        plain.push((4, s));
        plain[0].1 = format!("<< /Type /Catalog /Pages 2 0 R /C [{} 4 0 R] >>", refs.join(" ")).into_bytes();
    }
    plain
}

/// `k` object streams with the same `len` decoded bytes. Each lists objects 1000..1000+k (the catalog refers to
/// them) and 2000..2000+k (nothing does); the cross-reference data places 1000+i in stream 100+i, and with
/// `unread` 2000+i there too, so that no stream is ever read through.
fn streams(k: u32, len: usize, unread: bool, pad: usize) -> Vec<u8> {
    let objs: Vec<(u32, Vec<u8>)> = (0..k)
        .map(|i| (1000 + i, format!("<< /K {i} >>").into_bytes()))
        .chain((0..k).map(|i| (2000 + i, format!("<< /U {i} >>").into_bytes())))
        .collect();
    let (raw, first) = objstm_raw(&objs, len);
    let stms: Vec<_> = (0..k).map(|i| (100 + i, raw.clone(), objs.len(), first)).collect();
    let mut rows: Vec<(u32, u32, u32)> = (0..k).map(|i| (1000 + i, 100 + i, i)).collect();
    if unread {
        rows.extend((0..k).map(|i| (2000 + i, 100 + i, k + i)));
    }
    build(&catalog((0..k).map(|i| 1000 + i), pad), &stms, &rows)
}

fn opt(max_stream: usize) -> Options {
    Options { max_stream, threads: 2, ..Options::effort(0) }
}

fn untouched(r: &Optimized, input: &[u8]) -> String {
    assert_eq!(r.data, input);
    match &r.outcome {
        Outcome::Untouched(why) => why.clone(),
        o => panic!("{o:?}"),
    }
}

fn smaller(r: &Optimized, input: &[u8]) {
    assert!(matches!(r.outcome, Outcome::Smaller(_)), "{:?}", r.outcome);
    assert!(r.data.len() < input.len());
}

#[test]
fn object_streams_decode_within_max_stream_and_are_let_go_once_read() {
    // Eight object streams of 512 KiB, every object in them read: one is held at a time. (With a max_stream of
    // 1 MiB they would decode to more than 4 MiB in all, stages counted, which a file of a few KB may not.)
    let pdf = streams(8, 512 << 10, false, 0);
    let full = optimize(&pdf, &opt(64 << 20));
    smaller(&full, &pdf);
    assert_eq!(optimize(&pdf, &opt(2 << 20)).data, full.data, "within the limits the output is the same");
    assert_eq!(optimize(&pdf, &Options { threads: 1, ..opt(2 << 20) }).data, full.data);
    let e100 = |max_stream| optimize(&pdf, &Options { max_stream, threads: 2, ..Options::effort(100) }).data;
    assert_eq!(e100(2 << 20), e100(64 << 20));
    let why = untouched(&optimize(&pdf, &opt(256 << 10)), &pdf);
    assert!(why.contains("object stream past the stream limit"), "{why}");
    let why = untouched(&optimize(&pdf, &opt(1 << 20)), &pdf);
    assert!(why.contains("object streams past their decoding budget"), "{why}");
    let doc = xref::open_with(&pdf, Limits::once(2 << 20)).unwrap();
    for i in 0..8 {
        assert!(matches!(doc.load(1000 + i).unwrap().1, Obj::Dict(_)));
    }
    let s = doc.objstm_stats();
    assert_eq!((s.decoded, s.largest), (8, 512 << 10));
    assert!(s.peak_held < 520 << 10, "{s:?}");
}

#[test]
fn object_streams_held_at_once_are_bounded() {
    // Twelve of 512 KiB never read through (each lists an object nothing refers to), in a file large enough that
    // the budget is not what stops them: 6 MiB would be held. Four times a max_stream of 1 MiB is not enough; of
    // 2 MiB it is.
    let pdf = streams(12, 512 << 10, true, 160 << 10);
    let why = untouched(&optimize(&pdf, &opt(1 << 20)), &pdf);
    assert!(why.contains("object streams held at once past their limit"), "{why}");
    smaller(&optimize(&pdf, &opt(2 << 20)), &pdf);
    // Header entries count too, 16 bytes each: four streams of 3,500 bytes with 100 entries each kept (one
    // entry read, one nothing reads, so each is held) are 20,400 bytes, past four times a max_stream of 4,096.
    let stms: Vec<_> = (0..4u32)
        .map(|i| {
            let objs: Vec<(u32, Vec<u8>)> = std::iter::once((1000 + i, b"<< /K 1 >>".to_vec()))
                .chain((1..99).map(|j| (3000 + 100 * i + j, b"1".to_vec())))
                .chain(std::iter::once((2000 + i, b"<< /U 1 >>".to_vec())))
                .collect();
            let (raw, first) = objstm_raw(&objs, 3500);
            assert_eq!(raw.len(), 3500);
            (100 + i, raw, objs.len(), first)
        })
        .collect();
    let rows: Vec<(u32, u32, u32)> = (0..4).flat_map(|i| [(1000 + i, 100 + i, 0), (2000 + i, 100 + i, 99)]).collect();
    let pdf = build(&catalog(1000..1004, 0), &stms, &rows);
    let why = untouched(&optimize(&pdf, &opt(4096)), &pdf);
    assert!(why.contains("object streams held at once past their limit"), "{why}");
    smaller(&optimize(&pdf, &opt(8192)), &pdf);
}

#[test]
fn object_streams_decode_to_a_bounded_total() {
    // Forty of 200 KiB, each read through: 8 MB in all from a file of about 30 KB. With a max_stream of 256 KiB
    // they may decode to 1 MiB or 64 bytes per byte of the file (about 1.9 MiB here), whichever is more, in all.
    let pdf = streams(40, 200 << 10, false, 0);
    assert!(pdf.len() < 40 << 10, "{}", pdf.len());
    let why = untouched(&optimize(&pdf, &opt(256 << 10)), &pdf);
    assert!(why.contains("object streams past their decoding budget"), "{why}");
    smaller(&optimize(&pdf, &opt(64 << 20)), &pdf);
    // Every filter stage counts: the budget is spent at the same stream as with the last stage alone, or sooner.
    let doc = xref::open_with(&pdf, Limits::once(256 << 10)).unwrap();
    let mut read = 0;
    while read < 40 && doc.load(1000 + read).is_ok() {
        read += 1;
    }
    let s = doc.objstm_stats();
    assert!(read < 40 && s.decoded_bytes <= s.budget, "{read} {s:?}");
    assert!(s.decoded_bytes > read as u64 * (200 << 10), "{s:?}");
}

#[test]
fn object_streams_nested_in_each_other_count_their_stored_bytes() {
    // `k` object streams, each holding the next one whole in its stored bytes, after its own data (which Flate
    // ends): read one after the other they copy about k * k / 2 streams' worth, far more than they decode to.
    let k = 400u32;
    let stms: Vec<(Vec<u8>, usize)> = (0..k)
        .map(|i| {
            let (raw, first) = objstm_raw(&[(1000 + i, b"<< >>".to_vec())], 0);
            (z(&raw), first)
        })
        .collect();
    let head = |i: u32, first: usize, len: usize| {
        format!(
            "{} 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Filter /FlateDecode /Length {len} >>\nstream\n",
            100 + i
        )
    };
    let tail = "\nendstream\nendobj\n";
    let (mut lens, mut inside) = (vec![0; k as usize], 0);
    for i in (0..k).rev() {
        let (data, first) = &stms[i as usize];
        lens[i as usize] = data.len() + inside;
        inside = head(i, *first, lens[i as usize]).len() + lens[i as usize] + tail.len();
    }
    let mut plain = catalog(1000..1000 + k, 0);
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut at: Vec<(u32, u8, u32, u32)> = Vec::new();
    for (n, b) in plain.drain(..) {
        at.push((n, 1, pdf.len() as u32, 0));
        pdf.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        pdf.extend_from_slice(&b);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    for i in 0..k {
        let (data, first) = &stms[i as usize];
        at.push((100 + i, 1, pdf.len() as u32, 0));
        pdf.extend_from_slice(head(i, *first, lens[i as usize]).as_bytes());
        pdf.extend_from_slice(data);
        at.push((1000 + i, 2, 100 + i, 0));
    }
    pdf.extend_from_slice(tail.repeat(k as usize).as_bytes());
    let xn = 1000 + k;
    at.push((xn, 1, pdf.len() as u32, 0));
    let x = pdf.len();
    let mut e = Vec::new();
    for n in 0..=xn {
        let (t, a, b) =
            at.iter().find(|r| r.0 == n).map_or((0, 0, if n == 0 { 65535 } else { 0 }), |r| (r.1, r.2, r.3));
        e.push(t);
        e.extend_from_slice(&a.to_be_bytes());
        e.extend_from_slice(&(b as u16).to_be_bytes());
    }
    let ze = z(&e);
    pdf.extend_from_slice(
        format!(
            "{xn} 0 obj\n<< /Type /XRef /Size {} /W [1 4 2] /Root 1 0 R /Filter /FlateDecode /Length {} >>\nstream\n",
            xn + 1,
            ze.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&ze);
    pdf.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    // Read as they were: every object is there.
    let doc = xref::open(&pdf).unwrap();
    assert_eq!(doc.load(1000 + k - 1).unwrap().1, Obj::Dict(xref_dict(b"<< >>")));
    // Within a budget of 64 bytes per byte of the file (four times a max_stream of 4 KiB is less), the copies
    // are refused long before the end, though what the streams decode to would fit.
    let why = untouched(&optimize(&pdf, &opt(4096)), &pdf);
    assert!(why.contains("object streams past their decoding budget"), "{why}");
    let doc = xref::open_with(&pdf, Limits::once(4096)).unwrap();
    let mut read = 0;
    while read < k && doc.load(1000 + read).is_ok() {
        read += 1;
    }
    let s = doc.objstm_stats();
    assert!(read < k && s.decoded_bytes < s.budget / 8 && s.stored_bytes > s.budget / 2, "{read} {s:?}");
}

#[test]
fn an_object_stream_is_decoded_once_and_its_integers_are_remembered() {
    let pdf = streams(2, 64 << 10, false, 0);
    let doc = xref::open_with(&pdf, Limits::once(1 << 20)).unwrap();
    doc.load(1000).unwrap();
    assert_eq!(doc.load(1000), Err(xref::LoadError::BadObject(1000, "object stream already read through")));
    assert_eq!(doc.objstm_stats().decoded, 1);
    // Kept, as before, for callers that read objects in any order.
    let doc = xref::open(&pdf).unwrap();
    assert_eq!(doc.load(1000).unwrap(), doc.load(1000).unwrap());
    // An integer is remembered: three streams with one /Length, the only object of an object stream.
    let (raw, first) = objstm_raw(&[(9, b"3".to_vec())], 1 << 20);
    let mut plain = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, PAGES.to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [10 0 R 11 0 R 12 0 R] >>".to_vec()),
    ];
    for i in 0..3u32 {
        plain.push((10 + i, format!("<< /Length 9 0 R /X {i} >>\nstream\nq Q\nendstream").into_bytes()));
    }
    let pdf = build(&plain, &[(8, raw, 1, first)], &[(9, 8, 0)]);
    let doc = xref::open_with(&pdf, Limits::once(4 << 20)).unwrap();
    for _ in 0..3 {
        assert_eq!(doc.load(9).unwrap(), (0, Obj::Int(3)));
    }
    let s = doc.objstm_stats();
    assert_eq!((s.decoded, s.memo_hits), (1, 2));
    smaller(&optimize(&pdf, &opt(4 << 20)), &pdf);
    // Once everything is read, object streams go: a stream's objects are refused from then on, integers aside.
    doc.drop_cache();
    assert_eq!(doc.load(9).unwrap(), (0, Obj::Int(3)));
    let pdf = streams(2, 64 << 10, true, 0);
    let doc = xref::open_with(&pdf, Limits::once(1 << 20)).unwrap();
    doc.load(1000).unwrap();
    assert!(doc.objstm_stats().held > 64 << 10, "not read through, so held");
    doc.drop_cache();
    assert_eq!(doc.objstm_stats().held, 0);
    assert_eq!(doc.load(2000), Err(xref::LoadError::BadObject(2000, "object stream already read through")));
    // Kept, as before: dropped, a stream is decoded again.
    let doc = xref::open(&pdf).unwrap();
    doc.load(1000).unwrap();
    doc.drop_cache();
    doc.load(2000).unwrap();
    assert_eq!(doc.objstm_stats().decoded, 2);
}

#[test]
fn each_object_of_an_object_stream_is_read_within_its_own_bytes() {
    // A hundred entries at one offset before 1 MiB of spaces (each read went through all of it): the last one
    // listed may be read, as readers that cut each object at the next entry's offset give the others no bytes.
    let objs: Vec<(u32, Vec<u8>)> = (0..100).map(|i| (1000 + i, b"1".to_vec())).collect();
    let mut head = String::new();
    for (n, _) in &objs {
        head.push_str(&format!("{n} 0 "));
    }
    let first = head.len();
    let mut raw = head.into_bytes();
    raw.extend_from_slice(b"1");
    raw.resize(raw.len() + (1 << 20), b' ');
    let rows: Vec<(u32, u32, u32)> = (0..100).map(|i| (1000 + i, 5, i)).collect();
    let pdf = build(&catalog(1000..1100, 0), &[(5, raw, 100, first)], &rows);
    let why = untouched(&optimize(&pdf, &opt(64 << 20)), &pdf);
    assert!(why.contains("object stream lists two objects at one offset"), "{why}");
    let doc = xref::open(&pdf).unwrap();
    assert_eq!(doc.load(1099).unwrap().1, Obj::Int(1));
    assert_eq!(doc.load(1099).unwrap().1, Obj::Int(1));
    assert_eq!(doc.load(1098), Err(xref::LoadError::BadObject(1098, "object stream lists two objects at one offset")));
    // Entries at one offset with others listed between them: still only the last listed is read, so a header
    // that takes turns between two offsets can't have the same bytes parsed once per entry.
    let raw = b"1000 0 1001 6 1002 0 1003 6 [1 2] null".to_vec();
    let first = raw.iter().position(|&c| c == b'[').unwrap();
    let rows: Vec<(u32, u32, u32)> = (0..4).map(|i| (1000 + i, 5, i)).collect();
    let pdf = build(&catalog(1000..1004, 0), &[(5, raw, 4, first)], &rows);
    let why = untouched(&optimize(&pdf, &opt(64 << 20)), &pdf);
    assert!(why.contains("object stream lists two objects at one offset"), "{why}");
    let doc = xref::open(&pdf).unwrap();
    assert_eq!(doc.load(1002).unwrap().1, Obj::Array(vec![Obj::Int(1), Obj::Int(2)]));
    assert_eq!(doc.load(1003).unwrap().1, Obj::Null);
    assert_eq!(doc.objstm_stats().shared, 2);
    for n in [1000, 1001] {
        assert_eq!(doc.load(n), Err(xref::LoadError::BadObject(n, "object stream lists two objects at one offset")));
    }
    // Two entries at one offset, the second read (whatever the first lists, and whether or not anything reads
    // it): every reader reads it. The first read: refused.
    for (raw, row, read, n) in [
        (&b"1000 0 1001 0 << /A 1 >>"[..], (1000, 5, 0), 1001, 1),
        (b"1000 0 1000 0 << /A 1 >>", (1000, 5, 1), 1000, 1),
        (b"1000 0 1001 0 << /A 1 >>", (1001, 5, 1), 1000, 0),
    ] {
        let first = raw.iter().position(|&c| c == b'<').unwrap();
        let rows = [row, if read == 1001 { (1001, 5, 1) } else { (1000, 5, n) }];
        let pdf = build(&catalog(read..read + 1, 0), &[(5, raw.to_vec(), 2, first)], &rows);
        let r = optimize(&pdf, &opt(64 << 20));
        if n == 1 {
            smaller(&r, &pdf);
            let doc = xref::open_with(&pdf, Limits::once(1 << 20)).unwrap();
            assert_eq!(doc.load(read).unwrap().1, Obj::Dict(xref_dict(b"<< /A 1 >>")));
            assert_eq!(doc.objstm_stats().shared, 1);
        } else {
            assert!(untouched(&r, &pdf).contains("object stream lists two objects at one offset"));
        }
    }
    // An object that runs into the next one's bytes is not read past them.
    let raw = b"1000 0 1001 3 << /A 1 >>\n<< /B 2 >>".to_vec();
    let pdf = build(&catalog(1000..1002, 0), &[(5, raw, 2, 14)], &[(1000, 5, 0), (1001, 5, 1)]);
    assert!(xref::open(&pdf).unwrap().load(1000).is_err());
    untouched(&optimize(&pdf, &opt(64 << 20)), &pdf);
    // Offsets out of order are read as they are: each object ends where the next one in the data starts.
    let raw = b"1001 11 1000 0 << /A 1 >>\n<< /B 2 >>".to_vec();
    let pdf = build(&catalog(1000..1002, 0), &[(5, raw, 2, 15)], &[(1000, 5, 1), (1001, 5, 0)]);
    let doc = xref::open_with(&pdf, Limits::once(1 << 20)).unwrap();
    assert_eq!(doc.load(1000).unwrap().1, Obj::Dict(xref_dict(b"<< /A 1 >>")));
    assert_eq!(doc.load(1001).unwrap().1, Obj::Dict(xref_dict(b"<< /B 2 >>")));
    assert_eq!(doc.objstm_stats().unordered, 1);
    smaller(&optimize(&pdf, &opt(64 << 20)), &pdf);
    // Objects that touch, which readers that cut at the next offset read differently from readers that read on
    // (as each object reads on its own: "1234", "truefalse", "/" and "true", "5" and "0 R", "5" and a comment
    // hiding "0 R"): left as they are. The next entry ends the window whether or not anything points at it.
    for (body, second, both) in [
        (&b"1234"[..], 2, true),
        (b"truefalse", 4, true),
        (b"/true", 1, true),
        (b"5 0 R", 2, true),
        (b"5 %x\n0 R", 3, false),
        (b"42", 1, false),
    ] {
        let head = format!("1000 0 1001 {second} ");
        let mut raw = head.clone().into_bytes();
        raw.extend_from_slice(body);
        let rows: &[(u32, u32, u32)] = if both { &[(1000, 5, 0), (1001, 5, 1)] } else { &[(1000, 5, 0)] };
        let pdf = build(&catalog(1000..1000 + 1 + u32::from(both), 0), &[(5, raw, 2, head.len())], rows);
        let why = untouched(&optimize(&pdf, &opt(64 << 20)), &pdf);
        assert!(
            why.contains("object runs into the next one in its object stream"),
            "{}: {why}",
            String::from_utf8_lossy(body)
        );
    }
    // An offset past 32 bits in an entry nothing reads is no harm, on any platform.
    let raw = b"1000 0 1001 4294967296 << /A 1 >>".to_vec();
    let pdf = build(&catalog(1000..1001, 0), &[(5, raw, 2, 23)], &[(1000, 5, 0), (1001, 5, 1)]);
    let doc = xref::open_with(&pdf, Limits::once(1 << 20)).unwrap();
    assert_eq!(doc.load(1000).unwrap().1, Obj::Dict(xref_dict(b"<< /A 1 >>")));
    assert_eq!(doc.load(1001), Err(xref::LoadError::BadObject(1001, "offset past the object stream")));
    // A /Length past 32 bits is past the end of the file on any platform.
    let pdf = build(&catalog(1000..1001, 0), &[(5, b"1000 0 << /A 1 >>".to_vec(), 1, 7)], &[(1000, 5, 0)]);
    let at = pdf.windows(21).position(|w| w.starts_with(b"/Length ") && w.ends_with(b" >>")).unwrap();
    let len: u64 = String::from_utf8_lossy(&pdf[at + 8..at + 18]).trim().parse().unwrap();
    let mut wide = pdf.clone();
    wide[at + 8..at + 18].copy_from_slice(format!("{:>10}", len + (1 << 32)).as_bytes());
    let why = untouched(&optimize(&wide, &opt(64 << 20)), &wide);
    assert!(why.contains("stream runs past the end"), "{why}");
}

fn xref_dict(d: &[u8]) -> gpress::pdf::object::Dict {
    match gpress::pdf::parse::Lexer::new(d, 0).parse_obj().unwrap() {
        Obj::Dict(d) => d,
        o => panic!("{o:?}"),
    }
}

#[test]
fn our_own_object_streams_are_read_back_whatever_max_stream() {
    // No object stream in the input; the output packs its 203 small objects into two. The self-check reads
    // them back however small max_stream is, so a max_stream of 64 bytes still gives a smaller file; that
    // file, as an input, has object streams past the limit and is left as it is.
    let mut plain = catalog(10..210, 0);
    for n in 10..210u32 {
        plain.push((n, format!("<< /Type /Annot /Subtype /Link /Rect [0 0 {n} {n}] /Border [0 0 0] >>").into_bytes()));
    }
    let pdf = build(&plain, &[], &[]);
    let r = optimize(&pdf, &opt(64));
    smaller(&r, &pdf);
    assert!(matches!(&r.outcome, Outcome::Smaller(s) if s.objstms == 2), "{:?}", r.outcome);
    let why = untouched(&optimize(&r.data, &opt(64)), &r.data);
    assert!(why.contains("object stream past the stream limit"), "{why}");
    assert_eq!(untouched(&optimize(&r.data, &opt(1 << 20)), &r.data), "no saving possible");
}

#[test]
fn encrypted_files_with_object_streams_read_them_again_once_the_key_is_known() {
    let file = |name: &str| std::fs::read(format!("{}/tests/data/enc/{name}.pdf", env!("CARGO_MANIFEST_DIR"))).unwrap();
    // Metadata left unencrypted: the catalog is read while the file is opened, before its key is complete, and
    // read again after. Rewritten with one object per object stream, the catalog sits alone in one, which the
    // first read lets go: it must be decoded again, and what it decoded to the first time stays counted.
    let input = file("r4_aes_cleartext_md_plain");
    let one = optimize(&input, &Options { objstm_size: 1, ..opt(64 << 20) });
    smaller(&one, &input);
    let alone = one.data;
    let doc = xref::open_with(&alone, Limits::once(64 << 20)).unwrap();
    assert!(matches!(doc.crypt, Some(Ok(_))));
    let sn = match doc.trailer.get(b"Root") {
        Some(&Obj::Ref(n, _)) => match doc.entries.get(&n) {
            Some(&xref::Entry::InStream(sn, _)) => sn,
            e => panic!("{e:?}"),
        },
        r => panic!("{r:?}"),
    };
    assert_eq!(doc.entries.values().filter(|e| matches!(e, xref::Entry::InStream(s, _) if *s == sn)).count(), 1);
    let s = doc.objstm_stats();
    assert_eq!((s.decoded, s.held), (1, 0), "{s:?}");
    assert_eq!(s.budget_left, s.budget - s.decoded_bytes - s.stored_bytes, "{s:?}");
    let r = optimize(&alone, &opt(64 << 20));
    smaller(&r, &alone);
    assert!(matches!(&r.outcome, Outcome::Smaller(s) if s.encryption == "R4/AESV2"), "{:?}", r.outcome);
    assert_eq!(
        untouched(&optimize(&alone, &Options { objstm_size: 1, ..opt(64 << 20) }), &alone),
        "no saving possible"
    );
    // With object streams of several objects, the one read at open is held, and let go by the reset.
    let shared = optimize(&file("r4_aes_cleartext_md"), &opt(64 << 20));
    let s = xref::open_with(&shared.data, Limits::once(64 << 20)).unwrap().objstm_stats();
    assert!(s.peak_held > 0 && s.held == 0, "{s:?}");
    // The archive opens the file only within max_stream_archive: below the catalog's object stream, its streams
    // are stored encrypted, as they are; either way the bytes come back exactly.
    let catalog = xref::open_with(&alone, Limits::once(64 << 20)).unwrap().objstm_stats().largest;
    for (max_stream, decrypted) in [(catalog - 1, false), (catalog, true)] {
        let (a, stats) = pack_with(&alone, &PackOptions { max_stream, ..PackOptions::default() });
        assert_eq!(stats.decrypted > 0, decrypted, "{max_stream}");
        assert_eq!(unpack(&a).unwrap(), alone);
    }
    let r6 = file("r6_objstm");
    let (a, stats) = pack_with(&r6, &PackOptions::default());
    assert!(stats.decrypted > 0);
    assert_eq!(unpack(&a).unwrap(), r6);
}
