//! The gpress archive: any PDF, packed with our own PDF-aware model, restored byte for byte.
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

pub const MAGIC: &[u8; 4] = b"\x89GPZ";
/// Current archive format version. Versions 0 (raw model) and 2 (stored) are also written.
pub const VERSION: u8 = 4;
const HASH_LEN: usize = 16;

/// A typical PDF fragment both sides feed to the model first, so small files don't pay for a cold start.
const PRIME_SYNTAX: &[u8] = include_bytes!("prime.txt");
const PRIME_CONTENT: &[u8] = include_bytes!("prime_content.txt");

struct Null;
impl Coder for Null {
    fn code(&mut self, bit: Option<u32>, _p: i32) -> u32 {
        bit.unwrap_or(0)
    }
}

fn primed(bits: u32) -> Model {
    let mut m = Model::new(bits);
    // The priming text runs twice to train the full model, so the long-repeat shortcut stays off here.
    m.bypass_on = false;
    let mut n = Null;
    for _ in 0..2 {
        m.set_kind(Kind::Syntax, Geom::default(), 0);
        for &b in PRIME_SYNTAX {
            m.code_byte(&mut n, Some(b));
        }
        m.set_kind(Kind::Content, Geom::default(), 0);
        for &b in PRIME_CONTENT {
            m.code_byte(&mut n, Some(b));
        }
    }
    m.forget_document();
    m.bypass_on = true;
    m
}

/// Data that is already compressed (high byte entropy) may be coded plainly: much faster, but about 1%
/// larger than the full model even on such data (measured), so only when speed is asked for. Only the encoder
/// decides; the decision is stored in the file.
fn looks_compressed(x: &[u8], allowed: bool) -> bool {
    if !allowed || x.len() < 1024 {
        return false;
    }
    let mut h = [0u64; 256];
    for &b in x {
        h[b as usize] += 1;
    }
    let n = x.len() as f64;
    let bits: f64 = h
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum();
    bits >= 7.9
}

fn header(version: u8, input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(MAGIC);
    out.push(version);
    let mut n = input.len() as u64;
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
    out.extend_from_slice(&sha256(input)[..HASH_LEN]);
    out
}

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
    pub preflated: usize,
    pub leptons: usize,
    pub fallback: bool,
    pub stored: bool,
    /// Restore only: the file is damaged (it can't be restored; nothing wrong is ever returned).
    pub damaged: bool,
    /// Bytes coded plainly (already compressed) and bytes coded by one long-repeat decision.
    pub flat_bytes: usize,
    pub bypassed: u64,
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
    kind: Kind,
    geom: Geom,
    /// Image without a predictor: (width, height, bits per component, components guessed from the colour
    /// space), so the row length can be confirmed from the data length.
    img: Option<(usize, usize, usize, usize)>,
    /// PNG predictor rows: (bytes per row without the filter byte, bytes per pixel).
    png: Option<(usize, usize)>,
    /// TIFF predictor 2 on 8-bit samples: (bytes per row, bytes per pixel).
    tiff: Option<(usize, usize)>,
}

fn int(d: &Dict, k: &[u8]) -> Option<i64> {
    d.get(k).and_then(|x| x.as_int())
}

