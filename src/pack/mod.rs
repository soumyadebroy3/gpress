//! The peyaji format: any PDF, packed with our own PDF-aware model, restored byte for byte.
//!
//! The file is cut into PDF syntax and stream payloads. Flate payloads that zlib made are stored decoded
//! (with the few parameters needed to rebuild the exact compressed bytes); image rows lose their PNG
//! predictor bytes so the pixels themselves can be modelled. Everything is then coded by the
//! context-mixing model, which knows what kind of data each part is.
//!
//! Layout: magic (8) | version (1) | original length (8, LE) | SHA-256 of the original (32) | coded data.

use crate::cm::model::{Geom, Kind, Model};
use crate::cm::{Coder, Decoder, Encoder};
use crate::flate::{zlib, zlibexact};
use crate::pdf::object::{Dict, Obj};
use crate::pdf::parse::Lexer;

pub const MAGIC: &[u8; 8] = b"\x89PYJ\r\n\x1a\n";
const VERSION: u8 = 1;

mod sha256;
pub use sha256::sha256;

#[derive(Debug, Default, Clone)]
pub struct PackStats {
    pub syntax_bytes: usize,
    pub streams: usize,
    pub rebuilt: usize,
    pub rebuilt_bytes: usize,
    pub raw_stream_bytes: usize,
    pub decoded_bytes: usize,
    pub unpredicted_images: usize,
    pub fallback: bool,
}

#[derive(Debug)]
enum Seg {
    Syntax(usize, usize),
    Stream(usize, usize),
}

fn find(h: &[u8], n: &[u8], from: usize) -> Option<usize> {
    if from >= h.len() {
        return None;
    }
    h[from..].windows(n.len()).position(|w| w == n).map(|p| p + from)
}

/// Cuts the file into syntax and stream payloads. Works on any bytes (never fails).
fn segments(d: &[u8]) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut pos = 0;
    let mut syn_start = 0;
    while let Some(k) = find(d, b"stream", pos) {
        // Must be the keyword: preceded by ">>" (optionally with whitespace), followed by CRLF or LF.
        let mut b = k;
        while b > 0 && matches!(d[b - 1], b' ' | b'\r' | b'\n' | b'\t') {
            b -= 1;
        }
        let after = k + 6;
        let eol = if d.get(after) == Some(&b'\r') && d.get(after + 1) == Some(&b'\n') {
            2
        } else if d.get(after) == Some(&b'\n') {
            1
        } else {
            0
        };
        if b < 2 || &d[b - 2..b] != b">>" || eol == 0 || (k >= 3 && &d[k - 3..k] == b"end") {
            pos = k + 6;
            continue;
        }
        let start = after + eol;
        // Prefer /Length when it lands on endstream; otherwise search.
        let dict = dict_before(d, k);
        let mut end = None;
        if let Some(Obj::Int(l)) = dict.as_ref().and_then(|x| x.get(b"Length").cloned()) {
            if l >= 0 {
                let e = start + l as usize;
                if e <= d.len() {
                    let mut q = e;
                    while q < d.len() && matches!(d[q], b'\r' | b'\n' | b' ') {
                        q += 1;
                    }
                    if d[q..].starts_with(b"endstream") {
                        end = Some(e);
                    }
                }
            }
        }
        let end = match end {
            Some(e) => e,
            None => match find(d, b"endstream", start) {
                Some(e) => {
                    let mut e2 = e;
                    if e2 > start && d[e2 - 1] == b'\n' {
                        e2 -= 1;
                        if e2 > start && d[e2 - 1] == b'\r' {
                            e2 -= 1;
                        }
                    } else if e2 > start && d[e2 - 1] == b'\r' {
                        e2 -= 1;
                    }
                    e2
                }
                None => break,
            },
        };
        if syn_start < start {
            out.push(Seg::Syntax(syn_start, start));
        }
        out.push(Seg::Stream(start, end));
        syn_start = end;
        pos = end;
    }
    if syn_start < d.len() {
        out.push(Seg::Syntax(syn_start, d.len()));
    }
    out
}

