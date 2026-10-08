//! For measuring: Type 1 font programs (decoded FontFile streams, one file each) opened as the gpress archive
//! opens them, the opened forms written out (for other compressors), and both forms coded by our model.
//!
//!   cargo run --release --example type1_fonts -- INDEX OUTDIR [font]
//!
//! INDEX: tab-separated lines "file pdf object length1 length2" (files next to INDEX). The fonts of one PDF
//! share a model, as they do in a pack. Prints, per PDF and in all: bytes, model output for the fonts as
//! they are and opened. With "font", the private part is coded as plain font data instead of charstrings.

use gpress::cm::model::{Geom, Kind, Model};
use gpress::cm::{Coder, Encoder};
use gpress::pack::type1;
use std::path::Path;

/// Encodes, and adds up the information (-log2 p) of every decision for the class of byte being coded:
/// 0 clear text, 1 private text, 2 charstrings, 3 the rest.
struct Costed {
    e: Encoder,
    class: usize,
    bits: [f64; 4],
    n: [usize; 4],
}

impl Coder for Costed {
    fn code(&mut self, bit: Option<u32>, p1: i32) -> u32 {
        let p = p1.clamp(1, 4095) as f64 / 4096.0;
        self.bits[self.class] -= if bit == Some(1) { p.log2() } else { (1.0 - p).log2() };
        self.e.code(bit, p1)
    }
    fn code16(&mut self, bit: Option<u32>, p16: u32) -> u32 {
        let p = p16.clamp(1, 65535) as f64 / 65536.0;
        self.bits[self.class] -= if bit == Some(1) { p.log2() } else { (1.0 - p).log2() };
        self.e.code16(bit, p16)
    }
}

fn code(m: &mut Model, e: &mut Costed, kind: Kind, data: &[u8]) {
    m.set_kind(kind, Geom::default(), 0);
    for &b in data {
        e.n[e.class] += 1;
        m.code_byte(e, Some(b));
    }
}

fn code_plain(m: &mut Model, e: &mut Encoder, kind: Kind, data: &[u8]) {
    m.set_kind(kind, Geom::default(), 0);
    for &b in data {
        m.code_byte(e, Some(b));
    }
}

/// The private part, byte by byte, with each byte's class (private text or charstring).
fn code_private(m: &mut Model, e: &mut Costed, kind: Kind, p: &[u8]) {
    let mut class = vec![1u8; p.len()];
    for (o, n) in type1::charstrings(p) {
        class[o..o + n].iter_mut().for_each(|c| *c = 2);
    }
    m.set_kind(kind, Geom::default(), 0);
    for (i, &b) in p.iter().enumerate() {
        e.class = class[i] as usize;
        e.n[e.class] += 1;
        m.code_byte(e, Some(b));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: type1_fonts INDEX OUTDIR");
        std::process::exit(2);
    }
    let dir = Path::new(&args[0]).parent().unwrap_or(Path::new("."));
    let out = Path::new(&args[1]);
    std::fs::create_dir_all(out).unwrap();
    let index = std::fs::read_to_string(&args[0]).unwrap();
    let private = if args.get(2).map(|s| s.as_str()) == Some("font") { Kind::Font } else { Kind::Charstring };
    let mut pdfs: Vec<(String, Vec<Vec<String>>)> = Vec::new();
    for line in index.lines() {
        let f: Vec<String> = line.split('\t').map(|s| s.to_string()).collect();
        if f.len() < 5 {
            continue;
        }
        match pdfs.last_mut() {
            Some((p, v)) if *p == f[1] => v.push(f),
            _ => pdfs.push((f[1].clone(), vec![f])),
        }
    }
    let (mut all_in, mut all_as_is, mut all_opened, mut all_fonts, mut all_open) = (0, 0, 0, 0, 0);
    let mut all_bits = [0f64; 4];
    let mut all_n = [0usize; 4];
    println!("pdf\tfonts\topened\tbytes\tas_is\topened_coded");
    for (pdf, fonts) in &pdfs {
        let (mut m1, mut e1) = (Model::new(18), Encoder::new());
        let (mut m2, mut e2) = (Model::new(18), Costed { e: Encoder::new(), class: 0, bits: [0.0; 4], n: [0; 4] });
        let (mut bytes, mut opened_n) = (0, 0);
        for f in fonts {
            let data = std::fs::read(dir.join(&f[0])).unwrap();
            let (l1, l2) = (f[3].parse().unwrap_or(0), f[4].parse().unwrap_or(0));
            bytes += data.len();
            code_plain(&mut m1, &mut e1, Kind::Font, &data);
            let opened = type1::find(&data, l1, l2).and_then(|l| {
                let t = type1::open(&data, &l)?;
                (type1::close(&t, &l)? == data).then_some((l, t))
            });
            match opened {
                Some((l, t)) => {
                    opened_n += 1;
                    let a = l.start;
                    let b = l.start + l.n;
                    e2.class = 0;
                    code(&mut m2, &mut e2, Kind::Font, &t[..a]);
                    code_private(&mut m2, &mut e2, private, &t[a..b]);
                    e2.class = 3;
                    code(&mut m2, &mut e2, Kind::Font, &t[b..]);
                    std::fs::write(out.join(format!("{}.t1", f[0])), &t).unwrap();
                }
                None => {
                    e2.class = 3;
                    code(&mut m2, &mut e2, Kind::Font, &data);
                    std::fs::write(out.join(format!("{}.t1", f[0])), &data).unwrap();
                }
            }
        }
        for k in 0..4 {
            all_bits[k] += e2.bits[k];
            all_n[k] += e2.n[k];
        }
        let (c1, c2) = (e1.finish().len(), e2.e.finish().len());
        println!("{pdf}\t{}\t{opened_n}\t{bytes}\t{c1}\t{c2}", fonts.len());
        all_in += bytes;
        all_as_is += c1;
        all_opened += c2;
        all_fonts += fonts.len();
        all_open += opened_n;
    }
    println!("ALL\t{all_fonts}\t{all_open}\t{all_in}\t{all_as_is}\t{all_opened}");
    let b = all_bits.map(|x| (x / 8.0) as u64);
    println!("CODED clear {} private_text {} charstrings {} rest {}", b[0], b[1], b[2], b[3]);
    println!("INPUT clear {} private_text {} charstrings {} rest {}", all_n[0], all_n[1], all_n[2], all_n[3]);
}
