//! Command-line tool for the engine (testing and measuring).
//!
//!   pdfe deflate IN OUT [--quick] [--raw]     zlib-wrapped (or raw) Deflate of IN
//!   pdfe inflate IN OUT [--raw]               decode with our decoder
//!   pdfe batch [--quick]                      stdin lines "IN OUT": compress each, verify, print sizes
//!   pdfe optimize IN OUT [--effort N]         normal PDF; N from 0 (fast) to 100 (default); --quick = 0
//!   pdfe pack IN OUT [--effort N]             peyaji format (the effort only changes the encoder's choices;
//!                                             --no-decrypt keeps encrypted streams as stored)
//!
//! --threads N sets the threads (default: what the machine offers, at most 8); the output is the same.

use engine::flate::{self, Options};
use std::io::{BufRead, Write};
use std::time::Instant;

fn die(msg: &str) -> ! {
    eprintln!("pdfe: {msg}");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let mut pos: Vec<&String> = Vec::new();
    let mut skip_next = false;
    for a in &args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with("--") {
            skip_next = matches!(a.as_str(), "--iter" | "--exact" | "--chain" | "--nice" | "--threads" | "--effort");
            continue;
        }
        pos.push(a);
    }
    let num = |name: &str| -> Option<usize> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok())
    };
    let effort: u8 = if flag("--quick") { 0 } else { num("--effort").map_or(100, |e| e.min(100) as u8) };
    let threads = num("--threads").unwrap_or(0);
    // pack: --no-decrypt keeps encrypted streams as stored (for measuring).
    let decrypt = !flag("--no-decrypt");
    let mut opt = Options::effort(effort);
    if let Some(v) = num("--iter") {
        opt.iterations = v;
    }
    if let Some(v) = num("--exact") {
        opt.exact_rounds = v;
    }
    if let Some(v) = num("--chain") {
        opt.max_chain = v;
    }
    if flag("--nosplit") {
        opt.split = false;
    }
    if flag("--noalt") {
        opt.alternatives = false;
    }
    if flag("--nomulti") {
        opt.multi_start_limit = 0;
    }
    if let Some(v) = num("--nice") {
        opt.nice = v;
    }
    if flag("--nopolish") {
        opt.polish = false;
    }
    if threads > 0 {
        opt.shared = Some(flate::deflate::Shared::new(threads));
    }
    match pos.first().map(|s| s.as_str()) {
        Some("deflate") => {
            let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            let out = if flag("--raw") { flate::deflate(&data, &opt) } else { flate::deflate_zlib(&data, &opt) };
            std::fs::write(o, &out).unwrap_or_else(|e| die(&format!("{o}: {e}")));
        }
        Some("inflate") => {
            let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            let out = if flag("--raw") {
                flate::inflate(&data, 1 << 31).map(|x| x.data).unwrap_or_else(|e| die(&format!("{e:?}")))
            } else {
                flate::zlib::unwrap(&data, 1 << 31).map(|x| x.data).unwrap_or_else(|e| die(&format!("{e:?}")))
            };
            std::fs::write(o, &out).unwrap_or_else(|e| die(&format!("{o}: {e}")));
        }
        Some("batch") => {
            let stdin = std::io::stdin();
            let mut out = std::io::stdout().lock();
            for line in stdin.lock().lines() {
                let line = line.unwrap();
                let mut it = line.split('\t');
                let (Some(i), Some(o)) = (it.next(), it.next()) else { continue };
                let data = match std::fs::read(i) {
                    Ok(d) => d,
                    Err(e) => {
                        writeln!(out, "{i}\tERROR\t{e}").unwrap();
                        continue;
                    }
                };
                let t = Instant::now();
                // A fresh pool (and memory of inputs) for each file.
                let opt = Options { shared: opt.shared.as_ref().map(|_| flate::deflate::Shared::new(threads)), ..opt.clone() };
                let z = flate::deflate_zlib(&data, &opt);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                let ok = flate::zlib::unwrap(&z, data.len() + 1).map(|u| u.data == data).unwrap_or(false);
                if !ok {
                    writeln!(out, "{i}\tMISMATCH\t{}", data.len()).unwrap();
                    continue;
                }
                std::fs::write(o, &z).unwrap();
                writeln!(out, "{i}\t{}\t{}\t{:.1}", data.len(), z.len(), ms).unwrap();
                out.flush().unwrap();
            }
        }
        Some("optimize") => {
            let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            let t = Instant::now();
            let r = engine::pdf::rewrite::optimize(&data, &pdf_options(&opt, effort, threads));
            std::fs::write(o, &r.data).unwrap_or_else(|e| die(&format!("{o}: {e}")));
            println!("{}", describe(i, data.len(), &r, t.elapsed().as_secs_f64()));
        }
        Some("pdf-batch") => {
            // stdin lines "IN<TAB>OUT"
            let stdin = std::io::stdin();
            let mut out = std::io::stdout().lock();
            for line in stdin.lock().lines() {
                let line = line.unwrap();
                let mut it = line.split('\t');
                let (Some(i), Some(o)) = (it.next(), it.next()) else { continue };
                let data = match std::fs::read(i) {
                    Ok(d) => d,
                    Err(e) => {
                        writeln!(out, "{i}\tERROR\t{e}").unwrap();
                        continue;
                    }
                };
                let t = Instant::now();
                let r = std::panic::catch_unwind(|| engine::pdf::rewrite::optimize(&data, &pdf_options(&opt, effort, threads)));
                match r {
                    Ok(r) => {
                        std::fs::write(o, &r.data).unwrap();
                        writeln!(out, "{}", describe(i, data.len(), &r, t.elapsed().as_secs_f64())).unwrap();
                    }
                    Err(_) => writeln!(out, "{i}\tPANIC\t{}", data.len()).unwrap(),
                }
                out.flush().unwrap();
            }
        }
        Some("repro") => {
            // stdin: paths of whole zlib streams; prints the zlib settings that rebuild each exactly.
            let stdin = std::io::stdin();
            let mut out = std::io::stdout().lock();
            for line in stdin.lock().lines() {
                let path = line.unwrap();
                let raw = match std::fs::read(&path) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let t = Instant::now();
                let res = match flate::zlib::unwrap(&raw, 1 << 30) {
                    Ok(u) => {
                        let used = raw.len() - u.trailing;
                        match flate::zlibexact::find_params(&u.data, &raw[..used]) {
                            Some(p) => format!("L{} W{} M{}{}", p.level, p.window_bits, p.mem_level, if p.filtered { " F" } else { "" }),
                            None => "none".into(),
                        }
                    }
                    Err(e) => format!("undecodable {e:?}"),
                };
                writeln!(out, "{path}\t{}\t{res}\t{:.3}", raw.len(), t.elapsed().as_secs_f64()).unwrap();
            }
        }
        Some("pack") => {
            let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            let t = Instant::now();
            let (z, st) = engine::pack::pack_with(&data, &engine::pack::PackOptions { effort, threads, decrypt });
            std::fs::write(o, &z).unwrap_or_else(|e| die(&format!("{o}: {e}")));
            println!("{i}\t{}\t{}\t{:.2}\t{:?}", data.len(), z.len(), t.elapsed().as_secs_f64(), st);
        }
        Some("unpack") => {
            let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            let out = engine::pack::unpack(&data).unwrap_or_else(|e| die(&e));
            std::fs::write(o, &out).unwrap_or_else(|e| die(&format!("{o}: {e}")));
        }
        Some("expand") => {
            // For measuring only: pdfe expand IN OUT -> OUT is IN with rebuildable streams decoded (see
            // pack::expand); prints the bytes a restore would need on top.
            let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            let (x, extra) = engine::pack::expand(&data);
            std::fs::write(o, &x).unwrap_or_else(|e| die(&format!("{o}: {e}")));
            println!("{i}\t{}\t{}\t{extra}", data.len(), x.len());
        }
        Some("cm-test") => {
            // pdfe cm-test FILE KIND [STRIDE BPP]: size of FILE coded by the model as KIND.
            use engine::cm::model::{Geom, Kind, Model};
            let i = pos.get(1).unwrap_or_else(|| die("need FILE"));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            let kind = match pos.get(2).map(|s| s.as_str()) {
                Some("image") => Kind::Image,
                Some("content") => Kind::Content,
                Some("syntax") => Kind::Syntax,
                Some("font") => Kind::Font,
                Some("raw") => Kind::Raw,
                _ => Kind::Data,
            };
            let stride: usize = pos.get(3).and_then(|x| x.parse().ok()).unwrap_or(0);
            let bpp: usize = pos.get(4).and_then(|x| x.parse().ok()).unwrap_or(1);
            let exact = pos.get(5).map(|s| s.as_str()) == Some("exact");
            let t = Instant::now();
            let mut m = Model::new(18);
            m.set_kind(kind, Geom { stride, bpp, exact }, 0);
            let mut e = engine::cm::Encoder::new();
            for &b in &data {
                m.code_byte(&mut e, Some(b));
            }
            let z = e.finish();
            println!("{} -> {} bytes ({:.3} bits/byte) in {:.2}s", data.len(), z.len(), 8.0 * z.len() as f64 / data.len() as f64, t.elapsed().as_secs_f64());
        }
        Some("pack-batch") => {
            let stdin = std::io::stdin();
            let mut out = std::io::stdout().lock();
            for line in stdin.lock().lines() {
                let line = line.unwrap();
                let mut it = line.split('\t');
                let (Some(i), Some(o)) = (it.next(), it.next()) else { continue };
                let Ok(data) = std::fs::read(i) else { continue };
                let t = Instant::now();
                let r = std::panic::catch_unwind(|| engine::pack::pack_with(&data, &engine::pack::PackOptions { effort, threads, decrypt }));
                match r {
                    Ok((z, st)) => {
                        std::fs::write(o, &z).unwrap();
                        writeln!(out, "{i}\t{}\t{}\t{:.2}\t{:?}", data.len(), z.len(), t.elapsed().as_secs_f64(), st).unwrap();
                    }
                    Err(_) => writeln!(out, "{i}\tPANIC").unwrap(),
                }
                out.flush().unwrap();
            }
        }
        Some("jpeg-info") => {
            for i in &pos[1..] {
                let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
                match engine::jpeg::info(&data) {
                    Ok(s) => println!("{i}\t{s}"),
                    Err(e) => println!("{i}\trefused: {e}"),
                }
            }
        }
        Some("jpeg") | Some("jpeg-batch") => {
            // pdfe jpeg IN OUT: one JPEG file with optimal Huffman tables (OUT is a copy of IN when refused);
            // jpeg-batch: stdin lines "IN<TAB>OUT". Prints IN, bytes in, bytes out, seconds, and why not if refused.
            let one = |i: &str, o: &str| -> String {
                let data = match std::fs::read(i) {
                    Ok(d) => d,
                    Err(e) => return format!("{i}\tERROR\t{e}"),
                };
                let t = Instant::now();
                let r = engine::jpeg::optimize(&data);
                let secs = t.elapsed().as_secs_f64();
                let (bytes, why) = match &r {
                    Ok(x) => (x.as_slice(), "optimized".to_string()),
                    Err(e) => (data.as_slice(), format!("refused: {e}")),
                };
                std::fs::write(o, bytes).unwrap_or_else(|e| die(&format!("{o}: {e}")));
                format!("{i}\t{}\t{}\t{secs:.3}\t{why}", data.len(), bytes.len())
            };
            if pos[0] == "jpeg" {
                let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
                println!("{}", one(i, o));
            } else {
                let mut out = std::io::stdout().lock();
                for line in std::io::stdin().lock().lines() {
                    let line = line.unwrap();
                    let mut it = line.split('\t');
                    let (Some(i), Some(o)) = (it.next(), it.next()) else { continue };
                    let r = std::panic::catch_unwind(|| one(i, o)).unwrap_or_else(|_| format!("{i}\tPANIC"));
                    writeln!(out, "{r}").unwrap();
                    out.flush().unwrap();
                }
            }
        }
        Some("fax-decode") => {
            // pdfe fax-decode RAW K COLUMNS ALIGN(0|1) OUT.pbm: our CCITT decoder on raw fax data (for checking
            // it against other decoders); prints rows, why it stopped, and whether the bytes come back exactly.
            use engine::fax::ccitt;
            let raw = std::fs::read(pos.get(1).unwrap_or_else(|| die("need RAW"))).unwrap_or_else(|e| die(&e.to_string()));
            let k: i64 = pos.get(2).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need K"));
            let cols: usize = pos.get(3).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need COLUMNS"));
            let align = pos.get(4).map(|s| s.as_str()) == Some("1");
            let o = pos.get(5).unwrap_or_else(|| die("need OUT"));
            let p = ccitt::parse(&raw, ccitt::Scheme::from_k(k), cols, align, ccitt::Limits::default()).unwrap_or_else(|| die("bad parameters"));
            let exact = ccitt::encode(&p.image, &p.layout) == raw;
            std::fs::write(o, pbm(&p.image)).unwrap_or_else(|e| die(&e.to_string()));
            println!("rows={} stop={:?} rule={:?} end_codes={} trailing={} exact={exact} why={:?}", p.image.rows(), p.stop, p.layout.rule, p.layout.end.len(), p.layout.trailing.len(), p.why);
        }
        Some("fax-encode") => {
            // pdfe fax-encode IN.pbm K EOL(0|1) OUT.raw: our CCITT encoder (EOFB for K < 0, RTC otherwise).
            use engine::fax::ccitt;
            let data = std::fs::read(pos.get(1).unwrap_or_else(|| die("need IN.pbm"))).unwrap_or_else(|e| die(&e.to_string()));
            let k: i64 = pos.get(2).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need K"));
            let eol = pos.get(3).map(|s| s.as_str()) == Some("1");
            let o = pos.get(4).unwrap_or_else(|| die("need OUT"));
            let img = read_pbm(&data).unwrap_or_else(|| die("not a binary PBM"));
            let scheme = ccitt::Scheme::from_k(k);
            let gaps = (0..img.rows()).map(|r| ccitt::Gap { fill: 0, eol, tag: k > 0 && r as i64 % k == 0 }).collect();
            let n_end = if k < 0 { 2 } else { 6 };
            let end = vec![ccitt::Gap { fill: 0, eol: true, tag: k > 0 }; n_end];
            let lay = ccitt::Layout { scheme, rule: ccitt::Rule::Standard, gaps, end, pad: 0, trailing: Vec::new() };
            std::fs::write(o, ccitt::encode(&img, &lay)).unwrap_or_else(|e| die(&e.to_string()));
        }
        Some("fax-scan") => {
            // pdfe fax-scan FILE.pdf: every CCITT image stream our reader finds, and what normal-PDF output does
            // with it.
            let data = std::fs::read(pos.get(1).unwrap_or_else(|| die("need FILE"))).unwrap_or_else(|e| die(&e.to_string()));
            let doc = engine::pdf::xref::open(&data).unwrap_or_else(|e| die(&format!("unreadable: {e:?}")));
            for &n in doc.entries.keys() {
                if let Ok((_, engine::pdf::object::Obj::Stream(s))) = doc.load(n) {
                    let fax = engine::pdf::filters::chain(&s.dict).map_or(false, |c| c.iter().any(|st| st.name == b"CCITTFaxDecode"));
                    if fax {
                        let resolve = |o: &engine::pdf::object::Obj| match o {
                            engine::pdf::object::Obj::Ref(r, _) => doc.load(*r).ok().map(|x| x.1),
                            x => Some(x.clone()),
                        };
                        println!("{n}\t{}", engine::fax::pdf::explain(&s, &resolve));
                    }
                }
            }
        }
        Some("fax-model") => {
            // pdfe fax-model RAW K COLUMNS ALIGN(0|1): size of a raw fax stream as a peyaji fax part, with
            // the time to code it and to restore it (checked).
            use engine::fax::{ccitt, part};
            let raw = std::fs::read(pos.get(1).unwrap_or_else(|| die("need RAW"))).unwrap_or_else(|e| die(&e.to_string()));
            let k: i64 = pos.get(2).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need K"));
            let cols: usize = pos.get(3).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need COLUMNS"));
            let align = pos.get(4).map(|s| s.as_str()) == Some("1");
            // Prepared as pack prepares it (the stream's byte alignment, or the other one if that reads more).
            let p = part::prepare(&raw, ccitt::Scheme::from_k(k), cols, align).unwrap_or_else(|| die("not stored as a fax part"));
            // The work budget as if the stream started a file (where it is smallest); the size is measured anyway.
            let within = part::Work::default().admit(&p, 0);
            let at = if within { 0 } else { usize::MAX / 4 };
            let t = Instant::now();
            let mut e = engine::cm::Encoder::new();
            let back = part::code(&mut e, &mut part::FaxCoder::new(raw.len()), Some(&p), at, raw.len());
            let z = e.finish();
            let t1 = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let mut d = engine::cm::Decoder::new(&z);
            let restored = part::code(&mut d, &mut part::FaxCoder::new(raw.len()), None, at, raw.len());
            let t2 = t.elapsed().as_secs_f64();
            println!(
                "{} -> {} bytes ({} rows, {} trailing), coded in {t1:.2}s, restored in {t2:.2}s, exact: {}, within the work budget at the start of a file: {within}",
                raw.len(),
                z.len(),
                p.image.rows(),
                p.layout.trailing.len(),
                back.as_deref() == Some(&raw[..]) && restored.as_deref() == Some(&raw[..])
            );
        }
        Some("jbig2-encode") => {
            // pdfe jbig2-encode IN.pbm OUT.jb2 [invert]: our JBIG2Decode stream (embedded form) of a PBM.
            let data = std::fs::read(pos.get(1).unwrap_or_else(|| die("need IN.pbm"))).unwrap_or_else(|e| die(&e.to_string()));
            let o = pos.get(2).unwrap_or_else(|| die("need OUT"));
            let invert = pos.get(3).map(|s| s.as_str()) == Some("invert");
            let img = read_pbm(&data).unwrap_or_else(|| die("not a binary PBM"));
            let s = engine::fax::jbig2::pdf_stream(&img, invert);
            let back = engine::fax::jbig2::read_pdf_stream(&s, invert, usize::MAX);
            std::fs::write(o, &s).unwrap_or_else(|e| die(&e.to_string()));
            println!("{} bytes, our decoder gives the image back: {}", s.len(), back.as_ref() == Some(&img));
        }
        _ => die("usage: pdfe deflate|inflate|batch|optimize|pdf-batch|repro|pack|unpack|pack-batch|jpeg|jpeg-batch|fax-decode|fax-encode|fax-scan|fax-model|jbig2-encode ..."),
    }
}