fn info(dict: Option<&Dict>, decoded_sample: Option<&[u8]>) -> Info {
    let mut inf = Info { kind: Kind::Raw, geom: Geom::default(), img: None, png: None, tiff: None };
    let d = match dict {
        Some(d) => d,
        None => return inf,
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
    // Behind Flate there may be another codec (a JPEG, say): then the decoded data is that codec's bytes, not
    // samples or text.
    if matches!(d.get(b"Filter"), Some(Obj::Array(a)) if a.len() > 1) {
        return inf;
    }
    if subtype.as_deref() == Some(b"Image") {
        let w = int(d, b"Width").unwrap_or(0).clamp(0, 1 << 24) as usize;
        let h = int(d, b"Height").unwrap_or(0).clamp(0, 1 << 24) as usize;
        let mask = d.get(b"ImageMask") == Some(&Obj::Bool(true));
        let bpc = if mask { 1 } else { int(d, b"BitsPerComponent").unwrap_or(8).clamp(1, 16) as usize };
        let first = |a: &Vec<Obj>| a.first().and_then(|x| x.as_name()).map(|x| x.to_vec());
        let (comps, indexed) = match d.get(b"ColorSpace") {
            Some(Obj::Name(n)) if n == b"DeviceRGB" || n == b"CalRGB" || n == b"RGB" => (3, false),
            Some(Obj::Name(n)) if n == b"DeviceCMYK" || n == b"CMYK" => (4, false),
            Some(Obj::Name(n)) if n == b"DeviceGray" || n == b"CalGray" || n == b"G" => (1, false),
            Some(Obj::Array(a)) => match first(a).as_deref() {
                Some(b"Indexed") | Some(b"I") => (1, true),
                Some(b"DeviceN") => (a.get(1).and_then(|x| x.as_array()).map_or(1, |v| v.len().max(1)), false),
                Some(b"Separation") | Some(b"CalGray") => (1, false),
                Some(b"CalRGB") | Some(b"Lab") => (3, false),
                // The real count is in the ICC stream (/N); the data length confirms it below.
                _ => (3, false),
            },
            _ => (1, false),
        };
        let row = (w * comps * bpc).div_ceil(8);
        let bpp = ((comps * bpc) / 8).max(1);
        inf.kind = Kind::Image;
        // Palette indices and packed pixels are symbols: modelled exactly, not as differences.
        let exact = indexed || bpc < 8;
        inf.geom = Geom { stride: row, bpp, exact };
        inf.img = Some((w, h, bpc, comps));
        if let Some(p) = &parms {
            let pred = int(p, b"Predictor").unwrap_or(1);
            if pred >= 10 {
                let cols = int(p, b"Columns").unwrap_or(1).max(1) as usize;
                let colors = int(p, b"Colors").unwrap_or(1).max(1) as usize;
                let pbpc = int(p, b"BitsPerComponent").unwrap_or(8).max(1) as usize;
                let prow = (cols * colors * pbpc).div_ceil(8);
                let pbpp = (colors * pbpc).div_ceil(8).max(1);
                inf.png = Some((prow, pbpp));
                inf.geom = Geom { stride: prow, bpp: pbpp, exact: exact || pbpc < 8 };
                inf.img = None;
            } else if pred == 2 {
                let cols = int(p, b"Columns").unwrap_or(1).max(1) as usize;
                let colors = int(p, b"Colors").unwrap_or(1).max(1) as usize;
                let pbpc = int(p, b"BitsPerComponent").unwrap_or(8).max(1) as usize;
                if pbpc == 8 {
                    inf.tiff = Some((cols * colors, colors));
                    inf.geom = Geom { stride: cols * colors, bpp: colors, exact: false };
                } else {
                    inf.geom = Geom { stride: 0, bpp: 1, exact: false };
                }
                inf.img = None;
            } else if pred > 1 {
                inf.geom = Geom { stride: 0, bpp: 1, exact: false };
                inf.img = None;
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
    if row == 0 || !data.len().is_multiple_of(row + 1) {
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
    /// Set when decoding meets something only a damaged file can contain; decoding stops there.
    damaged: bool,
}

/// Largest single part a file may declare (decoded stream, corrections, Lepton data): 2 GiB.
const MAX_PART: usize = 1 << 31;
/// Largest file the format restores: 16 GiB.
const MAX_FILE: usize = 1 << 34;
/// Memory reserved up front is capped; vectors grow with what is actually decoded.
fn reserve(n: usize) -> usize {
    n.min(1 << 22)
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
            if i & 0xfff == 0 && self.c.exhausted() {
                self.damaged = true;
                return;
            }
            let b = self.byte(data.map(|d| d[i]));
            out.push(b);
        }
    }
    /// A length that must be at most `max`; anything larger can only come from a damaged file.
    fn length(&mut self, v: Option<u64>, max: usize) -> usize {
        let n = self.varint(v);
        if n > max as u64 {
            self.damaged = true;
            return 0;
        }
        n as usize
    }
}

/// Plain coding for compressed-looking streams: off at the current (maximum) effort.
const PLAIN_WHEN_COMPRESSED: bool = false;

const T_SYNTAX: u8 = 0;
const T_RAW: u8 = 1;
const T_REBUILT: u8 = 2;
const T_PREFLATED: u8 = 3;
const T_LEPTON: u8 = 4;
const T_END: u8 = 255;

fn table_bits(n: usize) -> u32 {
    // 16-byte buckets: up to 2^20 per context (16 MiB each) for large files.
    let mut b = 14;
    while b < 20 && (1usize << (b + 3)) < n {
        b += 1;
    }
    b
}

#[cfg_attr(not(feature = "existing"), allow(dead_code))]
enum Plan {
    Syntax(usize, usize),
    Raw(usize, usize),
    /// `lep`: the decoded data is a JPEG, recompressed with Lepton (when that beats the Flate bytes).
    Rebuilt {
        params: zlibexact::Params,
        data: Vec<u8>,
        tail: Vec<u8>,
        lep: Option<Vec<u8>>,
    },
    /// Rebuilt with the corrections an existing library (preflate) records for non-zlib encoders.
    Preflated {
        header: [u8; 2],
        corrections: Vec<u8>,
        data: Vec<u8>,
        adler_ok: bool,
        rest: Vec<u8>,
        lep: Option<Vec<u8>>,
    },
    /// A JPEG recompressed with an existing library (Lepton), restored bit for bit.
    Lepton {
        data: Vec<u8>,
    },
}

fn plan(d: &[u8]) -> Vec<Plan> {
    segments(d)
        .into_iter()
        .map(|s| match s {
            Seg::Syntax(a, b) => Plan::Syntax(a, b),
            Seg::Stream(a, b) => match rebuild_params(d, a, &d[a..b]) {
                Some((params, data, tail)) => {
                    let lep = nested_lepton(&data, b - a);
                    Plan::Rebuilt { params, data, tail, lep }
                }
                None => match preflated(d, a, &d[a..b]) {
                    Some(p) => p,
                    None => match lepton(d, a, &d[a..b]) {
                        Some(p) => p,
                        None => Plan::Raw(a, b),
                    },
                },
            },
        })
        .collect()
}

/// For measuring only: the file with every stream we can rebuild replaced by its decoded bytes and the JPEGs
/// Lepton takes left out, plus the size of what a restore needs on top (rebuild settings, preflate
/// corrections, Lepton data). A general compressor on the first, plus the second, is the fair "decompress
/// first, then compress" baseline for the gpress archive: the same preparation, without our model.
pub fn expand(d: &[u8]) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(d.len() * 2);
    let mut extra = 0;
    for p in plan(d) {
        match p {
            Plan::Syntax(a, b) | Plan::Raw(a, b) => out.extend_from_slice(&d[a..b]),
            Plan::Rebuilt { data, tail, lep, .. } => {
                match lep {
                    Some(l) => extra += l.len(),
                    None => out.extend_from_slice(&data),
                }
                out.extend_from_slice(&tail);
                extra += 2;
            }
            Plan::Preflated { corrections, data, rest, lep, .. } => {
                match lep {
                    Some(l) => extra += l.len(),
                    None => out.extend_from_slice(&data),
                }
                out.extend_from_slice(&rest);
                extra += corrections.len();
            }
            Plan::Lepton { data } => extra += data.len(),
        }
    }
    (out, extra)
}

#[cfg(feature = "existing")]
fn preflated(_d: &[u8], _start: usize, raw: &[u8]) -> Option<Plan> {
    if !zlib_header(raw) {
        return None;
    }
    let (cmf, flg) = (raw[0], raw[1]);
    let cfg = preflate_rs::PreflateConfig::default();
    let r = preflate_rs::preflate_whole_deflate_stream(&raw[2..], &cfg);
    if std::env::var_os("PDFE_DEBUG").is_some() {
        match &r {
            Ok((res, _)) => eprintln!("preflate: raw {} corrections {}", raw.len(), res.corrections.len()),
            Err(e) => eprintln!("preflate: raw {} error {e:?}", raw.len()),
        }
    }
    let (res, plain) = r.ok()?;
    let used = 2 + res.compressed_size;
    let data = plain.text().to_vec();
    // Worth it only if the corrections are small next to what decoding gains.
    if res.corrections.len() > raw.len() / 3 {
        return None;
    }
    let back = preflate_rs::recreate_whole_deflate_stream(&data, &res.corrections).ok()?;
    if back != raw[2..used] {
        return None;
    }
    let after = &raw[used..];
    let adler_ok = after.len() >= 4 && after[..4] == zlib::adler32(&data).to_be_bytes();
    let rest = if adler_ok { after[4..].to_vec() } else { after.to_vec() };
    let lep = nested_lepton(&data, raw.len());
    Some(Plan::Preflated { header: [cmf, flg], corrections: res.corrections, data, adler_ok, rest, lep })
}

#[cfg(not(feature = "existing"))]
fn preflated(_d: &[u8], _start: usize, _raw: &[u8]) -> Option<Plan> {
    None
}

#[cfg(feature = "existing")]
fn lepton_features() -> lepton_jpeg::EnabledFeatures {
    let mut f = lepton_jpeg::EnabledFeatures::compat_lepton_vector_write();
    f.max_processor_threads = 1;
    f
}

#[cfg(feature = "existing")]
fn lepton(_d: &[u8], _start: usize, raw: &[u8]) -> Option<Plan> {
    lepton_bytes(raw).map(|data| Plan::Lepton { data })
}

/// Decoded Flate data that is a JPEG: Lepton, when its output is at least 10% smaller than the Flate bytes
/// (a JPEG that Flate shrinks a lot is better left to our model).
fn nested_lepton(data: &[u8], flate_len: usize) -> Option<Vec<u8>> {
    lepton_bytes(data).filter(|l| l.len() * 10 < flate_len * 9)
}

/// A JPEG by its bytes (start of image, then a marker) recompressed with Lepton; kept only if smaller and
/// restored exactly.
#[cfg(feature = "existing")]
fn lepton_bytes(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 1024 || !raw.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return None;
    }
    let pool = lepton_jpeg::SingleThreadPool {};
    let mut out = std::io::Cursor::new(Vec::new());
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lepton_jpeg::encode_lepton(&mut std::io::Cursor::new(raw), &mut out, &lepton_features(), &pool).ok()
    }));
    r.ok()??;
    let data = out.into_inner();
    if data.len() >= raw.len() || unlepton(&data)? != raw {
        return None;
    }
    Some(data)
}

