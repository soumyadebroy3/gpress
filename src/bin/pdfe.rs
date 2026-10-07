//! Command-line tool for the engine (testing and measuring).
//!
//!   pdfe deflate IN OUT [--quick] [--raw]     zlib-wrapped (or raw) Deflate of IN
//!   pdfe inflate IN OUT [--raw]               decode with our decoder
//!   pdfe batch [--quick]                      stdin lines "IN OUT": compress each, verify, print sizes

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
            skip_next = matches!(a.as_str(), "--iter" | "--exact" | "--chain");
            continue;
        }
        pos.push(a);
    }
    let mut opt = if flag("--quick") { Options::quick() } else { Options::max() };
    let num = |name: &str| -> Option<usize> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok())
    };
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
            let r = engine::pdf::rewrite::optimize(&data, &pdf_options(&opt));
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
                let r = std::panic::catch_unwind(|| engine::pdf::rewrite::optimize(&data, &pdf_options(&opt)));
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
            let (z, st) = engine::pack::pack(&data);
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
                let r = std::panic::catch_unwind(|| engine::pack::pack(&data));
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
        _ => die("usage: pdfe deflate|inflate|batch|optimize|pdf-batch|repro|pack|unpack|pack-batch|jpeg|jpeg-batch ..."),
    }
}

fn pdf_options(f: &Options) -> engine::pdf::rewrite::Options {
    engine::pdf::rewrite::Options { flate: f.clone(), ..Default::default() }
}

fn describe(name: &str, in_len: usize, r: &engine::pdf::rewrite::Optimized, secs: f64) -> String {
    use engine::pdf::rewrite::Outcome;
    match &r.outcome {
        Outcome::Smaller(s) => format!(
            "{name}\tSMALLER\t{in_len}\t{}\t{secs:.2}\tobjects={} streams={} reencoded={} kept={} objstms={} dropped={}",
            r.data.len(),
            s.objects,
            s.streams,
            s.streams_reencoded,
            s.streams_kept,
            s.objstms,
            s.dropped_unreachable
        ),
        Outcome::Untouched(why) => format!("{name}\tUNTOUCHED\t{in_len}\t{}\t{secs:.2}\t{why}", r.data.len()),
    }
}
