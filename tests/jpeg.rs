//! JPEG Huffman optimisation: the same coefficients in fewer bytes, or the file left as it is.
//!
//! The test files (tests/data/jpeg, made by make.py with libjpeg-turbo's cjpeg) come with `jpegtran -optimize`
//! versions: libjpeg decoded the original to coefficients and wrote them again, so if our reading of the
//! original gives the same symbols as our reading of libjpeg's file, we read JPEG as libjpeg does.

use engine::jpeg::{equivalent, optimize, same_coded_data, Refused};
use engine::pdf::filters::{chain, decode_stage, is_general};
use engine::pdf::object::Obj;
use engine::pdf::rewrite::{self, Outcome};
use std::collections::HashMap;

fn data(name: &str) -> Vec<u8> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/jpeg").join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

const GOOD: [&str; 14] = [
    "base420", "base444", "base422", "base440", "gray", "restart1", "restart5", "optimized", "sof1", "tiny", "edge17x9",
    "multiscan", "multiscan2", "cmyk",
];

fn find(h: &[u8], n: &[u8]) -> usize {
    h.windows(n.len()).position(|w| w == n).expect("pattern")
}

// ------------------------------------------------------------------ a separate plain decoder, for checking

/// Every block's 64 coefficients (zigzag order, DC prediction undone), scan by scan, read bit by bit. Written
/// separately from the engine's reader so the two check each other. None for anything it doesn't handle.
fn coefficients(d: &[u8]) -> Option<Vec<Vec<[i32; 64]>>> {
    struct Table {
        counts: [u8; 16],
        values: Vec<u8>,
    }
    if d.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut tables: HashMap<u8, Table> = HashMap::new();
    // A frame: width, height and the components (id, horizontal and vertical sampling).
    type Frame = (usize, usize, Vec<(u8, usize, usize)>);
    let (mut restart, mut frame): (usize, Option<Frame>) = (0, None);
    let mut out = Vec::new();
    let mut pos = 2;
    loop {
        if *d.get(pos)? != 0xFF {
            return None;
        }
        while *d.get(pos)? == 0xFF {
            pos += 1;
        }
        let m = d[pos];
        pos += 1;
        if m == 0xD9 {
            return Some(out);
        }
        let len = u16::from_be_bytes([*d.get(pos)?, *d.get(pos + 1)?]) as usize;
        let body = d.get(pos + 2..pos + len)?;
        pos += len;
        match m {
            0xC0 | 0xC1 => {
                let n = body[5] as usize;
                let comps = (0..n).map(|i| (body[6 + 3 * i], (body[7 + 3 * i] >> 4) as usize, (body[7 + 3 * i] & 15) as usize)).collect();
                frame = Some((u16::from_be_bytes([body[3], body[4]]) as usize, u16::from_be_bytes([body[1], body[2]]) as usize, comps));
            }
            0xC4 => {
                let mut b = body;
                while !b.is_empty() {
                    let mut counts = [0u8; 16];
                    counts.copy_from_slice(b.get(1..17)?);
                    let n: usize = counts.iter().map(|&c| c as usize).sum();
                    tables.insert(b[0], Table { counts, values: b.get(17..17 + n)?.to_vec() });
                    b = &b[17 + n..];
                }
            }
            0xDD => restart = u16::from_be_bytes([body[0], body[1]]) as usize,
            0xDA => {
                let (w, h, comps) = frame.as_ref()?;
                let (hmax, vmax) = (comps.iter().map(|c| c.1).max()?, comps.iter().map(|c| c.2).max()?);
                let ns = body[0] as usize;
                // Blocks of one MCU: (component, DC table, AC table).
                let mut blocks = Vec::new();
                let mcus = if ns == 1 {
                    let c = comps.iter().position(|c| c.0 == body[1])?;
                    blocks.push((c, body[2] >> 4, 0x10 | body[2] & 15));
                    (w * comps[c].1).div_ceil(8 * hmax) * (h * comps[c].2).div_ceil(8 * vmax)
                } else {
                    for i in 0..ns {
                        let c = comps.iter().position(|c| c.0 == body[1 + 2 * i])?;
                        for _ in 0..comps[c].1 * comps[c].2 {
                            blocks.push((c, body[2 + 2 * i] >> 4, 0x10 | body[2 + 2 * i] & 15));
                        }
                    }
                    w.div_ceil(8 * hmax) * h.div_ceil(8 * vmax)
                };
                // The coded bits, with stuffed bytes and restart markers taken out.
                let mut bits: Vec<Vec<u8>> = vec![Vec::new()];
                loop {
                    let b = *d.get(pos)?;
                    if b == 0xFF {
                        let c = *d.get(pos + 1)?;
                        if c == 0 {
                            bits.last_mut()?.push(0xFF);
                            pos += 2;
                            continue;
                        }
                        if (0xD0..=0xD7).contains(&c) {
                            bits.push(Vec::new());
                            pos += 2;
                            continue;
                        }
                        break;
                    }
                    bits.last_mut()?.push(b);
                    pos += 1;
                }
                let mut scan = Vec::new();
                let mut interval = 0;
                let mut bitpos = 0usize;
                let mut pred = vec![0i32; comps.len()];
                let read = |bits: &Vec<Vec<u8>>, iv: usize, bp: &mut usize, n: usize| -> Option<i32> {
                    let mut v = 0i32;
                    for _ in 0..n {
                        let byte = *bits.get(iv)?.get(*bp / 8)?;
                        v = v << 1 | ((byte >> (7 - *bp % 8)) & 1) as i32;
                        *bp += 1;
                    }
                    Some(v)
                };
                let decode = |t: &Table, bits: &Vec<Vec<u8>>, iv: usize, bp: &mut usize| -> Option<u8> {
                    let (mut code, mut first, mut k) = (0i32, 0i32, 0usize);
                    for len in 0..16 {
                        code = code << 1 | read(bits, iv, bp, 1)?;
                        let c = t.counts[len] as i32;
                        if code - first < c {
                            return t.values.get(k + (code - first) as usize).copied();
                        }
                        k += c as usize;
                        first = (first + c) << 1;
                    }
                    None
                };
                for mcu in 0..mcus {
                    if restart > 0 && mcu > 0 && mcu % restart == 0 {
                        interval += 1;
                        bitpos = 0;
                        pred.iter_mut().for_each(|p| *p = 0);
                    }
                    for &(c, dc, ac) in &blocks {
                        let mut coef = [0i32; 64];
                        let s = decode(tables.get(&dc)?, &bits, interval, &mut bitpos)? as usize;
                        let mut v = read(&bits, interval, &mut bitpos, s)?;
                        if s > 0 && v < 1 << (s - 1) {
                            v -= (1 << s) - 1;
                        }
                        pred[c] += v;
                        coef[0] = pred[c];
                        let mut k = 1;
                        while k < 64 {
                            let rs = decode(tables.get(&ac)?, &bits, interval, &mut bitpos)?;
                            let (r, s) = ((rs >> 4) as usize, (rs & 15) as usize);
                            if s == 0 {
                                if r == 15 {
                                    k += 16;
                                    continue;
                                }
                                break;
                            }
                            k += r;
                            let mut v = read(&bits, interval, &mut bitpos, s)?;
                            if v < 1 << (s - 1) {
                                v -= (1 << s) - 1;
                            }
                            *coef.get_mut(k)? = v;
                            k += 1;
                        }
                        scan.push(coef);
                    }
                }
                out.push(scan);
            }
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ JPEG files

#[test]
fn smaller_with_the_symbols_libjpeg_reads() {
    for name in GOOD {
        let a = data(&format!("{name}.jpg"));
        let reference = data(&format!("{name}.ref.jpg"));
        let o = optimize(&a).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(o.len() < a.len(), "{name}");
        assert!(equivalent(&a, &o), "{name}");
        assert!(same_coded_data(&o, &reference), "{name}: symbols differ from libjpeg's re-encoding");
        let c = coefficients(&a).unwrap_or_else(|| panic!("{name}: plain decoder"));
        assert_eq!(coefficients(&o).as_ref(), Some(&c), "{name}: coefficients");
        assert_eq!(coefficients(&reference).as_ref(), Some(&c), "{name}: coefficients of libjpeg's file");
        // Optimal code lengths: never more coded bits than libjpeg's tables (its file has a table segment per
        // table, 4 bytes each, where ours has one per scan; 0xFF stuffing can differ by a few bytes).
        assert!(o.len() <= reference.len() + 8, "{name}: ours {} jpegtran {}", o.len(), reference.len());
        // Optimising again gives nothing more than the table order may.
        if let Ok(o2) = optimize(&o) {
            assert!(o2.len() < o.len() && equivalent(&o, &o2), "{name}");
        }
    }
}

#[test]
fn every_other_byte_is_kept() {
    // A comment between the tables and the scan, bytes after the end marker.
    let a = data("base444.jpg");
    let mut b = Vec::new();
    let sos = find(&a, &[0xFF, 0xDA]);
    b.extend_from_slice(&a[..sos]);
    b.extend_from_slice(&[0xFF, 0xFE, 0, 6, b'h', b'e', b'l', b'o']);
    b.extend_from_slice(&a[sos..]);
    b.extend_from_slice(b"\r\nafter the end");
    let o = optimize(&b).unwrap();
    assert!(equivalent(&b, &o));
    assert!(o.ends_with(b"\xFF\xD9\r\nafter the end"));
    assert_eq!(&o[..4], &[0xFF, 0xD8, 0xFF, 0xE0]);
    assert!(find(&o, b"\xFF\xFE\x00\x06helo") < find(&o, &[0xFF, 0xDA]));
    // ICC profile, JFIF and comment segments are byte for byte the same.
    for seg in [&a[2..20], &a[find(&a, b"ICC_PROFILE") - 4..find(&a, b"ICC_PROFILE") + 780]] {
        assert!(o.windows(seg.len()).any(|w| w == seg));
    }
    // The CMYK file keeps its Adobe marker.
    let c = data("cmyk.jpg");
    let oc = optimize(&c).unwrap();
    assert_eq!(&oc[..18], &c[..18]);
}

#[test]
fn a_restart_marker_after_the_last_interval_is_kept() {
    // restart5: 20 MCUs in intervals of 5, so restart markers 0, 1, 2 between them; some encoders add a 3.
    let a = data("restart5.jpg");
    let end = a.len() - 2;
    let with = [&a[..end], &[0xFF, 0xD3], &a[end..]].concat();
    let o = optimize(&with).unwrap();
    assert!(equivalent(&with, &o));
    assert!(o.ends_with(&[0xFF, 0xD3, 0xFF, 0xD9]));
    assert_eq!(coefficients(&o), coefficients(&a));
    // Out of turn, or two of them: left alone.
    let wrong = [&a[..end], &[0xFF, 0xD5], &a[end..]].concat();
    assert_eq!(optimize(&wrong), Err(Refused("marker inside the scan's data")));
    let two = [&a[..end], &[0xFF, 0xD3, 0xFF, 0xD4], &a[end..]].concat();
    assert!(optimize(&two).is_err());
    // Without a restart interval, none is allowed.
    let b = data("base420.jpg");
    let end = b.len() - 2;
    assert!(optimize(&[&b[..end], &[0xFF, 0xD0], &b[end..]].concat()).is_err());
}

#[test]
fn other_kinds_are_left_alone() {
    assert_eq!(optimize(&data("progressive.jpg")), Err(Refused("progressive")));
    assert_eq!(optimize(&data("arithmetic.jpg")), Err(Refused("arithmetic coding")));
    let a = data("base420.jpg");
    let sof = find(&a, &[0xFF, 0xC0]);
    let patched = |at: usize, v: u8| {
        let mut b = a.clone();
        b[at] = v;
        b
    };
    assert_eq!(optimize(&patched(sof + 4, 12)), Err(Refused("not 8-bit samples")));
    assert_eq!(optimize(&patched(sof + 1, 0xC3)), Err(Refused("lossless")));
    assert_eq!(optimize(&patched(sof + 1, 0xC5)), Err(Refused("hierarchical")));
    assert_eq!(optimize(&patched(sof + 1, 0xC9)), Err(Refused("arithmetic coding")));
    assert_eq!(optimize(&patched(sof + 6, 0)), Err(Refused("number of lines given by a DNL marker")));
    // Luma sampled 2x2, chroma 1x1 here; chroma 3x1 makes 2:3, which libjpeg can't scale ("fractional
    // sampling not implemented yet").
    assert_eq!(optimize(&patched(sof + 14, 0x31)), Err(Refused("fractional sampling")));
    // A scan that is not the whole of the coefficients.
    let sos = find(&a, &[0xFF, 0xDA]);
    let len = u16::from_be_bytes([a[sos + 2], a[sos + 3]]) as usize;
    assert_eq!(optimize(&patched(sos + len, 62)), Err(Refused("not a sequential scan")));
    // No Huffman tables: some readers use default tables, others fail; not ours to change.
    let mut b = a.clone();
    while let Some(p) = b.windows(2).position(|w| w == [0xFF, 0xC4]) {
        let l = u16::from_be_bytes([b[p + 2], b[p + 3]]) as usize;
        b.drain(p..p + 2 + l);
    }
    assert_eq!(optimize(&b), Err(Refused("scan uses an undefined Huffman table")));
    // Multi-picture data: offsets to the images after this one would move.
    let mpf = [&a[..2], &[0xFF, 0xE2, 0, 8, b'M', b'P', b'F', 0, 0, 0], &a[2..]].concat();
    assert_eq!(optimize(&mpf), Err(Refused("multi-picture data")));
    // Not a JPEG at all.
    assert!(optimize(b"").is_err());
    assert!(optimize(b"\xFF\xD8").is_err());
    assert!(optimize(&a[1..]).is_err());
    // Extra coded data after the last block, or a missing end marker.
    let end = a.len() - 2;
    assert_eq!(optimize(&[&a[..end], &[0x12, 0x34], &a[end..]].concat()), Err(Refused("extra entropy-coded data")));
    assert!(optimize(&a[..end]).is_err());
}

/// A JPEG's segments in order: (marker, its bytes; a scan's include its coded data). For the test files only.
fn segments(d: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 2;
    loop {
        let s = i;
        assert_eq!(d[i], 0xFF);
        let m = d[i + 1];
        i += 2;
        if m == 0xD9 {
            out.push((m, d[s..].to_vec()));
            return out;
        }
        i += u16::from_be_bytes([d[i], d[i + 1]]) as usize;
        if m == 0xDA {
            while !(d[i] == 0xFF && d[i + 1] != 0 && !(0xD0..=0xD7).contains(&d[i + 1])) {
                i += 1;
            }
        }
        out.push((m, d[s..i].to_vec()));
    }
}

/// The segments of `d` with marker `m`, joined.
fn pick(d: &[u8], m: u8) -> Vec<u8> {
    segments(d).into_iter().filter(|s| s.0 == m).flat_map(|s| s.1).collect()
}

fn segment(m: u8, body: &[u8]) -> Vec<u8> {
    [&[0xFF, m][..], &(body.len() as u16 + 2).to_be_bytes(), body].concat()
}

#[test]
fn fill_bytes_leave_the_file_as_it_is() {
    // A valid layout (T.81 allows fill bytes before any marker) that pdf.js decodes as it is but not once the
    // tables move: a comment holding a small JPEG (so an FF D8 pair), the tables, fill bytes, the rest. With the
    // tables gone from between them, pdf.js would find 0xFFFF after the comment, take its length for wrong and
    // stop at the FF D8 inside it.
    let a = data("base420.jpg");
    let com = segment(0xFE, &[b"thumbnail:".as_slice(), &data("tiny.jpg")].concat());
    let layout = |fill: &[u8]| {
        [&a[..2], &pick(&a, 0xE0), &com, &pick(&a, 0xC4), fill, &pick(&a, 0xDB), &pick(&a, 0xC0), &pick(&a, 0xDA), &pick(&a, 0xD9)].concat()
    };
    assert_eq!(optimize(&layout(&[0xFF, 0xFF])), Err(Refused("fill bytes before a marker")));
    // Without the fill bytes the same layout is optimised, and every segment of the new file is followed by
    // a marker code readers expect (0xFFC0 to 0xFFFE).
    let plain = layout(&[]);
    let o = optimize(&plain).unwrap();
    assert!(equivalent(&plain, &o));
    let segs = segments(&o);
    for w in segs.windows(2) {
        assert!((0xC0..=0xFE).contains(&w[1].0), "marker {:02X} after {:02X}", w[1].0, w[0].0);
    }
    // Fill bytes anywhere else: after the start marker, before the scan, before the end marker.
    let sos = find(&a, &[0xFF, 0xDA]);
    let eoi = a.len() - 2;
    for at in [2, sos, eoi] {
        let b = [&a[..at], &[0xFF][..], &a[at..]].concat();
        assert_eq!(optimize(&b), Err(Refused("fill bytes before a marker")), "fill at {at}");
    }
}

#[test]
fn quantisation_tables_are_checked() {
    let a = data("base420.jpg");
    let dqt = find(&a, &[0xFF, 0xDB]);
    let len = u16::from_be_bytes([a[dqt + 2], a[dqt + 3]]) as usize;
    let end = dqt + 2 + len;
    let with_len = |b: &mut Vec<u8>, l: usize| b[dqt + 2..dqt + 4].copy_from_slice(&(l as u16).to_be_bytes());
    // The length 2 too long, with two zero bytes inside the segment: libjpeg refuses this file, and pdf.js reads
    // the tables, not the length, so it would read the bytes after it as the next marker.
    let mut b = [&a[..end], &[0, 0][..], &a[end..]].concat();
    with_len(&mut b, len + 2);
    assert_eq!(optimize(&b), Err(Refused("bad quantisation table segment")));
    // The length one short: the last value is left over between segments.
    let mut b = a.clone();
    with_len(&mut b, len - 1);
    assert!(optimize(&b).is_err());
    // Precision 2, table number 4.
    for pt in [0x20, 0x04, 0x14] {
        let mut b = a.clone();
        b[dqt + 4] = pt;
        assert_eq!(optimize(&b), Err(Refused("bad quantisation table precision or number")), "{pt:02X}");
    }
    // An empty segment.
    let b = [&a[..dqt], &[0xFF, 0xDB, 0, 2][..], &a[dqt..]].concat();
    assert_eq!(optimize(&b), Err(Refused("empty quantisation table segment")));
    // A table no segment defines.
    let g = data("gray.jpg");
    let sof = find(&g, &[0xFF, 0xC0]);
    let mut b = g.clone();
    b[sof + 12] = 1; // the one component's table number
    assert_eq!(optimize(&b), Err(Refused("scan uses an undefined quantisation table")));
    // Defined again after a scan used it (libjpeg keeps the first table, pdf.js the last): left as it is. Table
    // 1 defined again between the scans, before the scan that first uses it, is fine (readers agree on it).
    let m = data("multiscan.jpg");
    let segs = segments(&m);
    let second_scan = segs.iter().enumerate().filter(|(_, s)| s.0 == 0xDA).nth(1).unwrap().0;
    let table = |n: u8| segment(0xDB, &[&[n][..], &[1u8; 64]].concat());
    let insert = |extra: &[u8]| {
        let mut b = Vec::from(&m[..2]);
        for (k, s) in segs.iter().enumerate() {
            if k == second_scan {
                b.extend_from_slice(extra);
            }
            b.extend_from_slice(&s.1);
        }
        b
    };
    assert_eq!(optimize(&insert(&table(0))), Err(Refused("quantisation table defined again after a scan used it")));
    let fine = insert(&table(1));
    let o = optimize(&fine).unwrap();
    assert!(equivalent(&fine, &o));
}

#[test]
fn data_bound_to_the_bytes_leaves_the_file_as_it_is() {
    // An APP11 segment with a JUMBF box labelled c2pa (content credentials hash the file's bytes).
    let a = data("base420.jpg");
    let mut jumbf = b"JP\x00\x01\x00\x00\x00\x01".to_vec();
    jumbf.extend_from_slice(&[0, 0, 0, 0x21]);
    jumbf.extend_from_slice(b"jumb\x00\x00\x00\x19jumdc2pa\x00\x11\x00\x10\x80\x00\x00\xaa\x00\x38\x9b\x71\x03c2pa\x00");
    let b = [&a[..2], &segment(0xEB, &jumbf), &a[2..]].concat();
    assert_eq!(optimize(&b), Err(Refused("APP11 data (JUMBF, such as content credentials)")));
    // Any APP11.
    let b = [&a[..2], &segment(0xEB, b"other"), &a[2..]].concat();
    assert!(optimize(&b).is_err());
}

#[test]
fn more_blocks_than_the_data_can_hold_are_refused_at_once() {
    // Every block takes at least 2 bits, so a frame of 65535 x 65535 pixels (67 million blocks) can't fit in 2 KB
    // of coded data: refused before any decoding, not after reading to the end of the data.
    let a = data("gray.jpg");
    let sof = find(&a, &[0xFF, 0xC0]);
    let mut b = a.clone();
    b[sof + 5..sof + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    let t = std::time::Instant::now();
    assert_eq!(optimize(&b), Err(Refused("more blocks than the data can hold")));
    assert!(t.elapsed().as_millis() < 100);
    // At exactly the bound it is read: 1-bit codes for the one DC difference and the end of block, 4 blocks of
    // 2 bits in one byte of coded data (the new file saves 4 bytes with one table segment instead of two).
    let dht = |class: u8| segment(0xC4, &[&[class][..], &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[0]].concat());
    let frame = segment(0xC0, &[8, 0, 16, 0, 16, 1, 1, 0x11, 0]);
    let scan = segment(0xDA, &[1, 1, 0x00, 0, 63, 0]);
    let jpeg = [&[0xFF, 0xD8][..], &segment(0xDB, &[&[0u8][..], &[1u8; 64]].concat()), &frame, &dht(0x00), &dht(0x10), &scan, &[0x00], &[0xFF, 0xD9]].concat();
    assert_eq!(coefficients(&jpeg).map(|s| s[0].len()), Some(4));
    let o = optimize(&jpeg).unwrap();
    assert!(o.len() < jpeg.len() && equivalent(&jpeg, &o));
    assert_eq!(coefficients(&o), coefficients(&jpeg));
}

#[test]
fn many_segments_are_refused() {
    // Millions of empty comments would cost memory per segment: past 4096 segments, a file is left as it is.
    let a = data("base420.jpg");
    let n = segments(&a).len() + 1; // and the start marker
    let comments = |k: usize| [&a[..2], &[0xFF, 0xFE, 0, 2].repeat(k), &a[2..]].concat();
    let most = comments(4096 - n);
    let o = optimize(&most).unwrap();
    assert!(equivalent(&most, &o));
    assert_eq!(optimize(&comments(4096 - n + 1)), Err(Refused("too many segments")));
    assert_eq!(optimize(&comments(1 << 20)), Err(Refused("too many segments")));
}

/// Damaged input: an error, or a smaller file with exactly the same coefficients.
fn check_damaged(x: &[u8]) {
    if let Ok(o) = optimize(x) {
        assert!(o.len() < x.len());
        assert!(equivalent(x, &o));
        let c = coefficients(x).expect("our reader took it; the plain one must too");
        assert_eq!(coefficients(&o), Some(c));
    }
}

#[test]
fn damaged_files_give_an_error_or_the_same_coefficients() {
    for name in ["base420", "restart5", "multiscan", "cmyk"] {
        let a = data(&format!("{name}.jpg"));
        // Every position in the headers and the first coded bytes, then every 7th.
        let positions = (0..a.len()).filter(|&i| i < 800 || i % 7 == 0);
        for i in positions {
            check_damaged(&a[..i]);
            for bit in [0, 7] {
                let mut b = a.clone();
                b[i] ^= 1 << bit;
                check_damaged(&b);
            }
        }
        // Random multi-byte damage.
        let mut s = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..500 {
            let mut b = a.clone();
            for _ in 0..1 + s % 4 {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let i = (s % b.len() as u64) as usize;
                b[i] = (s >> 32) as u8;
            }
            check_damaged(&b);
        }
    }
}

// ------------------------------------------------------------------ inside a PDF

/// A one-page PDF showing one image whose stream is `img` with the filter `filter`.
fn pdf_with_image(img: &[u8], filter: &str, w: u32, h: u32, cs: &str) -> Vec<u8> {
    let content = format!("q {w} 0 0 {h} 0 0 cm /Im1 Do Q");
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>").into_bytes(),
        [
            format!("<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /{cs} /BitsPerComponent 8 /Filter {filter} /Length {} >>\nstream\n", img.len()).as_bytes(),
            img,
            b"\nendstream",
        ]
        .concat(),
        [format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(), content.as_bytes(), b"\nendstream"].concat(),
    ];
    let mut d = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(d.len());
        d.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        d.extend_from_slice(o);
        d.extend_from_slice(b"\nendobj\n");
    }
    let xref = d.len();
    d.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        d.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    d.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    d
}

/// The image stream of an optimised PDF: its codec stages, and its bytes with the general filters undone.
fn image_of(pdf: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let doc = engine::pdf::xref::open(pdf).expect("readable");
    let (_, o) = doc.load(4).expect("image object");
    let Obj::Stream(s) = o else { panic!("not a stream") };
    let mut bytes = s.data.clone();
    let mut codecs = Vec::new();
    for st in chain(&s.dict).expect("chain") {
        if is_general(&st.name) {
            assert!(codecs.is_empty(), "general filter after the codec");
            bytes = decode_stage(&st, &bytes, 1 << 30).expect("decodes");
        } else {
            codecs.push(st.name.clone());
        }
    }
    (codecs, bytes)
}

fn quick() -> rewrite::Options {
    rewrite::Options { flate: engine::flate::Options::quick(), ..Default::default() }
}

#[test]
fn pdf_images_get_optimised_tables() {
    for (name, cs) in [("base420", "DeviceRGB"), ("gray", "DeviceGray"), ("restart1", "DeviceRGB"), ("cmyk", "DeviceCMYK")] {
        let jpeg = data(&format!("{name}.jpg"));
        let (w, h) = if name == "gray" { (61, 37) } else { (75, 53) };
        // A lone JPEG, and a JPEG behind Flate.
        let z = engine::flate::deflate_zlib(&jpeg, &engine::flate::Options::quick());
        for (img, filter) in [(jpeg.clone(), "/DCTDecode"), (z, "[/FlateDecode /DCTDecode]")] {
            let pdf = pdf_with_image(&img, filter, w, h, cs);
            let r = rewrite::optimize(&pdf, &quick());
            assert!(matches!(r.outcome, Outcome::Smaller(_)), "{name}: {:?}", r.outcome);
            let (codecs, bytes) = image_of(&r.data);
            assert_eq!(codecs, vec![b"DCTDecode".to_vec()], "{name}");
            assert_ne!(bytes, jpeg, "{name}: the JPEG was optimised");
            assert!(bytes.len() < jpeg.len());
            assert!(equivalent(&jpeg, &bytes), "{name}");
            assert_eq!(coefficients(&bytes), coefficients(&jpeg), "{name}");
        }
    }
}

#[test]
fn pdf_jpeg_behind_flate_is_the_optimised_one() {
    // A long comment Flate shrinks well: the image goes behind Flate, and the JPEG inside has the new tables.
    let a = data("base420.jpg");
    let mut com = vec![0xFF, 0xFE, 0x30, 0x02];
    com.extend(b"PDF engine test. ".iter().cycle().take(0x3000));
    let jpeg = [&a[..2], &com[..], &a[2..]].concat();
    let r = rewrite::optimize(&pdf_with_image(&jpeg, "/DCTDecode", 75, 53, "DeviceRGB"), &quick());
    assert!(matches!(r.outcome, Outcome::Smaller(_)));
    let doc = engine::pdf::xref::open(&r.data).unwrap();
    let Obj::Stream(s) = doc.load(4).unwrap().1 else { panic!() };
    assert_eq!(s.dict.get(b"Filter"), Some(&Obj::Array(vec![Obj::Name(b"FlateDecode".to_vec()), Obj::Name(b"DCTDecode".to_vec())])));
    let (_, inner) = image_of(&r.data);
    assert_eq!(inner, optimize(&jpeg).unwrap());
}

#[test]
fn pdf_images_we_cannot_optimise_stay_as_they_are() {
    let base = data("base420.jpg");
    let sof = find(&base, &[0xFF, 0xC0]);
    let mut twelve = base.clone();
    twelve[sof + 4] = 12;
    let truncated = base[..base.len() - 40].to_vec();
    let mut flipped = base.clone();
    let n = flipped.len();
    flipped[n - 30] ^= 0x40; // damaged coded data: may still read as a JPEG; then it must keep its coefficients
    // The layouts of the review: fill bytes after the tables (pdf.js), a quantisation segment 2 bytes too long,
    // content credentials.
    let com = segment(0xFE, &[b"thumbnail:".as_slice(), &data("tiny.jpg")].concat());
    let b = &base;
    let fill = [&b[..2], &pick(b, 0xE0), &com, &pick(b, 0xC4), &[0xFF, 0xFF], &pick(b, 0xDB), &pick(b, 0xC0), &pick(b, 0xDA), &pick(b, 0xD9)].concat();
    let dqt = find(b, &[0xFF, 0xDB]);
    let dqt_end = dqt + 2 + u16::from_be_bytes([b[dqt + 2], b[dqt + 3]]) as usize;
    let mut long_dqt = [&b[..dqt_end], &[0, 0][..], &b[dqt_end..]].concat();
    long_dqt[dqt + 3] += 2;
    let app11 = [&b[..2], &segment(0xEB, b"JP\x00\x01\x00\x00\x00\x01"), &b[2..]].concat();
    // (what, JPEG, must keep its bytes)
    let cases = [
        ("progressive", data("progressive.jpg"), true),
        ("arithmetic", data("arithmetic.jpg"), true),
        ("12-bit", twelve, true),
        ("truncated", truncated, true),
        ("flipped", flipped, false),
        ("fill bytes", fill, true),
        ("long DQT", long_dqt, true),
        ("APP11", app11, true),
    ];
    for (what, jpeg, keep) in cases {
        let pdf = pdf_with_image(&jpeg, "/DCTDecode", 75, 53, "DeviceRGB");
        let r = rewrite::optimize(&pdf, &quick());
        if let Outcome::Smaller(_) = r.outcome {
            let (codecs, bytes) = image_of(&r.data);
            assert_eq!(codecs, vec![b"DCTDecode".to_vec()], "{what}");
            if keep || optimize(&jpeg).is_err() {
                assert_eq!(bytes, jpeg, "{what}: a JPEG we can't optimise keeps its bytes");
            } else {
                assert!(equivalent(&jpeg, &bytes), "{what}");
                assert_eq!(coefficients(&bytes), coefficients(&jpeg), "{what}");
            }
        }
    }
}