#[cfg(feature = "existing")]
fn unlepton(data: &[u8]) -> Option<Vec<u8>> {
    let pool = lepton_jpeg::SingleThreadPool {};
    let mut out = Vec::new();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lepton_jpeg::decode_lepton(&mut std::io::Cursor::new(data), &mut out, &lepton_features(), &pool).ok()
    }));
    r.ok()??;
    Some(out)
}

#[cfg(not(feature = "existing"))]
fn lepton(_d: &[u8], _start: usize, _raw: &[u8]) -> Option<Plan> {
    None
}

#[cfg(not(feature = "existing"))]
fn lepton_bytes(_raw: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(feature = "existing"))]
fn unlepton(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(feature = "existing")]
fn recreate(data: &[u8], corrections: &[u8]) -> Option<Vec<u8>> {
    std::panic::catch_unwind(|| preflate_rs::recreate_whole_deflate_stream(data, corrections).ok()).ok()?
}

#[cfg(not(feature = "existing"))]
fn recreate(_data: &[u8], _corrections: &[u8]) -> Option<Vec<u8>> {
    None
}

/// One pass of the format: encoding `d` (Some) or rebuilding it (None). The same code runs both ways so the
/// model sees exactly the same sequence of events.
fn run<C: Coder>(c: C, d: Option<&[u8]>, total_len: usize, stats: &mut PackStats) -> (Vec<u8>, C) {
    let mut model = primed(table_bits(total_len));
    let mut io = Io { m: &mut model, c, damaged: false };
    let mut out: Vec<u8> = Vec::with_capacity(reserve(total_len));
    let plans = d.map(plan);
    let mut si = 0;
    loop {
        let cur = plans.as_ref().map(|p| p.get(si));
        let tag_in = cur.map(|p| match p {
            None => T_END,
            Some(Plan::Syntax(..)) => T_SYNTAX,
            Some(Plan::Raw(..)) => T_RAW,
            Some(Plan::Rebuilt { .. }) => T_REBUILT,
            Some(Plan::Preflated { .. }) => T_PREFLATED,
            Some(Plan::Lepton { .. }) => T_LEPTON,
        });
        if io.damaged || out.len() > total_len || io.c.exhausted() {
            io.damaged = true;
            break;
        }
        let left = total_len - out.len();
        io.m.set_kind(Kind::Meta, Geom::default(), out.len() as u64);
        let tag = io.byte(tag_in);
        match tag {
            T_END => break,
            T_SYNTAX | T_RAW => {
                let src: Option<&[u8]> = match cur.flatten() {
                    Some(Plan::Syntax(a, b)) | Some(Plan::Raw(a, b)) => Some(&d.unwrap()[*a..*b]),
                    _ => None,
                };
                let n = io.length(src.map(|x| x.len() as u64), left);
                let mut flat = false;
                if tag == T_SYNTAX {
                    io.m.set_kind(Kind::Syntax, Geom::default(), out.len() as u64);
                    stats.syntax_bytes += n;
                } else {
                    flat = io.byte(src.map(|x| looks_compressed(x, PLAIN_WHEN_COMPRESSED) as u8)) == 1;
                    io.m.set_kind(Kind::Raw, Geom::default(), out.len() as u64);
                    stats.streams += 1;
                    stats.raw_stream_bytes += n;
                    if flat {
                        stats.flat_bytes += n;
                    }
                }
                io.m.set_flat(flat);
                io.bytes(src, n, &mut out);
                io.m.set_flat(false);
            }
            T_REBUILT => {
                let (p_in, data_in, tail_in, lep_in) = match cur.flatten() {
                    Some(Plan::Rebuilt { params, data, tail, lep }) => {
                        (Some(*params), Some(data.as_slice()), Some(tail.as_slice()), Some(lep.as_deref()))
                    }
                    _ => (None, None, None, None),
                };
                let level = io.byte(p_in.map(|p| p.level));
                let wb = io.byte(p_in.map(|p| p.window_bits));
                let mem = io.byte(p_in.map(|p| p.mem_level));
                let filt = io.byte(p_in.map(|p| p.filtered as u8));
                let params = zlibexact::Params { level, window_bits: wb, mem_level: mem, filtered: filt != 0 };
                if !(1..=9).contains(&level) || !(8..=15).contains(&wb) || !(1..=9).contains(&mem) || filt > 1 {
                    io.damaged = true;
                    break;
                }
                let tail_n = io.length(tail_in.map(|t| t.len() as u64), left);
                let mut tail = Vec::with_capacity(reserve(tail_n));
                io.bytes(tail_in, tail_n, &mut tail);
                let decoded = payload(&mut io, data_in, lep_in, &out, stats);
                if io.damaged {
                    break;
                }
                let z = zlibexact::zlib_stream(&decoded, params);
                out.extend_from_slice(&z);
                out.extend_from_slice(&tail);
                stats.streams += 1;
                stats.rebuilt += 1;
                stats.rebuilt_bytes += z.len();
            }
            T_PREFLATED => {
                let cur_p = match cur.flatten() {
                    Some(Plan::Preflated { header, corrections, data, adler_ok, rest, lep }) => Some((
                        *header,
                        corrections.as_slice(),
                        data.as_slice(),
                        *adler_ok,
                        rest.as_slice(),
                        lep.as_deref(),
                    )),
                    _ => None,
                };
                let h0 = io.byte(cur_p.map(|p| p.0[0]));
                let h1 = io.byte(cur_p.map(|p| p.0[1]));
                let clen = io.length(cur_p.map(|p| p.1.len() as u64), MAX_PART);
                io.m.set_kind(Kind::Data, Geom::default(), 0);
                let mut corrections = Vec::with_capacity(reserve(clen));
                io.bytes(cur_p.map(|p| p.1), clen, &mut corrections);
                io.m.set_kind(Kind::Meta, Geom::default(), 0);
                let adler_ok = io.byte(cur_p.map(|p| p.3 as u8)) == 1;
                let rlen = io.length(cur_p.map(|p| p.4.len() as u64), left);
                let mut rest = Vec::with_capacity(reserve(rlen));
                io.bytes(cur_p.map(|p| p.4), rlen, &mut rest);
                let decoded = payload(&mut io, cur_p.map(|p| p.2), cur_p.map(|p| p.5), &out, stats);
                if io.damaged {
                    break;
                }
                let z = match recreate(&decoded, &corrections) {
                    Some(z) => z,
                    None => {
                        io.damaged = true;
                        break;
                    }
                };
                out.push(h0);
                out.push(h1);
                out.extend_from_slice(&z);
                if adler_ok {
                    out.extend_from_slice(&zlib::adler32(&decoded).to_be_bytes());
                }
                out.extend_from_slice(&rest);
                stats.streams += 1;
                stats.preflated += 1;
            }
            T_LEPTON => {
                let src = match cur.flatten() {
                    Some(Plan::Lepton { data }) => Some(data.as_slice()),
                    _ => None,
                };
                let n = io.length(src.map(|x| x.len() as u64), MAX_PART);
                let mut lep = Vec::with_capacity(reserve(n));
                lepton_part(&mut io, src, n, &mut lep, stats);
                if io.damaged {
                    break;
                }
                match unlepton(&lep) {
                    Some(j) => out.extend_from_slice(&j),
                    None => {
                        io.damaged = true;
                        break;
                    }
                }
                stats.streams += 1;
                stats.leptons += 1;
            }
            _ => {
                io.damaged = true;
                break;
            }
        }
        si += 1;
    }
    if io.damaged {
        stats.damaged = true;
    }
    stats.bypassed = io.m.bypassed;
    let Io { c, .. } = io;
    (out, c)
}

