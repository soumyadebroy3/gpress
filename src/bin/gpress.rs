//! gpress on the command line: the library's three operations with every option as a flag, plus the
//! engine's measuring tools.
//!
//!   gpress optimize IN OUT [options]   a smaller PDF that any reader opens (or IN untouched, and why)
//!   gpress pack IN OUT [options]       the gpress archive of IN (any file)
//!   gpress unpack IN OUT [options]     the original from an archive; a damaged archive is an error
//!
//! Options (the library's `Options`, the same defaults): --effort N (0 fast .. 100 smallest, default 100;
//! --quick = 0), --threads N (0: every core the machine offers), --max-input BYTES, --max-stream BYTES
//! (normal PDF), --max-stream-archive BYTES, --max-decoded BYTES (the archive's decoded-bytes budget, for pack
//! and unpack), --max-len BYTES (unpack: a file declaring more is refused),
//! --objects-per-stream N, --time-budget MS (give up after that long), --ignore-conformance,
//! --no-flate-over-jpeg, --no-jpeg-tables, --no-jbig2, --keep-encrypted (normal PDF), --no-decrypt
//! (archive), --no-existing. The output never depends on the thread count or the time budget: a budget that
//! runs out gives an error and no file.
//!
//! Measuring tools (the engine's own modules, for the benchmarks): deflate, inflate, batch, pdf-batch,
//! repro, expand, cm-test, pack-batch, jpeg, jpeg-batch, jpeg-info, fax-decode, fax-encode, fax-scan,
//! fax-model, jbig2-encode, claim-archive (the security suite's crafted archive). `--iter`, `--exact`, `--chain`, `--nice`, `--nosplit`, `--noalt`, `--nomulti`
//! and `--nopolish` fine-tune the strong encoder's settings for the effort in the deflate tools; below effort
//! 20, where the fast encoder runs, any of them selects the strong encoder (its quick settings, fine-tuned).
//! `--fast N` (1 to `flate::fast::LEVELS`) uses the fast encoder at level N instead, and `--fast 0` the
//! strong encoder.
//!
//! Exit status: 0 done; a library error exits with its code (1 input too large, 2 not an archive or damaged,
//! 3 archive version, 4 internal error (a caught panic: a bug), 5 invalid options, 6 time budget, 7 declared
//! length over the limit, 8 needs the existing library); 2 for a usage or file error.

use gpress::flate::{self, Options};
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

/// A usage or file error: exit 2.
fn die(msg: &str) -> ! {
    eprintln!("gpress: {msg}");
    std::process::exit(2);
}

/// A library error: its message and its stable code as the exit status (1 too large, 2 damaged, 3 version,
/// 4 internal, 5 options, 6 time budget, 7 declared length, 8 needs the existing library). 4 is a caught
/// panic, a bug, and the security suite counts it as one rather than as a refusal.
fn fail(e: &gpress::Error) -> ! {
    eprintln!("gpress: {e}");
    std::process::exit(e.code());
}

const USAGE: &str = "\
usage: gpress optimize IN OUT [options]   a smaller PDF that any reader opens (or IN untouched, and why)
       gpress pack IN OUT [options]       the gpress archive of IN (any file)
       gpress unpack IN OUT [options]     the original from an archive (a damaged archive is an error)

options: --effort N (0 fast .. 100 smallest, default 100; --quick = 0)   --threads N (0: every core)
         --max-input BYTES   --max-stream BYTES (normal PDF)   --max-stream-archive BYTES   --max-decoded BYTES (archive,
         pack and unpack)   --max-len BYTES (unpack)   --objects-per-stream N   --time-budget MS   --ignore-conformance
         --no-flate-over-jpeg   --no-jpeg-tables   --no-jbig2   --keep-encrypted (normal PDF)   --no-decrypt (archive)
         --no-existing
measuring tools: deflate, inflate, batch, pdf-batch, repro, expand, cm-test, pack-batch, jpeg, jpeg-batch, jpeg-info,
         fax-decode, fax-encode, fax-scan, fax-model, jbig2-encode, claim-archive (see the comments at the top of
         src/bin/gpress.rs)";