/// The stream dictionary just before a "stream" keyword (best effort).
fn dict_before(d: &[u8], k: usize) -> Option<Dict> {
    // Look back for "obj" within 64 KiB and parse the object after it.
    let lo = k.saturating_sub(65536);
    let window = &d[lo..k];
    let o = window.windows(3).rposition(|w| w == b"obj")?;
    let mut lx = Lexer::new(d, lo + o + 3);
    match lx.parse_obj() {
        Ok(Obj::Dict(dd)) => Some(dd),
        _ => None,
    }
}

/// What the payload of a stream is, from its dictionary (the decoder sees the same dictionary).
struct Info {
    flate: bool,
    kind: Kind,
    geom: Geom,
    /// PNG predictor rows: (bytes per row without the filter byte, bytes per pixel).
    png: Option<(usize, usize)>,
}

fn int(d: &Dict, k: &[u8]) -> Option<i64> {
    d.get(k).and_then(|x| x.as_int())
}

fn info(dict: Option<&Dict>, decoded_sample: Option<&[u8]>) -> Info {
    let mut inf = Info { flate: false, kind: Kind::Raw, geom: Geom::default(), png: None };
    let d = match dict {
        Some(d) => d,
        None => return inf,
    };
    let filter = d.get(b"Filter");
    inf.flate = match filter {
        Some(Obj::Name(n)) => n == b"FlateDecode" || n == b"Fl",
        Some(Obj::Array(a)) if a.len() == 1 => matches!(&a[0], Obj::Name(n) if n == b"FlateDecode" || n == b"Fl"),
        _ => false,
    };
    let parms = match d.get(b"DecodeParms") {
        Some(Obj::Dict(p)) => Some(p.clone()),
        Some(Obj::Array(a)) if a.len() == 1 => match &a[0] {
            Obj::Dict(p) => Some(p.clone()),
            _ => None,
        },
        _ => None,
    };
    let subtype = d.name(b"Subtype").map(|x| x.to_vec());
    let typ = d.name(b"Type").map(|x| x.to_vec());
    if subtype.as_deref() == Some(b"Image") {
        let w = int(d, b"Width").unwrap_or(0).max(0) as usize;
        let bpc = int(d, b"BitsPerComponent").unwrap_or(8).max(1) as usize;
        let comps = match d.get(b"ColorSpace") {
            Some(Obj::Name(n)) if n == b"DeviceRGB" || n == b"CalRGB" => 3,
            Some(Obj::Name(n)) if n == b"DeviceCMYK" => 4,
            Some(Obj::Name(n)) if n == b"DeviceGray" || n == b"CalGray" => 1,
            Some(Obj::Array(a)) if a.first().and_then(|x| x.as_name()) == Some(b"Indexed") => 1,
            Some(Obj::Array(a)) if a.first().and_then(|x| x.as_name()) == Some(b"ICCBased") => 3,
            _ if d.get(b"ImageMask") == Some(&Obj::Bool(true)) => 1,
            _ => 1,
        };
        let row = (w * comps * bpc + 7) / 8;
        let bpp = ((comps * bpc) / 8).max(1);
        inf.kind = Kind::Image;
        inf.geom = Geom { stride: row, bpp };
        if let Some(p) = &parms {
            let pred = int(p, b"Predictor").unwrap_or(1);
            if pred >= 10 {
                let cols = int(p, b"Columns").unwrap_or(1).max(1) as usize;
                let colors = int(p, b"Colors").unwrap_or(1).max(1) as usize;
                let pbpc = int(p, b"BitsPerComponent").unwrap_or(8).max(1) as usize;
                let prow = (cols * colors * pbpc + 7) / 8;
                let pbpp = ((colors * pbpc + 7) / 8).max(1);
                inf.png = Some((prow, pbpp));
                inf.geom = Geom { stride: prow, bpp: pbpp };
            } else if pred > 1 {
                inf.geom = Geom { stride: 0, bpp: 1 };
            }
        }
        return inf;
    }
    if d.has(b"Length1") || matches!(subtype.as_deref(), Some(b"Type1C") | Some(b"CIDFontType0C") | Some(b"OpenType")) {
        inf.kind = Kind::Font;
    } else if typ.as_deref() == Some(b"ObjStm") {
        inf.kind = Kind::Syntax;
    } else if subtype.as_deref() == Some(b"Form") {
        inf.kind = Kind::Content;
    } else if typ.is_some() || subtype.is_some() {
        inf.kind = Kind::Data;
    } else {
        inf.kind = match decoded_sample {
            Some(s) if looks_like_content(s) => Kind::Content,
            _ => Kind::Data,
        };
    }
    inf
}