/// Lepton's output: usually entropy-coded already, so coded plainly (repeats are still found); our model
/// when it isn't (an odd JPEG). The encoder decides; the decision is stored.
fn lepton_part<C: Coder>(io: &mut Io<C>, src: Option<&[u8]>, n: usize, lep: &mut Vec<u8>, stats: &mut PackStats) {
    let flat = io.byte(src.map(|x| looks_compressed(x, true) as u8)) == 1;
    io.m.set_kind(Kind::Raw, Geom::default(), 0);
    io.m.set_flat(flat);
    io.bytes(src, n, lep);
    io.m.set_flat(false);
    if flat {
        stats.flat_bytes += n;
    }
}

/// Codes a decoded stream payload (shared by the rebuilt kinds): its length, then the data with the model
/// set for what it is; image rows lose their PNG predictor bytes on the way.
fn payload<C: Coder>(
    io: &mut Io<C>,
    data_in: Option<&[u8]>,
    lep_in: Option<Option<&[u8]>>,
    out: &[u8],
    stats: &mut PackStats,
) -> Vec<u8> {
    let dlen = io.length(data_in.map(|x| x.len() as u64), MAX_PART);
    // A JPEG behind Flate (as normal-PDF output writes them): Lepton for the JPEG inside.
    if io.byte(lep_in.map(|l| l.is_some() as u8)) == 1 {
        let src = lep_in.flatten();
        let n = io.length(src.map(|x| x.len() as u64), MAX_PART);
        let mut lep = Vec::with_capacity(reserve(n));
        lepton_part(io, src, n, &mut lep, stats);
        stats.leptons += 1;
        stats.decoded_bytes += dlen;
        return match (io.damaged, unlepton(&lep)) {
            (false, Some(j)) if j.len() == dlen => j,
            _ => {
                io.damaged = true;
                Vec::new()
            }
        };
    }
    let dict = dict_before(out, out.len().saturating_sub(1));
    let looks = io.byte(data_in.map(|x| looks_like_content(x) as u8));
    let mut inf = info(dict.as_ref(), Some(if looks == 1 { b"q" as &[u8] } else { b"\x00" as &[u8] }));
    let mut decoded: Vec<u8> = Vec::with_capacity(reserve(dlen));
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
            let mut types = Vec::with_capacity(reserve(rows));
            io.bytes(types_in, rows, &mut types);
            io.m.set_kind(Kind::Image, Geom { stride: row, bpp, exact: inf.geom.exact }, 0);
            let mut px = Vec::with_capacity(reserve(rows * row));
            io.bytes(px_in, rows * row, &mut px);
            if io.damaged {
                return Vec::new();
            }
            decoded = refilter(&types, &px, row, bpp);
            used_png = true;
            stats.unpredicted_images += 1;
        }
    }
    if let (false, Some((row, bpp))) = (used_png, inf.tiff) {
        // TIFF differences back to samples (per row, 8-bit), and differenced again on the way out.
        let ok = io.byte(data_in.map(|x| (row > 0 && x.len() % row == 0) as u8));
        if ok == 1 && row > 0 {
            let px_in = data_in.map(|x| {
                let mut v = x.to_vec();
                for r in v.chunks_mut(row) {
                    for i in bpp..r.len() {
                        r[i] = r[i].wrapping_add(r[i - bpp]);
                    }
                }
                v
            });
            io.m.set_kind(Kind::Image, Geom { stride: row, bpp, exact: false }, 0);
            let mut px = Vec::with_capacity(reserve(dlen));
            io.bytes(px_in.as_deref(), dlen, &mut px);
            for r in px.chunks_mut(row) {
                for i in (bpp..r.len()).rev() {
                    r[i] = r[i].wrapping_sub(r[i - bpp]);
                }
            }
            decoded = px;
            used_png = true;
            stats.unpredicted_images += 1;
        }
    }
    if !used_png {
        if inf.png.is_some() {
            inf.geom = Geom { stride: inf.geom.stride + 1, bpp: 1, exact: true };
        }
        if let Some((w, h, bpc, comps)) = inf.img {
            // The colour space can't always tell the number of components (an ICC profile's /N lives in another
            // object); the data length can. Both sides see the same length and dictionary.
            for c in [comps, 1, 3, 4, 2, 5, 6, 7, 8] {
                let row = (w * c * bpc).div_ceil(8);
                if row > 0 && h > 0 && row.checked_mul(h) == Some(dlen) {
                    inf.geom.stride = row;
                    inf.geom.bpp = ((c * bpc) / 8).max(1);
                    break;
                }
            }
        }
        // Decoded data can itself be compressed (a JPEG behind Flate, say).
        let flat =
            io.byte(data_in.map(|x| (inf.png.is_none() && looks_compressed(x, PLAIN_WHEN_COMPRESSED)) as u8)) == 1;
        io.m.set_kind(inf.kind, inf.geom, 0);
        io.m.set_flat(flat);
        io.bytes(data_in, dlen, &mut decoded);
        io.m.set_flat(false);
        if flat {
            stats.flat_bytes += dlen;
        }
    }
    stats.decoded_bytes += dlen;
    decoded
}