/// A binary PBM (1 is black) of a bilevel image.
fn pbm(img: &engine::fax::ccitt::Image) -> Vec<u8> {
    let w = img.width;
    let mut out = format!("P4\n{} {}\n", w, img.rows()).into_bytes();
    let mut px = vec![0u8; w];
    for r in 0..img.rows() {
        engine::fax::ccitt::expand_row(img.row(r), &mut px);
        for chunk in px.chunks(8) {
            let mut b = 0u8;
            for (i, &p) in chunk.iter().enumerate() {
                b |= p << (7 - i);
            }
            out.push(b);
        }
    }
    out
}

fn read_pbm(d: &[u8]) -> Option<engine::fax::ccitt::Image> {
    // "P4", width, height, one whitespace byte, then packed rows (no comments).
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 3 {
        while i < d.len() && d[i].is_ascii_whitespace() {
            i += 1;
        }
        let s = i;
        while i < d.len() && !d[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&d[s..i]).ok()?.to_string());
    }
    if fields[0] != "P4" {
        return None;
    }
    let w: usize = fields[1].parse().ok()?;
    let h: usize = fields[2].parse().ok()?;
    let body = d.get(i + 1..)?;
    let stride = (w + 7) / 8;
    if body.len() < stride * h || w == 0 {
        return None;
    }
    let mut img = engine::fax::ccitt::Image::new(w);
    let mut px = vec![0u8; w];
    let mut ch = Vec::new();
    for r in 0..h {
        for x in 0..w {
            px[x] = (body[r * stride + x / 8] >> (7 - x % 8)) & 1;
        }
        engine::fax::ccitt::row_changes(&px, &mut ch);
        img.push_row(&ch);
    }
    Some(img)
}


fn pdf_options(f: &Options, effort: u8, threads: usize) -> engine::pdf::rewrite::Options {
    engine::pdf::rewrite::Options { flate: Options { shared: None, ..f.clone() }, effort, threads, ..Default::default() }
}

fn describe(name: &str, in_len: usize, r: &engine::pdf::rewrite::Optimized, secs: f64) -> String {
    use engine::pdf::rewrite::Outcome;
    match &r.outcome {
        Outcome::Smaller(s) => format!(
            "{name}\tSMALLER\t{in_len}\t{}\t{secs:.2}\tobjects={} streams={} reencoded={} kept={} objstms={} dropped={}{}",
            r.data.len(),
            s.objects,
            s.streams,
            s.streams_reencoded,
            s.streams_kept,
            s.objstms,
            s.dropped_unreachable,
            if s.encryption.is_empty() { String::new() } else { format!(" encrypted={}", s.encryption) }
        ),
        Outcome::Untouched(why) => format!("{name}\tUNTOUCHED\t{in_len}\t{}\t{secs:.2}\t{why}", r.data.len()),
    }
}