fn looks_like_content(s: &[u8]) -> bool {
    let s = &s[..s.len().min(4096)];
    if s.is_empty() {
        return false;
    }
    let printable = s.iter().filter(|&&c| (32..127).contains(&c) || c == b'\n' || c == b'\r' || c == b'\t').count();
    printable * 10 >= s.len() * 9
}

// ---------------------------------------------------------------- PNG predictor rows

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Filtered rows -> (filter types, pixels). None if the data isn't whole valid rows.
fn unfilter(data: &[u8], row: usize, bpp: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    if row == 0 || data.len() % (row + 1) != 0 {
        return None;
    }
    let rows = data.len() / (row + 1);
    let mut types = Vec::with_capacity(rows);
    let mut px = vec![0u8; rows * row];
    for r in 0..rows {
        let ft = data[r * (row + 1)];
        if ft > 4 {
            return None;
        }
        types.push(ft);
        let src = &data[r * (row + 1) + 1..(r + 1) * (row + 1)];
        for i in 0..row {
            let a = if i >= bpp { px[r * row + i - bpp] } else { 0 };
            let b = if r > 0 { px[(r - 1) * row + i] } else { 0 };
            let c = if r > 0 && i >= bpp { px[(r - 1) * row + i - bpp] } else { 0 };
            px[r * row + i] = match ft {
                0 => src[i],
                1 => src[i].wrapping_add(a),
                2 => src[i].wrapping_add(b),
                3 => src[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                _ => src[i].wrapping_add(paeth(a, b, c)),
            };
        }
    }
    Some((types, px))
}

fn refilter(types: &[u8], px: &[u8], row: usize, bpp: usize) -> Vec<u8> {
    let rows = types.len();
    let mut out = Vec::with_capacity(rows * (row + 1));
    for r in 0..rows {
        let ft = types[r];
        out.push(ft);
        for i in 0..row {
            let x = px[r * row + i];
            let a = if i >= bpp { px[r * row + i - bpp] } else { 0 };
            let b = if r > 0 { px[(r - 1) * row + i] } else { 0 };
            let c = if r > 0 && i >= bpp { px[(r - 1) * row + i - bpp] } else { 0 };
            out.push(match ft {
                0 => x,
                1 => x.wrapping_sub(a),
                2 => x.wrapping_sub(b),
                3 => x.wrapping_sub(((a as u16 + b as u16) / 2) as u8),
                _ => x.wrapping_sub(paeth(a, b, c)),
            });
        }
    }
    out
}

// ---------------------------------------------------------------- coding helpers

struct Io<'m, C: Coder> {
    m: &'m mut Model,
    c: C,
}

impl<'m, C: Coder> Io<'m, C> {
    fn byte(&mut self, b: Option<u8>) -> u8 {
        self.m.code_byte(&mut self.c, b)
    }
    fn varint(&mut self, v: Option<u64>) -> u64 {
        let mut out = 0u64;
        let mut shift = 0;
        let mut rest = v;
        loop {
            let b = rest.map(|x| {
                let low = (x & 0x7f) as u8;
                if x >> 7 != 0 {
                    low | 0x80
                } else {
                    low
                }
            });
            let got = self.byte(b);
            out |= ((got & 0x7f) as u64) << shift;
            rest = rest.map(|x| x >> 7);
            if got & 0x80 == 0 || shift > 56 {
                return out;
            }
            shift += 7;
        }
    }
    fn bytes(&mut self, data: Option<&[u8]>, n: usize, out: &mut Vec<u8>) {
        for i in 0..n {
            let b = self.byte(data.map(|d| d[i]));
            out.push(b);
        }
    }
}

const T_SYNTAX: u8 = 0;
const T_RAW: u8 = 1;
const T_REBUILT: u8 = 2;
const T_END: u8 = 255;

fn table_bits(n: usize) -> u32 {
    let mut b = 12;
    while b < 18 && (1usize << (b + 4)) < n {
        b += 1;
    }
    b
}