/// Starts like a zlib stream: deflate, window at most 32 KiB, no preset dictionary, valid check bits. The
/// stream's bytes decide, not its dictionary: a /Filter given by reference, for example, still counts.
/// Whatever is tried here is checked by rebuilding it exactly (and every pack is restored and compared).
fn zlib_header(raw: &[u8]) -> bool {
    raw.len() >= 8
        && raw[0] & 0x0f == 8
        && raw[0] >> 4 <= 7
        && ((raw[0] as u16) << 8 | raw[1] as u16).is_multiple_of(31)
        && raw[1] & 0x20 == 0
}

/// For a Flate payload that zlib made: (parameters, decoded data, bytes after the zlib stream).
fn rebuild_params(_d: &[u8], _start: usize, raw: &[u8]) -> Option<(zlibexact::Params, Vec<u8>, Vec<u8>)> {
    if !zlib_header(raw) {
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
    let mut out = header(VERSION, input);
    out.extend_from_slice(&enc.finish());
    // Never hand over something we can't restore exactly.
    let out = match unpack(&out) {
        Ok(back) if back == input => out,
        _ => {
            stats.fallback = true;
            fallback(input)
        }
    };
    let stored = stored(input);
    if out.len() > stored.len() {
        stats.stored = true;
        return (stored, stats);
    }
    (out, stats)
}

/// Version 2: the file as it is (when nothing makes it smaller).
fn stored(input: &[u8]) -> Vec<u8> {
    let mut out = header(2, input);
    out.extend_from_slice(input);
    out
}

/// Restores the original file; checks its length and hash.
pub fn unpack(a: &[u8]) -> Result<Vec<u8>, String> {
    if a.len() < 6 || &a[..4] != MAGIC {
        return Err("not a gpress archive".into());
    }
    let ver = a[4];
    let mut len: u64 = 0;
    let mut i = 5;
    let mut shift = 0;
    loop {
        let b = *a.get(i).ok_or("truncated header")?;
        i += 1;
        len |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 56 {
            return Err("bad length".into());
        }
    }
    if len > MAX_FILE as u64 {
        return Err("damaged gpress archive (impossible size)".into());
    }
    let len = len as usize;
    let want = a.get(i..i + HASH_LEN).ok_or("truncated header")?.to_vec();
    let body = &a[i + HASH_LEN..];
    let out = match ver {
        VERSION => {
            let mut stats = PackStats::default();
            let (out, _) = run(Decoder::new(body), None, len, &mut stats);
            if stats.damaged {
                return Err("damaged gpress archive".into());
            }
            out
        }
        0 => fallback_decode(body, len).ok_or("damaged gpress archive")?,
        2 => body.to_vec(),
        _ => return Err(format!("unknown version {ver}")),
    };
    if out.len() != len || sha256(&out)[..HASH_LEN] != want[..] {
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
    let mut out = header(0, input);
    out.extend_from_slice(&e.finish());
    out
}

fn fallback_decode(body: &[u8], len: usize) -> Option<Vec<u8>> {
    let mut model = Model::new(table_bits(len));
    model.set_kind(Kind::Raw, Geom::default(), 0);
    let mut dcd = Decoder::new(body);
    let mut out = Vec::with_capacity(reserve(len));
    for i in 0..len {
        if i & 0xfff == 0 && dcd.overrun() {
            return None;
        }
        out.push(model.code_byte(&mut dcd, None));
    }
    Some(out)
}