/// The flags that take a value.
const WITH_VALUE: &[&str] = &[
    "--iter",
    "--exact",
    "--chain",
    "--nice",
    "--fast",
    "--threads",
    "--effort",
    "--max-input",
    "--max-stream",
    "--max-stream-archive",
    "--max-decoded",
    "--max-len",
    "--objects-per-stream",
    "--time-budget",
];

/// The flags that fine-tune the strong encoder's settings in the deflate tools.
const TUNING: [&str; 8] = ["--iter", "--exact", "--chain", "--nice", "--nosplit", "--noalt", "--nomulti", "--nopolish"];

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
            skip_next = WITH_VALUE.contains(&a.as_str());
            continue;
        }
        pos.push(a);
    }
    let num = |name: &str| -> Option<u64> {
        let i = args.iter().position(|a| a == name)?;
        let v = args.get(i + 1).unwrap_or_else(|| die(&format!("{name} needs a number")));
        Some(v.parse().unwrap_or_else(|_| die(&format!("{name}: {v} is not a number"))))
    };
    let size = |name: &str| num(name).map(|n| usize::try_from(n).unwrap_or(usize::MAX));
    // The library's options, from the flags.
    let effort = if flag("--quick") { 0 } else { num("--effort").unwrap_or(100).min(255) as u8 };
    let mut lib = gpress::Options::default().effort(effort);
    if let Some(n) = size("--threads") {
        lib.threads = n;
    }
    if let Some(n) = size("--max-input") {
        lib.max_input = n;
    }
    if let Some(n) = size("--max-stream") {
        lib.max_stream = n;
    }
    if let Some(n) = size("--max-stream-archive") {
        lib.max_stream_archive = n;
    }
    if let Some(n) = num("--max-decoded") {
        lib.max_decoded = n;
    }
    if let Some(n) = num("--max-len") {
        lib.max_len = n;
    }
    if let Some(n) = size("--objects-per-stream") {
        lib.objects_per_stream = n;
    }
    if let Some(ms) = num("--time-budget") {
        lib.time_budget = Some(Duration::from_millis(ms));
    }
    lib.respect_conformance = !flag("--ignore-conformance");
    lib.flate_over_jpeg = !flag("--no-flate-over-jpeg");
    lib.jpeg_tables = !flag("--no-jpeg-tables");
    lib.jbig2 = !flag("--no-jbig2");
    lib.rewrite_encrypted = !flag("--keep-encrypted");
    lib.decrypt = !flag("--no-decrypt");
    lib.use_existing_libraries = !flag("--no-existing");
    lib.check().unwrap_or_else(|e| fail(&e));
    let threads = lib.threads;
    // The Flate settings of the measuring tools: the effort's, fine-tuned by the flags below it.
    let mut opt = Options::effort(effort);
    if let Some(v) = size("--iter") {
        opt.iterations = v;
    }
    if let Some(v) = size("--exact") {
        opt.exact_rounds = v;
    }
    if let Some(v) = size("--chain") {
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
    if let Some(v) = size("--nice") {
        opt.nice = v;
    }
    if flag("--nopolish") {
        opt.polish = false;
    }
    // Those flags tune the strong encoder: below effort 20 they select it.
    if TUNING.iter().any(|f| flag(f)) {
        opt.fast = 0;
    }
    if let Some(v) = num("--fast") {
        opt.fast = v.min(255) as u8;
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
                let opt = Options {
                    shared: opt.shared.as_ref().map(|_| flate::deflate::Shared::new(threads)),
                    ..opt.clone()
                };
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
            let r = gpress::optimize(&data, &lib).unwrap_or_else(|e| fail(&e));
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
                match gpress::optimize(&data, &lib) {
                    Ok(r) => {
                        std::fs::write(o, &r.data).unwrap();
                        writeln!(out, "{}", describe(i, data.len(), &r, t.elapsed().as_secs_f64())).unwrap();
                    }
                    Err(e) => writeln!(out, "{i}\tERROR\t{}\t{e}", data.len()).unwrap(),
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
                            Some(p) => format!(
                                "L{} W{} M{}{}",
                                p.level,
                                p.window_bits,
                                p.mem_level,
                                if p.filtered { " F" } else { "" }
                            ),
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
            let c = gpress::compress(&data, &lib).unwrap_or_else(|e| fail(&e));
            std::fs::write(o, &c.data).unwrap_or_else(|e| die(&format!("{o}: {e}")));
            println!("{}", describe_archive(i, data.len(), &c, t.elapsed().as_secs_f64()));
        }
        Some("unpack") => {
            let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            let out = gpress::expand(&data, &lib).unwrap_or_else(|e| fail(&e));
            std::fs::write(o, &out).unwrap_or_else(|e| die(&format!("{o}: {e}")));
        }
        Some("claim-archive") => {
            // For the security suite: gpress claim-archive IN OUT BYTES -> OUT is an archive of IN whose one
            // decoded part claims BYTES (see pack::crafted_claim); unpack must refuse it within its budget.
            let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
            let claim: u64 = pos.get(3).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need BYTES"));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            std::fs::write(o, gpress::pack::crafted_claim(&data, claim)).unwrap_or_else(|e| die(&format!("{o}: {e}")));
        }
        Some("expand") => {
            // For measuring only: gpress expand IN OUT -> OUT is IN with rebuildable streams decoded (see
            // pack::expand); prints the bytes a restore would need on top.
            let (i, o) = (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
            let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
            let (x, extra) = gpress::pack::expand(&data);
            std::fs::write(o, &x).unwrap_or_else(|e| die(&format!("{o}: {e}")));
            println!("{i}\t{}\t{}\t{extra}", data.len(), x.len());
        }
        Some("cm-test") => {
            // gpress cm-test FILE KIND [STRIDE BPP]: size of FILE coded by the model as KIND.
            use gpress::cm::model::{Geom, Kind, Model};
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
            let mut e = gpress::cm::Encoder::new();
            for &b in &data {
                m.code_byte(&mut e, Some(b));
            }
            let z = e.finish();
            println!(
                "{} -> {} bytes ({:.3} bits/byte) in {:.2}s",
                data.len(),
                z.len(),
                8.0 * z.len() as f64 / data.len() as f64,
                t.elapsed().as_secs_f64()
            );
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
                match gpress::compress(&data, &lib) {
                    Ok(c) => {
                        std::fs::write(o, &c.data).unwrap();
                        writeln!(out, "{}", describe_archive(i, data.len(), &c, t.elapsed().as_secs_f64())).unwrap();
                    }
                    Err(e) => writeln!(out, "{i}\tERROR\t{}\t{e}", data.len()).unwrap(),
                }
                out.flush().unwrap();
            }
        }
        Some("jpeg-info") => {
            for i in &pos[1..] {
                let data = std::fs::read(i).unwrap_or_else(|e| die(&format!("{i}: {e}")));
                match gpress::jpeg::info(&data) {
                    Ok(s) => println!("{i}\t{s}"),
                    Err(e) => println!("{i}\trefused: {e}"),
                }
            }
        }
        Some("jpeg") | Some("jpeg-batch") => {
            // gpress jpeg IN OUT: one JPEG file with optimal Huffman tables (OUT is a copy of IN when refused);
            // jpeg-batch: stdin lines "IN<TAB>OUT". Prints IN, bytes in, bytes out, seconds, and why not if refused.
            let one = |i: &str, o: &str| -> String {
                let data = match std::fs::read(i) {
                    Ok(d) => d,
                    Err(e) => return format!("{i}\tERROR\t{e}"),
                };
                let t = Instant::now();
                let r = gpress::jpeg::optimize(&data);
                let secs = t.elapsed().as_secs_f64();
                let (bytes, why) = match &r {
                    Ok(x) => (x.as_slice(), "optimized".to_string()),
                    Err(e) => (data.as_slice(), format!("refused: {e}")),
                };
                std::fs::write(o, bytes).unwrap_or_else(|e| die(&format!("{o}: {e}")));
                format!("{i}\t{}\t{}\t{secs:.3}\t{why}", data.len(), bytes.len())
            };
            if pos[0] == "jpeg" {
                let (i, o) =
                    (pos.get(1).unwrap_or_else(|| die("need IN")), pos.get(2).unwrap_or_else(|| die("need OUT")));
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
            // gpress fax-decode RAW K COLUMNS ALIGN(0|1) OUT.pbm: our CCITT decoder on raw fax data (for checking
            // it against other decoders); prints rows, why it stopped, and whether the bytes come back exactly.
            use gpress::fax::ccitt;
            let raw =
                std::fs::read(pos.get(1).unwrap_or_else(|| die("need RAW"))).unwrap_or_else(|e| die(&e.to_string()));
            let k: i64 = pos.get(2).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need K"));
            let cols: usize = pos.get(3).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need COLUMNS"));
            let align = pos.get(4).map(|s| s.as_str()) == Some("1");
            let o = pos.get(5).unwrap_or_else(|| die("need OUT"));
            let p = ccitt::parse(&raw, ccitt::Scheme::from_k(k), cols, align, ccitt::Limits::default())
                .unwrap_or_else(|| die("bad parameters"));
            let exact = ccitt::encode(&p.image, &p.layout) == raw;
            std::fs::write(o, pbm(&p.image)).unwrap_or_else(|e| die(&e.to_string()));
            println!(
                "rows={} stop={:?} rule={:?} end_codes={} trailing={} exact={exact} why={:?}",
                p.image.rows(),
                p.stop,
                p.layout.rule,
                p.layout.end.len(),
                p.layout.trailing.len(),
                p.why
            );
        }
        Some("fax-encode") => {
            // gpress fax-encode IN.pbm K EOL(0|1) OUT.raw: our CCITT encoder (EOFB for K < 0, RTC otherwise).
            use gpress::fax::ccitt;
            let data =
                std::fs::read(pos.get(1).unwrap_or_else(|| die("need IN.pbm"))).unwrap_or_else(|e| die(&e.to_string()));
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
            // gpress fax-scan FILE.pdf: every CCITT image stream our reader finds, and what normal-PDF output does
            // with it.
            let data =
                std::fs::read(pos.get(1).unwrap_or_else(|| die("need FILE"))).unwrap_or_else(|e| die(&e.to_string()));
            let doc = gpress::pdf::xref::open(&data).unwrap_or_else(|e| die(&format!("unreadable: {e:?}")));
            for &n in doc.entries.keys() {
                if let Ok((_, gpress::pdf::object::Obj::Stream(s))) = doc.load(n) {
                    let fax = gpress::pdf::filters::chain(&s.dict)
                        .is_some_and(|c| c.iter().any(|st| st.name == b"CCITTFaxDecode"));
                    if fax {
                        let resolve = |o: &gpress::pdf::object::Obj| match o {
                            gpress::pdf::object::Obj::Ref(r, _) => doc.load(*r).ok().map(|x| x.1),
                            x => Some(x.clone()),
                        };
                        println!("{n}\t{}", gpress::fax::pdf::explain(&s, &resolve));
                    }
                }
            }
        }
        Some("fax-model") => {
            // gpress fax-model RAW K COLUMNS ALIGN(0|1): size of a raw fax stream as an archive fax part, with
            // the time to code it and to restore it (checked).
            use gpress::fax::{ccitt, part};
            let raw =
                std::fs::read(pos.get(1).unwrap_or_else(|| die("need RAW"))).unwrap_or_else(|e| die(&e.to_string()));
            let k: i64 = pos.get(2).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need K"));
            let cols: usize = pos.get(3).and_then(|x| x.parse().ok()).unwrap_or_else(|| die("need COLUMNS"));
            let align = pos.get(4).map(|s| s.as_str()) == Some("1");
            // Prepared as pack prepares it (the stream's byte alignment, or the other one if that reads more).
            let p = part::prepare(&raw, ccitt::Scheme::from_k(k), cols, align)
                .unwrap_or_else(|| die("not stored as a fax part"));
            // The work budget as if the stream started a file (where it is smallest); the size is measured anyway.
            let within = part::Work::default().admit(&p, 0);
            let at = if within { 0 } else { usize::MAX / 4 };
            let t = Instant::now();
            let mut e = gpress::cm::Encoder::new();
            let back = part::code(&mut e, &mut part::FaxCoder::new(raw.len()), Some(&p), at, raw.len());
            let z = e.finish();
            let t1 = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let mut d = gpress::cm::Decoder::new(&z);
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
            // gpress jbig2-encode IN.pbm OUT.jb2 [invert]: our JBIG2Decode stream (embedded form) of a PBM.
            let data =
                std::fs::read(pos.get(1).unwrap_or_else(|| die("need IN.pbm"))).unwrap_or_else(|e| die(&e.to_string()));
            let o = pos.get(2).unwrap_or_else(|| die("need OUT"));
            let invert = pos.get(3).map(|s| s.as_str()) == Some("invert");
            let img = read_pbm(&data).unwrap_or_else(|| die("not a binary PBM"));
            let s = gpress::fax::jbig2::pdf_stream(&img, invert);
            let back = gpress::fax::jbig2::read_pdf_stream(&s, invert, usize::MAX);
            std::fs::write(o, &s).unwrap_or_else(|e| die(&e.to_string()));
            println!("{} bytes, our decoder gives the image back: {}", s.len(), back.as_ref() == Some(&img));
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

/// A binary PBM (1 is black) of a bilevel image.
fn pbm(img: &gpress::fax::ccitt::Image) -> Vec<u8> {
    let w = img.width;
    let mut out = format!("P4\n{} {}\n", w, img.rows()).into_bytes();
    let mut px = vec![0u8; w];
    for r in 0..img.rows() {
        gpress::fax::ccitt::expand_row(img.row(r), &mut px);
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

fn read_pbm(d: &[u8]) -> Option<gpress::fax::ccitt::Image> {
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
    let stride = w.div_ceil(8);
    if body.len() < stride * h || w == 0 {
        return None;
    }
    let mut img = gpress::fax::ccitt::Image::new(w);
    let mut px = vec![0u8; w];
    let mut ch = Vec::new();
    for r in 0..h {
        for x in 0..w {
            px[x] = (body[r * stride + x / 8] >> (7 - x % 8)) & 1;
        }
        gpress::fax::ccitt::row_changes(&px, &mut ch);
        img.push_row(&ch);
    }
    Some(img)
}

fn describe(name: &str, in_len: usize, r: &gpress::Optimized, secs: f64) -> String {
    match &r.outcome {
        gpress::Outcome::Smaller(s) => format!(
            "{name}\tSMALLER\t{in_len}\t{}\t{secs:.2}\tobjects={} streams={} reencoded={} kept={} objstms={} dropped={} \
             jpegs_retabled={} fax_to_jbig2={}{}",
            r.data.len(),
            s.objects,
            s.streams,
            s.streams_reencoded,
            s.streams_kept,
            s.object_streams,
            s.unreachable_objects_dropped,
            s.jpegs_retabled,
            s.fax_to_jbig2,
            if s.encryption.is_empty() { String::new() } else { format!(" encrypted={}", s.encryption) }
        ),
        gpress::Outcome::Untouched(why) => format!("{name}\tUNTOUCHED\t{in_len}\t{}\t{secs:.2}\t{why}", r.data.len()),
    }
}

fn describe_archive(name: &str, in_len: usize, c: &gpress::Compressed, secs: f64) -> String {
    format!(
        "{name}\t{in_len}\t{}\t{secs:.2}\t{} streams={} rebuilt={} rebuilt_bytes={} lepton={} type1={} fax={} decrypted={} \
         decoded_bytes={}",
        c.data.len(),
        if c.stored { "STORED" } else { "PACKED" },
        c.streams,
        c.streams_rebuilt,
        c.rebuilt_bytes,
        c.streams_lepton,
        c.type1_fonts,
        c.fax_images,
        c.streams_decrypted,
        c.decoded_bytes
    )
}