enum Plan {
    Syntax(usize, usize),
    Raw(usize, usize),
    Rebuilt { params: zlibexact::Params, data: Vec<u8>, tail: Vec<u8> },
}

fn plan(d: &[u8]) -> Vec<Plan> {
    segments(d)
        .into_iter()
        .map(|s| match s {
            Seg::Syntax(a, b) => Plan::Syntax(a, b),
            Seg::Stream(a, b) => match rebuild_params(d, a, &d[a..b]) {
                Some((params, data, tail)) => Plan::Rebuilt { params, data, tail },
                None => Plan::Raw(a, b),
            },
        })
        .collect()
}

/// One pass of the format: encoding `d` (Some) or rebuilding it (None). The same code runs both ways so the
/// model sees exactly the same sequence of events.
fn run<C: Coder>(c: C, d: Option<&[u8]>, total_len: usize, stats: &mut PackStats) -> (Vec<u8>, C) {
    let mut model = Model::new(table_bits(total_len));
    let mut io = Io { m: &mut model, c };
    let mut out: Vec<u8> = Vec::with_capacity(total_len);
    let plans = d.map(plan);
    let mut si = 0;
    loop {
        let cur = plans.as_ref().map(|p| p.get(si));
        let tag_in = cur.map(|p| match p {
            None => T_END,
            Some(Plan::Syntax(..)) => T_SYNTAX,
            Some(Plan::Raw(..)) => T_RAW,
            Some(Plan::Rebuilt { .. }) => T_REBUILT,
        });
        io.m.set_kind(Kind::Meta, Geom::default(), out.len() as u64);
        let tag = io.byte(tag_in);
        match tag {
            T_END => break,
            T_SYNTAX | T_RAW => {
                let src: Option<&[u8]> = match cur.flatten() {
                    Some(Plan::Syntax(a, b)) | Some(Plan::Raw(a, b)) => Some(&d.unwrap()[*a..*b]),
                    _ => None,
                };
                let n = io.varint(src.map(|x| x.len() as u64)) as usize;
                if tag == T_SYNTAX {
                    io.m.set_kind(Kind::Syntax, Geom::default(), out.len() as u64);
                    stats.syntax_bytes += n;
                } else {
                    io.m.set_kind(Kind::Raw, Geom::default(), out.len() as u64);
                    stats.streams += 1;
                    stats.raw_stream_bytes += n;
                }
                io.bytes(src, n, &mut out);
            }
            T_REBUILT => {
                let (p_in, data_in, tail_in) = match cur.flatten() {
                    Some(Plan::Rebuilt { params, data, tail }) => (Some(*params), Some(data.as_slice()), Some(tail.as_slice())),
                    _ => (None, None, None),
                };
                let level = io.byte(p_in.map(|p| p.level));
                let wb = io.byte(p_in.map(|p| p.window_bits));
                let mem = io.byte(p_in.map(|p| p.mem_level));
                let filt = io.byte(p_in.map(|p| p.filtered as u8));
                let params = zlibexact::Params { level, window_bits: wb, mem_level: mem, filtered: filt != 0 };
                let tail_n = io.varint(tail_in.map(|t| t.len() as u64)) as usize;
                let mut tail = Vec::with_capacity(tail_n);
                io.bytes(tail_in, tail_n, &mut tail);
                let dlen = io.varint(data_in.map(|x| x.len() as u64)) as usize;
                let dict = dict_before(&out, out.len().saturating_sub(1));
                let looks = io.byte(data_in.map(|x| looks_like_content(x) as u8));
                let mut inf = info(dict.as_ref(), Some(if looks == 1 { b"q" as &[u8] } else { b"\x00" as &[u8] }));
                let mut decoded: Vec<u8> = Vec::with_capacity(dlen);
                let mut used_png = false;
                if let Some((row, bpp)) = inf.png {
                    let split = data_in.map(|x| unfilter(x, row, bpp));
                    let ok = io.byte(split.as_ref().map(|s| s.is_some() as u8));
                    if ok == 1 {
                        let (types_in, px_in) = match &split {
                            Some(Some((t, p))) => (Some(t.as_slice()), Some(p.as_slice())),
                            _ => (None, None),
                        };
                        let rows = dlen / (row + 1);
                        io.m.set_kind(Kind::Data, Geom::default(), 0);
                        let mut types = Vec::with_capacity(rows);
                        io.bytes(types_in, rows, &mut types);
                        io.m.set_kind(Kind::Image, Geom { stride: row, bpp }, 0);
                        let mut px = Vec::with_capacity(rows * row);
                        io.bytes(px_in, rows * row, &mut px);
                        decoded = refilter(&types, &px, row, bpp);
                        used_png = true;
                        stats.unpredicted_images += 1;
                    }
                }
                if !used_png {
                    if inf.png.is_some() {
                        inf.geom = Geom { stride: inf.geom.stride + 1, bpp: 1 };
                    }
                    io.m.set_kind(inf.kind, inf.geom, 0);
                    io.bytes(data_in, dlen, &mut decoded);
                }
                let z = zlibexact::zlib_stream(&decoded, params);
                out.extend_from_slice(&z);
                out.extend_from_slice(&tail);
                stats.streams += 1;
                stats.rebuilt += 1;
                stats.rebuilt_bytes += z.len();
                stats.decoded_bytes += dlen;
            }
            _ => break,
        }
        si += 1;
    }
    let Io { c, .. } = io;
    (out, c)
}

/// For a Flate payload that zlib made: (parameters, decoded data, bytes after the zlib stream).
fn rebuild_params(d: &[u8], start: usize, raw: &[u8]) -> Option<(zlibexact::Params, Vec<u8>, Vec<u8>)> {
    let dict = dict_before(d, start.saturating_sub(1));
    let inf = info(dict.as_ref(), None);
    if !inf.flate {
        return None;
    }
    let u = zlib::unwrap(raw, 1 << 28).ok()?;
    let used = raw.len() - u.trailing;
    let p = zlibexact::find_params(&u.data, &raw[..used])?;
    Some((p, u.data, raw[used..].to_vec()))
}

pub fn pack(input: &[u8]) -> (Vec<u8>, PackStats) {
    let mut stats = PackStats::default();
    let (copy, enc) = run(Encoder::new(), Some(input), input.len(), &mut stats);
    debug_assert_eq!(copy, input);
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(input.len() as u64).to_le_bytes());
    out.extend_from_slice(&sha256(input));
    out.extend_from_slice(&enc.finish());
    // Never hand over something we can't restore exactly.
    match unpack(&out) {
        Ok(back) if back == input => (out, stats),
        _ => {
            stats.fallback = true;
            (fallback(input), stats)
        }
    }
}

/// Restores the original file; checks its length and SHA-256.
pub fn unpack(a: &[u8]) -> Result<Vec<u8>, String> {
    if a.len() < 49 || &a[..8] != MAGIC {
        return Err("not a peyaji file".into());
    }
    let ver = a[8];
    let len = u64::from_le_bytes(a[9..17].try_into().unwrap()) as usize;
    let want: [u8; 32] = a[17..49].try_into().unwrap();
    let body = &a[49..];
    let out = match ver {
        VERSION => {
            let mut stats = PackStats::default();
            let (out, _) = run(Decoder::new(body), None, len, &mut stats);
            out
        }
        0 => fallback_decode(body, len),
        _ => return Err(format!("unknown version {ver}")),
    };
    if out.len() != len || sha256(&out) != want {
        return Err("restored file does not match its checksum".into());
    }
    Ok(out)
}

/// Version 0: the whole file as one raw part (always exact).
fn fallback(input: &[u8]) -> Vec<u8> {
    let mut model = Model::new(table_bits(input.len()));
    model.set_kind(Kind::Raw, Geom::default(), 0);
    let mut e = Encoder::new();
    for &b in input {
        model.code_byte(&mut e, Some(b));
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(0);
    out.extend_from_slice(&(input.len() as u64).to_le_bytes());
    out.extend_from_slice(&sha256(input));
    out.extend_from_slice(&e.finish());
    out
}

fn fallback_decode(body: &[u8], len: usize) -> Vec<u8> {
    let mut model = Model::new(table_bits(len));
    model.set_kind(Kind::Raw, Geom::default(), 0);
    let mut dcd = Decoder::new(body);
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        out.push(model.code_byte(&mut dcd, None));
    }
    out
}
