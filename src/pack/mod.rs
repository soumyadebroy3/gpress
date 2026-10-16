//! The gpress archive: any PDF, packed with our own PDF-aware model, restored byte for byte.
//!
//! The file is cut into PDF syntax and stream payloads. Flate payloads that zlib made are stored decoded
//! (with the few parameters needed to rebuild the exact compressed bytes); image rows lose their PNG
//! predictor bytes so the pixels themselves can be modelled. Everything is then coded by the
//! context-mixing model, which knows what kind of data each part is.
//!
//! Type 1 fonts lose their two layers of encryption first (`type1`), so the model sees their text and their
//! charstrings. CCITT fax streams are stored as their image (`crate::fax::part`). In a file the empty
//! password opens, encrypted streams are stored decrypted (`sealed`).
//!
//! Layout: magic (4) | version (1) | original length (varint) | decoded bytes the parts claim in all (varint) |
//! first 16 bytes of the SHA-256 of the original | coded data. Every part of the coded data starts with a tag
//! byte (T_*); the model's settings, the kinds of parts and how each is coded are constants of the format
//! version, so a pack restores the same anywhere. The two numbers in the header are what a restore costs: a
//! decoder refuses a file declaring more than its caller allows before it decodes anything, and holds every
//! part to them (see `UnpackOptions`).

use crate::clock::{self, Deadline};
use crate::cm::model::{Geom, Kind, Model};
use crate::cm::{Coder, Decoder, Encoder};
use crate::flate::{zlib, zlibexact};
use crate::par;
use crate::pdf::object::{Dict, Obj};
use crate::pdf::parse::Lexer;
use std::borrow::Cow;

pub const MAGIC: &[u8; 4] = b"\x89GPZ";
/// The format version this build writes and reads (plus 0 and 2, the fallback and stored forms). Version 8
/// is version 7 (version 6, the first with Type 1 fonts, fax images and decrypted streams together, without
/// the preflate part) with the decoded total in the header; the coded body is the same. Nothing earlier
/// shipped, so earlier versions are refused.
pub const VERSION: u8 = 8;
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
/// larger than the full model even on such data (measured), so only when speed is asked for (see
/// `PackOptions`). Only the encoder decides; the decision is stored in the file.
fn looks_compressed(x: &[u8], allowed: bool) -> bool {
    if !allowed || x.len() < 1024 {
        return false;
    }
    let mut h = [0u64; 256];
    for &b in x {
        h[b as usize] += 1;
    }
    entropy(&h) >= 7.9
}

/// Bits per symbol of a histogram.
fn entropy(h: &[u64; 256]) -> f64 {
    let n = h.iter().sum::<u64>() as f64;
    h.iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Plain coding below `PLAIN_BELOW` (see `PackOptions`): for data that looks compressed and whose differences
/// between bytes 1 to 4 apart (neighbouring samples of 1 to 4 bytes) are evenly spread too, as in compressed
/// data. Image samples, a gradient say, can have evenly spread bytes, but their differences show the structure
/// the model finds (coded plainly, a 512 x 512 gradient took 400 times the room). Large data is judged on 16
/// windows of 64 KiB spread over it.
fn plainly(x: &[u8], allowed: bool) -> bool {
    if !looks_compressed(x, allowed) {
        return false;
    }
    const WINDOW: usize = 1 << 16;
    const WINDOWS: usize = 16;
    let mut h = [[0u64; 256]; 4];
    let mut add = |w: &[u8]| {
        for (i, &b) in w.iter().enumerate() {
            for d in 1..=4.min(i) {
                h[d - 1][b.wrapping_sub(w[i - d]) as usize] += 1;
            }
        }
    };
    if x.len() <= WINDOW * WINDOWS {
        add(x);
    } else {
        for k in 0..WINDOWS {
            let a = (k as u64 * (x.len() - WINDOW) as u64 / (WINDOWS - 1) as u64) as usize;
            add(&x[a..a + WINDOW]);
        }
    }
    h.iter().all(|h| entropy(h) >= 7.9)
}

/// The header: magic, version, the original length, the decoded bytes the parts claim in all (0 for the
/// stored and fallback forms, which have no such parts), the hash.
fn header(version: u8, input: &[u8], claimed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(MAGIC);
    out.push(version);
    push_varint(&mut out, input.len() as u64);
    push_varint(&mut out, claimed);
    out.extend_from_slice(&sha256(input)[..HASH_LEN]);
    out
}

fn push_varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// A base-128 number from `a` at `i`: the value and the index after it, or None when it runs out or is longer
/// than a u64 allows.
fn read_varint(a: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut n: u64 = 0;
    let mut shift = 0;
    loop {
        let b = *a.get(i)?;
        i += 1;
        n |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some((n, i));
        }
        shift += 7;
        if shift > 56 {
            return None;
        }
    }
}

mod pipe;
mod sealed;
mod sha256;
pub use sha256::sha256;
pub mod type1;

#[derive(Debug, Default, Clone)]
pub struct PackStats {
    pub syntax_bytes: usize,
    pub streams: usize,
    pub rebuilt: usize,
    pub rebuilt_bytes: usize,
    pub raw_stream_bytes: usize,
    pub decoded_bytes: usize,
    pub unpredicted_images: usize,
    /// Type 1 fonts coded with their encryption removed.
    pub type1: usize,
    pub leptons: usize,
    pub fallback: bool,
    pub stored: bool,
    /// Restore only: the file is damaged (it can't be restored; nothing wrong is ever returned).
    pub damaged: bool,
    /// Restore only: the file has parts made with the existing library (Lepton; see `PackOptions::existing`)
    /// and this build doesn't have it, so the file can't be restored here.
    pub needs_existing: bool,
    /// Bytes coded plainly (already compressed) and bytes coded by one long-repeat decision.
    pub flat_bytes: usize,
    pub bypassed: u64,
    /// Encrypted streams stored decrypted (and encrypted again exactly on restore), and their bytes as stored
    /// in the file (encrypted).
    pub decrypted: usize,
    pub decrypted_bytes: usize,
    /// CCITT fax streams coded as images (and written back bit for bit).
    pub fax: usize,
    /// The deadline passed before the work was done: the encoder then stored the file as it is (see
    /// `PackOptions::deadline`); the decoder gave up (see `UnpackOptions::deadline`; `damaged` is set too).
    pub timed_out: bool,
    /// Encoder only: the decoded bytes the parts claim in all, what the header declares and what
    /// `UnpackOptions::max_decoded` must cover on restore (at most `PackOptions::max_decoded`).
    pub claimed: u64,
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
        // Prefer /Length when it lands on endstream; otherwise search. The sum is checked: on a 32-bit
        // target a crafted /Length would wrap and could land on an "endstream" before the data.
        let dict = dict_before(d, k);
        let mut end = None;
        if let Some(Obj::Int(l)) = dict.as_ref().and_then(|x| x.get(b"Length").cloned()) {
            if let Some(e) = usize::try_from(l).ok().and_then(|l| start.checked_add(l)) {
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
                Some(b"DeviceN") => (a.get(1).and_then(|x| x.as_array()).map_or(1, |v| v.len().clamp(1, 32)), false),
                Some(b"Separation") | Some(b"CalGray") => (1, false),
                Some(b"CalRGB") | Some(b"Lab") => (3, false),
                // The real count is in the ICC stream (/N); the data length confirms it below.
                _ => (3, false),
            },
            _ => (1, false),
        };
        // In u64, so that a 32-bit build gets the value a 64-bit one does (at most 2^30, which fits); so `prow`.
        let row = (w as u64 * (comps * bpc) as u64).div_ceil(8) as usize;
        let bpp = ((comps * bpc) / 8).max(1);
        inf.kind = Kind::Image;
        // Palette indices and packed pixels are symbols: modelled exactly, not as differences.
        let exact = indexed || bpc < 8;
        inf.geom = Geom { stride: row, bpp, exact };
        inf.img = Some((w, h, bpc, comps));
        if let Some(p) = &parms {
            let pred = int(p, b"Predictor").unwrap_or(1);
            // Guesses, so clamped like the image's own values: the data length confirms them (a predictor
            // row is checked against it), and both sides read the same dictionary.
            let cols = int(p, b"Columns").unwrap_or(1).clamp(1, 1 << 24) as usize;
            let colors = int(p, b"Colors").unwrap_or(1).clamp(1, 32) as usize;
            let pbpc = int(p, b"BitsPerComponent").unwrap_or(8).clamp(1, 16) as usize;
            if pred >= 10 {
                let prow = (cols as u64 * (colors * pbpc) as u64).div_ceil(8) as usize;
                let pbpp = (colors * pbpc).div_ceil(8).max(1);
                inf.png = Some((prow, pbpp));
                inf.geom = Geom { stride: prow, bpp: pbpp, exact: exact || pbpc < 8 };
                inf.img = None;
            } else if pred == 2 {
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

/// Undoes PNG filter type `ft` (0 to 4) on one row in place: `cur` holds the filtered bytes and becomes the
/// pixels; `prev` is the row above (pixels), None for the first row.
#[inline]
fn unfilter_row(ft: u8, cur: &mut [u8], prev: Option<&[u8]>, bpp: usize) {
    for i in 0..cur.len() {
        let a = if i >= bpp { cur[i - bpp] } else { 0 };
        let b = prev.map_or(0, |p| p[i]);
        let c = if i >= bpp { prev.map_or(0, |p| p[i - bpp]) } else { 0 };
        let x = cur[i];
        cur[i] = match ft {
            0 => x,
            1 => x.wrapping_add(a),
            2 => x.wrapping_add(b),
            3 => x.wrapping_add(((a as u16 + b as u16) / 2) as u8),
            _ => x.wrapping_add(paeth(a, b, c)),
        };
    }
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
    /// Encoder only: code compressed-looking data plainly (see `PackOptions`).
    plain: bool,
    /// Bytes the parts of this file may still claim beyond what they restore (decoded streams, Lepton data):
    /// what the header declares, less what the parts so far claimed. A damaged file that claims more is
    /// refused where it does, before anything is reserved for the claim.
    budget: u64,
    /// Encoder only: stop coding once this has passed (`timed_out` says so; the output is then dropped).
    deadline: Deadline,
    timed_out: bool,
}

/// Bytes coded between two looks at the deadline (a few milliseconds of model work).
const DEADLINE_STEP: usize = 1 << 16;
/// Bytes coded between two looks for a damaged file (`Coder::exhausted`).
const CHECK_STEP: usize = 1 << 12;
const _: () = assert!(DEADLINE_STEP.is_multiple_of(CHECK_STEP));

/// Largest single part a file may declare (decoded stream, Lepton data): 2 GiB.
const MAX_PART: usize = 1 << 31;
/// Largest file the format restores: 16 GiB (a u64, so the limit is the same on 32-bit targets, where a
/// file that doesn't fit in memory is refused by the header alone).
pub const MAX_FILE: u64 = 1 << 34;
/// The decoded bytes the parts of one archive may claim in all unless the caller says otherwise, on both
/// sides (`PackOptions::max_decoded`, `UnpackOptions::max_decoded`): enough for every file of the test set
/// (the largest needs 232 MiB) with a little to spare.
pub const DEFAULT_MAX_DECODED: u64 = 256 << 20;
/// Memory reserved up front is capped; vectors grow with what is actually decoded.
fn reserve(n: usize) -> usize {
    n.min(1 << 22)
}

impl<'m, C: Coder> Io<'m, C> {
    fn byte(&mut self, b: Option<u8>) -> u8 {
        self.m.code_byte(&mut self.c, b)
    }
    /// The length of a part that isn't restored as it is (a decoded stream, Lepton data): at most `max`, and
    /// within what the file may still claim.
    fn part(&mut self, v: Option<u64>, max: usize) -> usize {
        let n = self.length(v, max);
        if n as u64 > self.budget {
            self.damaged = true;
            return 0;
        }
        self.budget -= n as u64;
        n
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
        self.bytes_to(data, n, |b| out.push(b));
    }
    /// Codes `n` bytes (those of `data`, which holds them, when encoding) and hands each to `keep`.
    fn bytes_to(&mut self, data: Option<&[u8]>, n: usize, mut keep: impl FnMut(u8)) {
        let mut i = 0;
        while i < n {
            // Before every CHECK_STEP bytes a look for a damaged file, and at the deadline every DEADLINE_STEP.
            if self.c.exhausted() {
                self.damaged = true;
                return;
            }
            if i % DEADLINE_STEP == 0 && self.out_of_time() {
                return;
            }
            let end = n.min(i + CHECK_STEP);
            self.m.code_bytes(&mut self.c, data.map(|d| &d[i..end]), end - i, &mut keep);
            i = end;
        }
    }
    /// Codes `n` bytes without keeping them. Not inlined: one copy of the coding loop serves every caller.
    #[inline(never)]
    fn skip(&mut self, data: Option<&[u8]>, n: usize) {
        self.bytes_to(data, n, |_| ());
    }
    /// Codes `n` bytes the decoder makes a stream again from, which it keeps in `out`. The encoder makes
    /// nothing again (see `Restored`), so it keeps none.
    fn source_bytes(&mut self, data: Option<&[u8]>, n: usize, out: &mut Vec<u8>) {
        if data.is_some() {
            self.skip(data, n);
        } else {
            out.reserve(reserve(n));
            self.bytes(None, n, out);
        }
    }
    /// Encoder only: whether the deadline has passed; the pass is then given up (as if damaged, so nothing
    /// of it is used) and `timed_out` says why.
    fn out_of_time(&mut self) -> bool {
        if !self.timed_out && self.deadline.passed() {
            self.timed_out = true;
            self.damaged = true;
        }
        self.timed_out
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

/// Choices of the encoder. The decoder needs none of them: what they decide is stored in the file, so a
/// file restores the same whatever they were (a file with parts made by the existing library needs a build
/// that has it, see `existing`).
#[derive(Clone, Debug)]
pub struct PackOptions {
    /// How hard to try, 0 to 100. Below `PLAIN_BELOW`, data that looks compressed already (no structure in
    /// its bytes or their differences, see `looks_compressed`) is coded plainly: on 3 MB of random bytes 17
    /// times faster (0.8 s instead of 14 s) and no larger; up to about 1% larger on compressed data the model
    /// still finds a little in. A Type 1 font program is never coded plainly: encrypted it looks random, but
    /// opened (see `type1`) it is text and charstrings the model codes to a third or less.
    pub effort: u8,
    /// Threads for looking at the streams (0: every core the machine offers, at most 64). Never changes the
    /// output.
    pub threads: usize,
    /// Store the encrypted streams of a file the empty password opens decrypted, so the usual plan applies
    /// to them (see `sealed`); they are encrypted again exactly on restore. When the pack isn't clearly
    /// smaller than the encrypted bytes, the pack without decryption is made too and the smaller kept.
    pub decrypt: bool,
    /// Largest decoded stream stored in its decoded form; a Flate stream that decodes to more is stored as
    /// it is. (The decoder takes up to `MAX_PART` per part, whatever this was.)
    pub max_stream: usize,
    /// The most decoded bytes the parts of one archive may claim in all (decoded streams, and Lepton data
    /// for a JPEG inside one or stored as it is). In file order, a stream whose claim doesn't fit in what is
    /// left is stored as it is; decided from sizes alone, so the output is the same on any machine. The
    /// header declares the total, and a decoder refuses an archive declaring more than its own
    /// `UnpackOptions::max_decoded`: an archive made with this budget restores wherever that budget is
    /// allowed. The memory of a pack or a restore follows this and the file's length (see `README.md`).
    pub max_decoded: u64,
    /// Try the existing library (Lepton, for JPEG streams) when the build includes it (`--features existing`).
    /// A file with parts it made restores only with such a build (`unpack` says so); the default is what the
    /// build has.
    pub existing: bool,
    /// Stop when this passes (checked while planning and every 64 KiB of coding): the file is then stored
    /// as it is (a valid archive, restored exactly) and `PackStats::timed_out` says so. A pack that finishes
    /// gives the same bytes with or without a deadline.
    pub deadline: Deadline,
}

impl Default for PackOptions {
    fn default() -> Self {
        PackOptions {
            effort: 100,
            threads: 0,
            decrypt: true,
            max_stream: 1 << 28,
            max_decoded: DEFAULT_MAX_DECODED,
            existing: HAVE_EXISTING,
            deadline: Deadline::NONE,
        }
    }
}

/// Whether this build has the existing library (Lepton).
const HAVE_EXISTING: bool = cfg!(feature = "existing");

/// Choices of the decoder: the two numbers in a file's header it may refuse, and a deadline. The decoder's
/// work and memory follow the declared length and the declared decoded total (a damaged body can't claim
/// beyond them), so a service that takes files from anyone sets both to what it can afford.
#[derive(Clone, Debug)]
pub struct UnpackOptions {
    /// Largest file restored, in bytes: a file whose header declares more is refused before anything is
    /// decoded. At most `MAX_FILE` either way.
    pub max_len: u64,
    /// Most decoded bytes the parts may claim in all: a file whose header declares more is refused before
    /// anything is decoded, and every part is then held to what the header declared.
    pub max_decoded: u64,
    /// Stop when this passes (checked between parts and every 64 KiB of model work): the restore is then
    /// given up with `PackStats::timed_out` set, never a wrong file. A restore that finishes gives the same
    /// bytes with or without a deadline.
    pub deadline: Deadline,
}

impl Default for UnpackOptions {
    fn default() -> Self {
        UnpackOptions { max_len: MAX_FILE, max_decoded: DEFAULT_MAX_DECODED, deadline: Deadline::NONE }
    }
}

/// Below this effort, compressed-looking data is coded plainly.
const PLAIN_BELOW: u8 = 50;

const T_SYNTAX: u8 = 0;
const T_RAW: u8 = 1;
const T_REBUILT: u8 = 2;
// Tag 3 was the preflate part of version 6 and is not used.
const T_LEPTON: u8 = 4;
/// A CCITT fax stream coded as an image (see `crate::fax::part`).
const T_FAX: u8 = 10;
/// Encrypted files (see `sealed`): the file key, and an encrypted stream's settings before its payload.
const T_CRYPT_KEY: u8 = 20;
const T_CRYPT: u8 = 21;
const T_END: u8 = 255;

fn table_bits(n: usize) -> u32 {
    // 16-byte buckets: up to 2^20 per context (16 MiB each) for large files.
    let mut b = 14;
    while b < 20 && (1usize << (b + 3)) < n {
        b += 1;
    }
    b
}

/// Where a stream's bytes come from: a range of the file, or bytes made on the way (a decrypted payload).
enum Source {
    File(usize, usize),
    Bytes(Vec<u8>),
}

impl Source {
    fn bytes<'a>(&'a self, d: Option<&'a [u8]>) -> Option<&'a [u8]> {
        match self {
            Source::File(a, b) => d.map(|d| &d[*a..*b]),
            Source::Bytes(v) => Some(v.as_slice()),
        }
    }
}

enum Plan {
    Syntax(usize, usize),
    Raw(usize, usize),
    /// A zlib stream stored decoded: the planner keeps its settings and the length of its decoded form,
    /// `dlen`, and the stream is inflated again, in pieces, as it is coded (a plan holds no decoded data, so the
    /// planner's memory doesn't follow what the streams decode to). `tail`: the bytes after the zlib stream.
    /// `lep`: the decoded data is a JPEG, recompressed with Lepton (when that beats the Flate bytes). `looks`
    /// and `plain`: what the coding of the decoded data decides from all of it (`looks_like_content`,
    /// `plainly`), worked out by the planner, which holds it.
    Rebuilt {
        params: zlibexact::Params,
        src: Source,
        dlen: usize,
        tail: Vec<u8>,
        lep: Option<Vec<u8>>,
        looks: bool,
        plain: bool,
    },
    /// A JPEG recompressed with an existing library (Lepton), restored bit for bit.
    Lepton {
        src: Source,
        data: Vec<u8>,
    },
    /// An encrypted stream's decrypted bytes that nothing rebuilds (coded like a raw stream).
    Plain(Vec<u8>),
    /// The file key of an encrypted file, before its first encrypted stream.
    CryptKey(Vec<u8>),
    /// An encrypted stream's settings; the next part is its decrypted payload.
    Crypt(sealed::Sealed),
    /// A CCITT fax stream: the image and every choice of its encoder (see `crate::fax::part`), the byte of the
    /// file its data starts at (the work budget counts from there), and where its bytes are when they are
    /// stored as they are (None: an encrypted stream's decrypted bytes, see `sealed`).
    Fax {
        parsed: Box<crate::fax::ccitt::Parsed>,
        at: usize,
        raw: Option<(usize, usize)>,
    },
}

/// What to do with each part of the file. The streams are looked at in parallel (the biggest first); the
/// plans keep the file's order.
fn plan(d: &[u8], opt: &PackOptions) -> Vec<Plan> {
    let t = clock::now();
    let segs = segments(d);
    let pool = par::Pool::new(if opt.threads == 0 { par::default_threads() } else { opt.threads });
    let size = |s: &Seg| match *s {
        Seg::Syntax(..) => 0,
        Seg::Stream(a, b) => b - a,
    };
    let mut order: Vec<usize> = (0..segs.len()).collect();
    order.sort_by_key(|&k| (std::cmp::Reverse(size(&segs[k])), k));
    let objects = ObjIndex::new();
    let plans = par::map(Some(&pool), segs.len(), Some(&order), |k| match segs[k] {
        Seg::Syntax(a, b) => Plan::Syntax(a, b),
        // Out of time: the cheapest plan (the pass is given up anyway).
        Seg::Stream(a, b) if opt.deadline.passed() => Plan::Raw(a, b),
        Seg::Stream(a, b) => match stream_plan(&d[a..b], opt) {
            Some(found) => found.plan(Source::File(a, b)),
            None => fax(d, a, b, &objects).unwrap_or(Plan::Raw(a, b)),
        },
    });
    if std::env::var_os("PDFE_DEBUG").is_some() {
        eprintln!("pack: {} parts planned in {:?}", plans.len(), clock::since(t));
    }
    plans
}

/// What `stream_plan` found for a stream's bytes, before it knows where they come from.
enum Found {
    Rebuilt { params: zlibexact::Params, dlen: usize, tail: Vec<u8>, lep: Option<Vec<u8>>, looks: bool, plain: bool },
    Lepton(Vec<u8>),
}

impl Found {
    fn plan(self, src: Source) -> Plan {
        match self {
            Found::Rebuilt { params, dlen, tail, lep, looks, plain } => {
                Plan::Rebuilt { params, src, dlen, tail, lep, looks, plain }
            }
            Found::Lepton(data) => Plan::Lepton { src, data },
        }
    }
}

/// The plan for a stream's bytes, wherever they come from (as stored in the file, or an encrypted stream's
/// bytes decrypted): a zlib rebuild or Lepton, or None when neither applies (the stream is then stored as it
/// is). Every way of storing a stream's bytes other than as they are goes through here, so a decrypted
/// stream gets the same choices as a plain one. The decoded data is dropped here: the stream is inflated
/// again when it is coded.
fn stream_plan(raw: &[u8], opt: &PackOptions) -> Option<Found> {
    if let Some((params, data, tail)) = rebuild_params(raw, opt.max_stream) {
        let lep = if opt.existing { nested_lepton(&data, raw.len()) } else { None };
        // What the coding decides from all of the data (see `Plan::Rebuilt`). `plainly` is left out for a JPEG
        // inside, which goes to Lepton, and a stream larger than the whole budget, which is stored as it is
        // (`within_budget`). The planner doesn't read the dictionary, so PNG rows, TIFF samples and Type 1 fonts
        // still work it out though nothing reads it (a pass over the data, below `PLAIN_BELOW` only).
        let wanted = lep.is_none() && data.len() as u64 <= opt.max_decoded;
        let (looks, plain) = (looks_like_content(&data), wanted && plainly(&data, opt.effort < PLAIN_BELOW));
        return Some(Found::Rebuilt { params, dlen: data.len(), tail, lep, looks, plain });
    }
    if !opt.existing {
        return None;
    }
    lepton_bytes(raw).map(Found::Lepton)
}

/// The plans of a file with the encoder's choices in `opt`: `plan`, with the encrypted streams of a file the
/// empty password opens decrypted when `opt.decrypt` (see `sealed`), the fax streams within their work
/// budget, and the decoded parts within `opt.max_decoded`. Returns the plans and the decoded bytes they
/// claim in all, which the header declares.
fn plans(d: &[u8], opt: &PackOptions) -> (Vec<Plan>, u64) {
    let p = plan(d, opt);
    let p = if opt.decrypt { sealed::crypt_plans(d, p, opt) } else { p };
    within_budget(fax_budget(p), opt.max_decoded)
}

/// What a plan's part claims beyond what it restores: the decoded bytes the decoder reserves and models
/// (a decoded stream, and the Lepton data of the JPEG inside it or of a JPEG stored as it is). The same
/// numbers `Io::part` takes from the budget.
fn claim(p: &Plan) -> u64 {
    match p {
        Plan::Rebuilt { dlen, lep, .. } => *dlen as u64 + lep.as_ref().map_or(0, |l| l.len() as u64),
        Plan::Lepton { data, .. } => data.len() as u64,
        _ => 0,
    }
}

/// Keeps the decoded parts of a file within `max_decoded` bytes in all: in file order, a part whose claim
/// doesn't fit in what is left is stored as it is (a decrypted one as its decrypted bytes, like any stream
/// nothing rebuilds). Decided from sizes alone. Returns the plans and what they claim.
fn within_budget(mut plans: Vec<Plan>, max_decoded: u64) -> (Vec<Plan>, u64) {
    let mut left = max_decoded;
    let mut claimed = 0;
    for p in plans.iter_mut() {
        let c = claim(p);
        if c == 0 {
            continue;
        }
        if c <= left {
            left -= c;
            claimed += c;
            continue;
        }
        let src = match p {
            Plan::Rebuilt { src, .. } | Plan::Lepton { src, .. } => std::mem::replace(src, Source::File(0, 0)),
            _ => continue,
        };
        *p = match src {
            Source::File(a, b) => Plan::Raw(a, b),
            Source::Bytes(v) => Plan::Plain(v),
        };
    }
    (plans, claimed)
}

/// For the security suite only: an archive of `input` that declares `claim` decoded bytes and whose one
/// decoded part claims them all (zeros, which code to almost nothing), after a part that restores the whole
/// input. The decoder must refuse it: the decoded bytes are worked through and then found not to restore
/// anything, so the cost of a crafted file is the cost of the budget the caller allowed, never more. `pack`
/// never writes such a file.
#[doc(hidden)]
pub fn crafted_claim(input: &[u8], claim: u64) -> Vec<u8> {
    let claim = usize::try_from(claim).unwrap_or(usize::MAX).min(MAX_PART);
    let params = zlibexact::Params { level: 6, window_bits: 15, mem_level: 8, filtered: false };
    let src = Source::Bytes(zlib_of_zeros(claim));
    let zeros = Plan::Rebuilt { params, src, dlen: claim, tail: Vec::new(), lep: None, looks: false, plain: false };
    let plans = vec![Plan::Syntax(0, input.len()), zeros];
    let mut stats = PackStats::default();
    let (_, enc) =
        run(Encoder::new(), Some(input), Some(plans), input.len(), &PackOptions::default(), &mut stats, claim as u64);
    let mut out = header(VERSION, input, claim as u64);
    out.extend_from_slice(&enc.finish());
    out
}

/// A zlib stream of `n` zeros (the planner's source for a crafted part).
fn zlib_of_zeros(n: usize) -> Vec<u8> {
    zlibexact::zlib_stream(
        &vec![0u8; n],
        zlibexact::Params { level: 6, window_bits: 15, mem_level: 8, filtered: false },
    )
}

/// For measuring only: the file with every stream we can rebuild replaced by its decoded bytes (Type 1 fonts
/// opened) and the JPEGs Lepton takes left out, plus the size of what a restore needs on top (rebuild
/// settings, Lepton data). A general compressor on the first, plus the second, is the fair "decompress
/// first, then compress" baseline for the gpress archive: the same preparation, without our model.
pub fn expand(d: &[u8]) -> (Vec<u8>, usize) {
    // Type 1 font programs are opened, as in a pack.
    fn put(out: &mut Vec<u8>, data: &[u8]) -> usize {
        let (x, extra) = type1::expanded(out, data);
        out.extend_from_slice(&x);
        extra
    }
    let mut out = Vec::with_capacity(d.len() * 2);
    let mut extra = 0;
    for p in plans(d, &PackOptions::default()).0 {
        match p {
            Plan::Syntax(a, b) => out.extend_from_slice(&d[a..b]),
            Plan::Raw(a, b) => extra += put(&mut out, &d[a..b]),
            Plan::Plain(v) => extra += put(&mut out, &v),
            // The key once; per stream its settings and initialization vector.
            Plan::CryptKey(k) => extra += 1 + k.len(),
            Plan::Crypt(_) => extra += 18,
            Plan::Rebuilt { src, dlen, tail, lep, .. } => {
                match lep {
                    Some(l) => extra += l.len(),
                    None => {
                        let data = src.bytes(Some(d)).and_then(|z| inflated(z, dlen));
                        extra += put(&mut out, &data.unwrap_or_default())
                    }
                }
                out.extend_from_slice(&tail);
                extra += 2;
            }
            Plan::Lepton { data, .. } => extra += data.len(),
            Plan::Fax { parsed, .. } => {
                out.extend_from_slice(&crate::fax::part::packed(&parsed.image));
                out.extend_from_slice(&parsed.layout.trailing);
                extra += 8 + parsed.layout.end.len();
            }
        }
    }
    (out, extra)
}

#[cfg(feature = "existing")]
fn lepton_features() -> lepton_jpeg::EnabledFeatures {
    let mut f = lepton_jpeg::EnabledFeatures::compat_lepton_vector_write();
    f.max_processor_threads = 1;
    f
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
fn lepton_bytes(_raw: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(feature = "existing"))]
fn unlepton(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

/// The file restored so far. The decoder builds it: it is the result. The encoder only counts it: each part
/// restores exactly the input bytes it was planned from (the planner checked every way a part is made again:
/// the zlib settings rebuild the stream, Lepton's data gives the JPEG back, an opened font closes to itself, a
/// fax image writes its stream again, a decrypted payload encrypts back), so the input's first `len` bytes
/// stand for it, and the encoder makes nothing again but a fax stream, which its coder writes as it goes, and
/// an encrypted payload, encrypted again from the bytes it was given. Within the turn of an encrypted stream's
/// payload it counts the decrypted bytes, which are replaced by the encrypted ones before anything looks back.
enum Restored<'a> {
    Built(Vec<u8>),
    Counted(&'a [u8], usize),
}

impl Restored<'_> {
    fn len(&self) -> usize {
        match self {
            Restored::Built(v) => v.len(),
            Restored::Counted(_, n) => *n,
        }
    }
    /// The bytes restored so far, for looking back (at the dictionary before a stream, say).
    fn view(&self) -> &[u8] {
        match self {
            Restored::Built(v) => v,
            Restored::Counted(d, n) => &d[..(*n).min(d.len())],
        }
    }
    fn keep(&mut self, b: u8) {
        match self {
            Restored::Built(v) => v.push(b),
            Restored::Counted(_, n) => *n += 1,
        }
    }
    fn keep_all(&mut self, x: &[u8]) {
        match self {
            Restored::Built(v) => v.extend_from_slice(x),
            Restored::Counted(_, n) => *n += x.len(),
        }
    }
    fn truncate(&mut self, len: usize) {
        match self {
            Restored::Built(v) => v.truncate(len),
            Restored::Counted(_, n) => *n = len.min(*n),
        }
    }
    /// The decoder's file (the encoder keeps none).
    fn into_file(self) -> Vec<u8> {
        match self {
            Restored::Built(v) => v,
            Restored::Counted(..) => Vec::new(),
        }
    }
}

/// The decoded form of a stream's bytes, inflated again: exactly `dlen` bytes, or None (which can't happen for
/// a stream the planner inflated once already).
fn inflated(raw: &[u8], dlen: usize) -> Option<Vec<u8>> {
    let u = zlib::unwrap(raw, dlen).ok()?;
    (u.data.len() == dlen).then_some(u.data)
}

/// Encoder only: a rebuilt stream (`raw`, its bytes as stored), whose decoded bytes are inflated again in
/// pieces as they are coded, never held whole, and what the planner found in them (see `Plan::Rebuilt`).
struct Feed<'a> {
    raw: &'a [u8],
    dlen: usize,
    looks: bool,
    plain: bool,
}

impl Feed<'_> {
    /// Hands the decoded bytes to `f` in order, a Deflate block at a time. False if `f` stopped, or if they
    /// aren't the `dlen` bytes they were when the stream was planned. (`dyn`: one inflater for every caller.)
    fn each(&self, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        matches!(zlib::unwrap_pieces(self.raw, self.dlen, f), Ok(Some((n, _))) if n == self.dlen)
    }

    /// The row types of PNG predictor rows of `row` bytes, from a first pass: Ok(None) when the data isn't
    /// whole rows whose types are all known (0 to 4), and Err when it doesn't inflate again as planned.
    fn png_types(&self, row: usize) -> Result<Option<Vec<u8>>, ()> {
        if row == 0 || !self.dlen.is_multiple_of(row + 1) {
            return Ok(None);
        }
        let mut types = Vec::with_capacity(self.dlen / (row + 1));
        let (mut at, mut known) = (0, true);
        let done = self.each(&mut |p| {
            // The rows start every row + 1 bytes from the start of the data; `at` is where `p` starts.
            let mut i = (row + 1 - at % (row + 1)) % (row + 1);
            while i < p.len() {
                known &= p[i] <= 4;
                types.push(p[i]);
                i += row + 1;
            }
            at += p.len();
            known
        });
        match (known, done) {
            (false, _) => Ok(None),
            (true, true) => Ok(Some(types)),
            (true, false) => Err(()),
        }
    }

    /// The whole decoded stream (for a Type 1 font program, which is small).
    fn whole(&self) -> Option<Vec<u8>> {
        inflated(self.raw, self.dlen)
    }
}

/// Encoder: codes the pixels of PNG predictor rows of `row` bytes as the stream is inflated again (the types
/// are coded already): a row is unfiltered in place once its bytes are in, and whole rows are coded about
/// 64 KiB at a time (one call per row is slow for narrow rows). The row above the first of a batch is the last
/// one coded, read back from the model's history. Holds a batch and a row. False if the stream doesn't
/// inflate again as planned.
fn png_pixels<C: Coder>(io: &mut Io<C>, feed: &Feed, row: usize, bpp: usize) -> bool {
    const BATCH: usize = 1 << 16;
    // The unfiltered rows not coded yet, then the `fill` bytes of the row of type `ft` that is coming in.
    let mut px = Vec::new();
    let (mut ft, mut fill, mut first) = (None, 0, true);
    let done = feed.each(&mut |mut p| {
        while !p.is_empty() {
            let Some(t) = ft else {
                ft = Some(p[0]);
                p = &p[1..];
                continue;
            };
            let take = (row - fill).min(p.len());
            px.extend_from_slice(&p[..take]);
            (fill, p) = (fill + take, &p[take..]);
            if fill < row {
                continue;
            }
            let at = px.len() - row;
            let (before, cur) = px.split_at_mut(at);
            let above = if first {
                None
            } else if at >= row {
                Some(&before[at - row..])
            } else {
                match io.m.hist_len().checked_sub(row).and_then(|a| io.m.history(a, row)) {
                    Some(h) => Some(h),
                    None => return false,
                }
            };
            unfilter_row(t, cur, above, bpp);
            (ft, fill, first) = (None, 0, false);
            if px.len() >= BATCH {
                io.skip(Some(&px), px.len());
                px.clear();
                if io.damaged {
                    return false;
                }
            }
        }
        true
    });
    if done && !px.is_empty() {
        io.skip(Some(&px), px.len());
    }
    done && !io.damaged
}

/// Encoder: codes the samples of TIFF predictor 2 rows of `row` bytes, `bpp` per pixel, as the stream is
/// inflated again: each byte plus the sample `bpp` before it in its row, a piece at a time.
fn tiff_samples<C: Coder>(io: &mut Io<C>, feed: &Feed, row: usize, bpp: usize) -> bool {
    // The last `bpp` samples of the row by their column modulo `bpp` (`k`), and the samples of one piece.
    let mut back = vec![0u8; bpp];
    let (mut col, mut k) = (0, 0);
    let mut px = Vec::new();
    feed.each(&mut |p| {
        px.clear();
        px.extend(p.iter().map(|&x| {
            let s = if col >= bpp { x.wrapping_add(back[k]) } else { x };
            back[k] = s;
            (col, k) = (col + 1, k + 1);
            if k == bpp {
                k = 0;
            }
            if col == row {
                (col, k) = (0, 0);
            }
            s
        }));
        io.skip(Some(&px), px.len());
        !io.damaged
    })
}

/// A decoded stream as `payload` leaves it, for the decoder to make its zlib bytes again. The bytes it coded
/// are in the model's history, which keeps them (every byte coded is added to it, once), so they are read
/// from there; only what the model doesn't see as it is needs a copy.
enum Decoded {
    /// Coded as they are: `len` bytes of the history from `at`.
    Hist { at: usize, len: usize },
    /// PNG predictor rows: `rows` row types from `at`, then the pixels; filtered again on the way out.
    Png { at: usize, rows: usize, row: usize, bpp: usize },
    /// TIFF predictor 2: `len` samples from `at`, differenced again on the way out.
    Tiff { at: usize, len: usize, row: usize, bpp: usize },
    /// Made on the way: a Type 1 font closed again, a JPEG from Lepton (nothing on the encoder).
    Bytes(Vec<u8>),
}

impl Decoded {
    /// The zlib stream of the decoded bytes, or None when the history doesn't hold them (a damaged file).
    fn zlib(&self, m: &Model, p: zlibexact::Params) -> Option<Vec<u8>> {
        let z = match *self {
            Decoded::Hist { at, len } => zlibexact::zlib_stream(m.history(at, len)?, p),
            Decoded::Png { at, rows, row, bpp } => {
                let types = m.history(at, rows)?;
                let px = m.history(at.checked_add(rows)?, rows.checked_mul(row)?)?;
                zlibexact::zlib_stream(&refilter(types, px, row, bpp), p)
            }
            Decoded::Tiff { at, len, row, bpp } => {
                let mut px = m.history(at, len)?.to_vec();
                for r in px.chunks_mut(row) {
                    for i in (bpp..r.len()).rev() {
                        r[i] = r[i].wrapping_sub(r[i - bpp]);
                    }
                }
                zlibexact::zlib_stream(&px, p)
            }
            Decoded::Bytes(ref v) => zlibexact::zlib_stream(v, p),
        };
        Some(z)
    }
}

/// One pass of the format: encoding `d` with its `plans` (Some) or rebuilding it (None). The same code runs
/// both ways so the model sees exactly the same sequence of events. `opt` only guides the encoder's choices,
/// which are stored (among them whether encrypted streams are stored decrypted, see `sealed`), and gives
/// the deadline. `budget` is what the parts may claim in all: what the plans claim for the encoder, what the
/// header declares for the decoder.
fn run<'d, C: Coder>(
    c: C,
    d: Option<&'d [u8]>,
    plans: Option<Vec<Plan>>,
    total_len: usize,
    opt: &PackOptions,
    stats: &mut PackStats,
    budget: u64,
) -> (Restored<'d>, C) {
    let mut model = primed(table_bits(total_len));
    let mut io = Io {
        m: &mut model,
        c,
        damaged: false,
        plain: opt.effort < PLAIN_BELOW,
        budget,
        deadline: opt.deadline,
        timed_out: false,
    };
    let mut out = match d {
        Some(d) => Restored::Counted(d, 0),
        None => Restored::Built(Vec::with_capacity(reserve(total_len))),
    };
    // Encrypted files: the file key, and the settings of the stream whose decrypted payload comes next with
    // where that payload starts in `out`.
    let mut crypt_key: Option<Vec<u8>> = None;
    let mut sealing: Option<(sealed::Sealed, usize)> = None;
    // Every part restores something, except an empty stream and the crypt parts before a payload, so a
    // damaged or hostile file can't keep the decoder busy without progress: at most two such parts in a row.
    let mut idle = 0;
    let mut prev: Option<u8> = None;
    // Made at the first fax stream; the fax streams of a file share what it learns.
    let mut fax_coder: Option<crate::fax::part::FaxCoder> = None;
    let mut si = 0;
    loop {
        let cur = plans.as_ref().map(|p| p.get(si));
        let tag_in = cur.map(|p| match p {
            None => T_END,
            Some(Plan::Syntax(..)) => T_SYNTAX,
            Some(Plan::Raw(..)) | Some(Plan::Plain(..)) => T_RAW,
            Some(Plan::Rebuilt { .. }) => T_REBUILT,
            Some(Plan::Lepton { .. }) => T_LEPTON,
            Some(Plan::CryptKey(..)) => T_CRYPT_KEY,
            Some(Plan::Crypt(..)) => T_CRYPT,
            Some(Plan::Fax { .. }) => T_FAX,
        });
        if io.damaged || out.len() > total_len || io.c.exhausted() || io.out_of_time() {
            io.damaged = true;
            break;
        }
        let left = total_len - out.len();
        io.m.set_kind(Kind::Meta, Geom::default(), out.len() as u64);
        let tag = io.byte(tag_in);
        // An encrypted stream's settings come right after the syntax before the stream (the file key, once,
        // right before the first settings) and are followed by its payload.
        let in_order = match tag {
            T_CRYPT_KEY => crypt_key.is_none() && prev == Some(T_SYNTAX),
            T_CRYPT => matches!(prev, Some(T_SYNTAX) | Some(T_CRYPT_KEY)),
            T_RAW | T_REBUILT | T_LEPTON | T_FAX => prev != Some(T_CRYPT_KEY),
            _ => sealing.is_none() && prev != Some(T_CRYPT_KEY),
        };
        if !in_order {
            io.damaged = true;
            break;
        }
        let before = out.len();
        // Encoder: the bytes this part restores, which it has at hand. An encrypted stream's payload is
        // encrypted again from them (the decoder encrypts what it restored).
        let mut given: Cow<[u8]> = Cow::Borrowed(&[]);
        match tag {
            T_END => break,
            T_SYNTAX | T_RAW => {
                let src: Option<&[u8]> = match cur.flatten() {
                    Some(Plan::Syntax(a, b)) | Some(Plan::Raw(a, b)) => Some(&d.unwrap()[*a..*b]),
                    Some(Plan::Plain(v)) => Some(v.as_slice()),
                    _ => None,
                };
                let n = io.length(src.map(|x| x.len() as u64), left);
                // A Type 1 font stored without a filter: opened, never coded plainly (encrypted it looks random,
                // opened it is text; see `PackOptions::effort`). Both sides find it from the restored bytes.
                let font = if tag == T_RAW { type1::raw_font_dict(out.view()) } else { None };
                let mut flat = false;
                if tag == T_SYNTAX {
                    io.m.set_kind(Kind::Syntax, Geom::default(), out.len() as u64);
                    stats.syntax_bytes += n;
                } else {
                    flat = io.byte(src.map(|x| (font.is_none() && plainly(x, io.plain)) as u8)) == 1;
                    io.m.set_kind(Kind::Raw, Geom::default(), out.len() as u64);
                    stats.streams += 1;
                    stats.raw_stream_bytes += n;
                    if flat {
                        stats.flat_bytes += n;
                    }
                }
                io.m.set_flat(flat);
                match (font, flat) {
                    (Some(dict), false) => {
                        type1::code(&mut io, src, n, Some(&dict), Kind::Raw, |x| out.keep_all(x), stats)
                    }
                    _ => io.bytes_to(src, n, |b| out.keep(b)),
                }
                io.m.set_flat(false);
                if let Some(x) = src {
                    given = Cow::Borrowed(&x[..n]);
                }
            }
            T_REBUILT => {
                // The encoder inflates the stream again as it codes it: the plan kept only its settings and what
                // the planner found in the decoded bytes.
                let (p_in, feed, tail_in, lep_in) = match cur.flatten() {
                    Some(Plan::Rebuilt { params, src, dlen, tail, lep, looks, plain }) => {
                        let Some(raw) = src.bytes(d) else {
                            io.damaged = true;
                            break;
                        };
                        let feed = Feed { raw, dlen: *dlen, looks: *looks, plain: *plain };
                        (Some(*params), Some(feed), Some(tail.as_slice()), Some(lep.as_deref()))
                    }
                    _ => (None, None, None, None),
                };
                let level = io.byte(p_in.map(|p| p.level));
                let wb = io.byte(p_in.map(|p| p.window_bits));
                let mem = io.byte(p_in.map(|p| p.mem_level));
                let filt = io.byte(p_in.map(|p| p.filtered as u8));
                let params = zlibexact::Params { level, window_bits: wb, mem_level: mem, filtered: filt != 0 };
                // Window bits 9 to 15, all that `find_params` makes (zlib itself takes 8 as 9); the re-creation
                // needs a window larger than its lookahead, so 8 can only come from a damaged file.
                if !(1..=9).contains(&level) || !(9..=15).contains(&wb) || !(1..=9).contains(&mem) || filt > 1 {
                    io.damaged = true;
                    break;
                }
                let tail_n = io.length(tail_in.map(|t| t.len() as u64), left);
                let mut tail = Vec::new();
                io.source_bytes(tail_in, tail_n, &mut tail);
                let decoded = payload(&mut io, feed.as_ref(), lep_in, out.view(), stats);
                if io.damaged {
                    break;
                }
                match &feed {
                    // The encoder: these settings were found to make exactly the stream's bytes.
                    Some(f) => {
                        out.keep_all(f.raw);
                        stats.rebuilt_bytes += f.raw.len().saturating_sub(tail_n);
                        given = Cow::Borrowed(f.raw);
                    }
                    None => {
                        let Some(z) = decoded.zlib(io.m, params) else {
                            io.damaged = true;
                            break;
                        };
                        out.keep_all(&z);
                        out.keep_all(&tail);
                        stats.rebuilt_bytes += z.len();
                    }
                }
                stats.streams += 1;
                stats.rebuilt += 1;
            }
            // Parts the existing library made restore only where it is (see `PackOptions::existing`).
            T_LEPTON if !HAVE_EXISTING => {
                stats.needs_existing = true;
                io.damaged = true;
                break;
            }
            T_LEPTON => {
                let (src, jpeg_in) = match cur.flatten() {
                    Some(Plan::Lepton { data, src }) => (Some(data.as_slice()), src.bytes(d)),
                    _ => (None, None),
                };
                // Lepton's data is smaller than the JPEG it restores (see `lepton_bytes`), so never more than
                // what is left of the file.
                let n = io.part(src.map(|x| x.len() as u64), left);
                let at = lepton_part(&mut io, src, n, stats);
                if io.damaged {
                    break;
                }
                match jpeg_in {
                    // The encoder: `lepton_bytes` checked that the data gives this JPEG back.
                    Some(j) => {
                        out.keep_all(j);
                        given = Cow::Borrowed(j);
                    }
                    None => match io.m.history(at, n).and_then(unlepton) {
                        Some(j) => out.keep_all(&j),
                        None => {
                            io.damaged = true;
                            break;
                        }
                    },
                }
                stats.streams += 1;
                stats.leptons += 1;
            }
            T_CRYPT_KEY => {
                let key_in = match cur.flatten() {
                    Some(Plan::CryptKey(k)) => Some(k.as_slice()),
                    _ => None,
                };
                match sealed::code_key(&mut io, key_in) {
                    Some(k) => crypt_key = Some(k),
                    None => {
                        io.damaged = true;
                        break;
                    }
                }
            }
            T_CRYPT => {
                let s_in = match cur.flatten() {
                    Some(Plan::Crypt(s)) => Some(s),
                    _ => None,
                };
                match sealed::code_settings(&mut io, s_in, out.view(), crypt_key.as_deref()) {
                    Some(s) => sealing = Some((s, out.len())),
                    None => {
                        io.damaged = true;
                        break;
                    }
                }
            }
            T_FAX => {
                let src = match cur.flatten() {
                    Some(Plan::Fax { parsed, .. }) => Some(&**parsed),
                    _ => None,
                };
                let fc = fax_coder.get_or_insert_with(|| crate::fax::part::FaxCoder::new(total_len));
                match crate::fax::part::code(&mut io.c, fc, src, out.len(), left) {
                    Some(bytes) => {
                        out.keep_all(&bytes);
                        given = Cow::Owned(bytes);
                    }
                    None => {
                        io.damaged = true;
                        break;
                    }
                }
                stats.streams += 1;
                stats.fax += 1;
            }
            _ => {
                io.damaged = true;
                break;
            }
        }
        // The payload of an encrypted stream is now restored, decrypted: encrypt it again. An empty payload is
        // never stored this way.
        if tag != T_CRYPT {
            if let Some((s, mark)) = sealing.take() {
                let plain = match &out {
                    Restored::Built(v) => &v[mark..],
                    Restored::Counted(..) => &given[..],
                };
                match crypt_key
                    .as_deref()
                    .filter(|_| !plain.is_empty())
                    .and_then(|k| sealed::seal_payload(&s, k, plain))
                {
                    Some(z) => {
                        stats.decrypted += 1;
                        stats.decrypted_bytes += z.len();
                        out.truncate(mark);
                        out.keep_all(&z);
                    }
                    None => {
                        io.damaged = true;
                        break;
                    }
                }
            }
        }
        idle = if out.len() == before { idle + 1 } else { 0 };
        if idle > 2 {
            io.damaged = true;
            break;
        }
        prev = Some(tag);
        si += 1;
    }
    if io.damaged {
        stats.damaged = true;
    }
    stats.timed_out = io.timed_out;
    stats.bypassed = io.m.bypassed;
    let Io { c, .. } = io;
    (out, c)
}

/// Lepton's output: usually entropy-coded already, so coded plainly (repeats are still found); our model
/// when it isn't (an odd JPEG). The encoder decides; the decision is stored. Returns where the data starts
/// in the model's history, which keeps it.
fn lepton_part<C: Coder>(io: &mut Io<C>, src: Option<&[u8]>, n: usize, stats: &mut PackStats) -> usize {
    let flat = io.byte(src.map(|x| looks_compressed(x, true) as u8)) == 1;
    io.m.set_kind(Kind::Raw, Geom::default(), 0);
    io.m.set_flat(flat);
    let at = io.m.hist_len();
    io.skip(src, n);
    io.m.set_flat(false);
    if flat {
        stats.flat_bytes += n;
    }
    at
}

/// Codes a decoded stream payload (shared by the rebuilt kinds): its length, then the data with the model
/// set for what it is; image rows lose their PNG predictor bytes on the way. The encoder takes the data from
/// `feed`, inflated again in pieces as it is coded.
fn payload<C: Coder>(
    io: &mut Io<C>,
    feed: Option<&Feed>,
    lep_in: Option<Option<&[u8]>>,
    out: &[u8],
    stats: &mut PackStats,
) -> Decoded {
    let dlen = io.part(feed.map(|f| f.dlen as u64), MAX_PART);
    // A JPEG behind Flate (as normal-PDF output writes them): Lepton for the JPEG inside.
    if io.byte(lep_in.map(|l| l.is_some() as u8)) == 1 {
        if !HAVE_EXISTING {
            stats.needs_existing = true;
            io.damaged = true;
            return Decoded::Bytes(Vec::new());
        }
        let src = lep_in.flatten();
        let n = io.part(src.map(|x| x.len() as u64), MAX_PART);
        let at = lepton_part(io, src, n, stats);
        stats.leptons += 1;
        stats.decoded_bytes += dlen;
        // The encoder checked when it planned the part that the data gives the JPEG back (`lepton_bytes`).
        if src.is_some() || io.damaged {
            return Decoded::Bytes(Vec::new());
        }
        return match io.m.history(at, n).and_then(unlepton) {
            Some(j) if j.len() == dlen => Decoded::Bytes(j),
            _ => {
                io.damaged = true;
                Decoded::Bytes(Vec::new())
            }
        };
    }
    let dict = dict_before(out, out.len().saturating_sub(1));
    let looks = io.byte(feed.map(|f| f.looks as u8));
    let mut inf = info(dict.as_ref(), Some(if looks == 1 { b"q" as &[u8] } else { b"\x00" as &[u8] }));
    let mut decoded = Decoded::Bytes(Vec::new());
    let mut used_png = false;
    if let Some((row, bpp)) = inf.png {
        // The encoder's first pass: the row types, which must all be known for the rows to be unfiltered.
        let Ok(types_in) = feed.map(|f| f.png_types(row)).transpose() else {
            io.damaged = true;
            return decoded;
        };
        let ok = io.byte(types_in.as_ref().map(|t| t.is_some() as u8));
        if ok == 1 {
            let rows = dlen / (row + 1);
            io.m.set_kind(Kind::Data, Geom::default(), 0);
            let at = io.m.hist_len();
            io.skip(types_in.as_ref().and_then(|t| t.as_deref()), rows);
            io.m.set_kind(Kind::Image, Geom { stride: row, bpp, exact: inf.geom.exact }, 0);
            match feed {
                Some(f) => {
                    if !png_pixels(io, f, row, bpp) {
                        io.damaged = true;
                    }
                }
                None => io.skip(None, rows * row),
            }
            if io.damaged {
                return decoded;
            }
            decoded = Decoded::Png { at, rows, row, bpp };
            used_png = true;
            stats.unpredicted_images += 1;
        }
    }
    if let (false, Some((row, bpp))) = (used_png, inf.tiff) {
        // TIFF differences back to samples (per row, 8-bit), and differenced again on the way out.
        let ok = io.byte(feed.map(|f| (row > 0 && f.dlen.is_multiple_of(row)) as u8));
        if ok == 1 && row > 0 {
            io.m.set_kind(Kind::Image, Geom { stride: row, bpp, exact: false }, 0);
            let at = io.m.hist_len();
            match feed {
                Some(f) => {
                    if !tiff_samples(io, f, row, bpp) {
                        io.damaged = true;
                    }
                }
                None => io.skip(None, dlen),
            }
            decoded = Decoded::Tiff { at, len: dlen, row, bpp };
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
            // object); the data length can. Both sides see the same length and dictionary. In u64: w * c * bpc can
            // pass 2^32, and a 32-bit build must make the same guess as a 64-bit one.
            for c in [comps, 1, 3, 4, 2, 5, 6, 7, 8] {
                let row = (w as u64 * c as u64 * bpc as u64).div_ceil(8);
                if row > 0 && h > 0 && row.checked_mul(h as u64) == Some(dlen as u64) {
                    inf.geom.stride = row as usize;
                    inf.geom.bpp = ((c * bpc) / 8).max(1);
                    break;
                }
            }
        }
        // Decoded data can itself be compressed (a JPEG behind Flate, say); a Type 1 font program is opened
        // instead, never coded plainly (see `PackOptions::effort`).
        let font = type1::is_type1(dict.as_ref());
        let flat = io.byte(feed.map(|f| (inf.png.is_none() && !font && f.plain) as u8)) == 1;
        io.m.set_kind(inf.kind, inf.geom, 0);
        io.m.set_flat(flat);
        if font && !flat {
            // A font program is small: the encoder inflates it whole, as the planner did.
            let font_in = feed.map(|f| f.whole());
            if font_in.as_ref().is_some_and(Option::is_none) {
                io.damaged = true;
            } else {
                let mut made = Vec::new();
                let keep = |x: &[u8]| {
                    if feed.is_none() {
                        made.extend_from_slice(x)
                    }
                };
                type1::code(io, font_in.flatten().as_deref(), dlen, dict.as_ref(), inf.kind, keep, stats);
                decoded = Decoded::Bytes(made);
            }
        } else {
            let at = io.m.hist_len();
            match feed {
                Some(f) => {
                    let done = f.each(&mut |p| {
                        io.skip(Some(p), p.len());
                        !io.damaged
                    });
                    if !done {
                        io.damaged = true;
                    }
                }
                None => io.skip(None, dlen),
            }
            decoded = Decoded::Hist { at, len: dlen };
        }
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

/// Where each object "n g obj" of a file starts its value (the last definition wins), found by its bytes:
/// for guesses that are checked afterwards. Built once per file, when first needed.
type ObjIndex = std::sync::OnceLock<std::collections::HashMap<(u32, u16), usize>>;

fn object_index(d: &[u8]) -> std::collections::HashMap<(u32, u16), usize> {
    let mut out = std::collections::HashMap::new();
    let mut from = 0;
    while let Some(i) = find(d, b" obj", from) {
        from = i + 4;
        // Back over "g" and "n" (digits separated by one space).
        let digits = |end: usize| -> usize {
            let mut s = end;
            while s > 0 && d[s - 1].is_ascii_digit() && end - s < 10 {
                s -= 1;
            }
            s
        };
        let gs = digits(i);
        if gs == i || gs == 0 || d[gs - 1] != b' ' {
            continue;
        }
        let ns = digits(gs - 1);
        if ns == gs - 1 || (ns > 0 && d[ns - 1].is_ascii_alphanumeric()) {
            continue;
        }
        let num = |a: usize, b: usize| -> Option<u64> { std::str::from_utf8(&d[a..b]).ok()?.parse().ok() };
        if let (Some(n), Some(g)) = (num(ns, gs - 1), num(gs, i)) {
            if let (Ok(n), Ok(g)) = (u32::try_from(n), u16::try_from(g)) {
                out.insert((n, g), i + 4);
            }
        }
    }
    out
}

/// Smallest CCITT stream coded as an image (measured on the test files: below this, plain coding wins).
const MIN_FAX: usize = 64;

/// A stream stored as it is in the file whose only filter is CCITTFaxDecode, when our CCITT decoder
/// understands most of it: the image and the encoder's choices, checked to give the very same bytes back.
fn fax(d: &[u8], a: usize, b: usize, objects: &ObjIndex) -> Option<Plan> {
    let dict = dict_before(d, a.checked_sub(1)?)?;
    let parsed = fax_parse(&d[a..b], dict, &file_resolver(d, objects))?;
    Some(Plan::Fax { parsed: Box::new(parsed), at: a, raw: Some((a, b)) })
}

/// Follows a reference to an object of the file by its bytes ("n g obj"), for guesses that are checked
/// afterwards. The index is built once per file, when first needed.
fn file_resolver<'a>(d: &'a [u8], objects: &'a ObjIndex) -> impl Fn(&Obj) -> Option<Obj> + 'a {
    move |o: &Obj| match o {
        Obj::Ref(n, g) => {
            let at = *objects.get_or_init(|| object_index(d)).get(&(*n, *g))?;
            Lexer::new(d, at).parse_obj().ok()
        }
        x => Some(x.clone()),
    }
}

/// The CCITT data `raw` of a stream with dictionary `dict`, parsed for a fax part (`crate::fax::part`), or
/// None when it can't be stored that way. Parameters given by reference (/DecodeParms 13 0 R, /K 7 0 R) go
/// through `resolve`; whatever it finds is only a guess to try, since the result is checked by writing the
/// stream again. Shared by streams stored as they are and by decrypted ones (`sealed`).
pub(super) fn fax_parse(
    raw: &[u8],
    mut dict: Dict,
    resolve: &dyn Fn(&Obj) -> Option<Obj>,
) -> Option<crate::fax::ccitt::Parsed> {
    use crate::fax::{ccitt::Scheme, part, pdf::FaxParams};
    if raw.len() < MIN_FAX {
        return None;
    }
    if let Some(p) = dict.get(b"DecodeParms").cloned() {
        let mut p = resolve(&p)?;
        if let Obj::Dict(pd) = &mut p {
            for (_, v) in pd.0.iter_mut() {
                *v = resolve(v)?;
            }
        }
        dict.set(b"DecodeParms", p);
    }
    let stages = crate::pdf::filters::chain(&dict)?;
    if stages.len() != 1 || stages[0].name != b"CCITTFaxDecode" {
        return None;
    }
    let fp = FaxParams::from(stages[0].parms.as_ref())?;
    part::prepare(raw, Scheme::from_k(fp.k), fp.columns, fp.byte_align)
}

/// Keeps the fax streams of a file within the work budget the restore checks (`crate::fax::part::Work`), in
/// file order; streams past it are coded plainly (a decrypted one as its CCITT bytes, which `prepare` has
/// checked the image and layout give back exactly).
fn fax_budget(mut plans: Vec<Plan>) -> Vec<Plan> {
    let mut work = crate::fax::part::Work::default();
    for p in plans.iter_mut() {
        if let Plan::Fax { parsed, at, raw } = p {
            if !work.admit(parsed, *at) {
                *p = match raw {
                    Some((a, b)) => Plan::Raw(*a, *b),
                    None => Plan::Plain(crate::fax::ccitt::encode(&parsed.image, &parsed.layout)),
                };
            }
        }
    }
    plans
}

/// For a Flate payload that zlib made: (parameters, decoded data, bytes after the zlib stream).
fn rebuild_params(raw: &[u8], max_stream: usize) -> Option<(zlibexact::Params, Vec<u8>, Vec<u8>)> {
    if !zlib_header(raw) {
        return None;
    }
    let u = zlib::unwrap(raw, max_stream).ok()?;
    let used = raw.len() - u.trailing;
    let p = zlibexact::find_params(&u.data, &raw[..used])?;
    Some((p, u.data, raw[used..].to_vec()))
}

pub fn pack(input: &[u8]) -> (Vec<u8>, PackStats) {
    pack_with(input, &PackOptions::default())
}

/// `pack` with the encoder's choices set by `opt`; any file it makes restores with `unpack`.
pub fn pack_with(input: &[u8], opt: &PackOptions) -> (Vec<u8>, PackStats) {
    let (out, stats) = pack_pass(input, opt);
    // Storing encrypted streams decrypted costs the file key and each stream's settings. The encrypted bytes
    // can't be compressed, so a pack well below their size can only win; otherwise the pack without
    // decryption is made too, and the smaller kept (both are restored and checked).
    if stats.decrypted > 0 && out.len() as u64 * 50 > stats.decrypted_bytes as u64 * 49 {
        let (plain, plain_stats) = pack_pass(input, &PackOptions { decrypt: false, ..opt.clone() });
        if plain.len() <= out.len() {
            return (plain, plain_stats);
        }
    }
    (out, stats)
}

/// One pack of `input` with the choices in `opt`, restored and checked.
fn pack_pass(input: &[u8], opt: &PackOptions) -> (Vec<u8>, PackStats) {
    let mut stats = PackStats::default();
    let dbg = std::env::var_os("PDFE_DEBUG").is_some();
    let t = clock::now();
    let threads = if opt.threads == 0 { par::default_threads() } else { opt.threads };
    let (plans, claimed) = plans(input, opt);
    stats.claimed = claimed;
    let mut plans = Some(plans);
    // Never hand over something we can't restore exactly. With a second thread the restore runs while the
    // encoder writes (the same `run`, on the same bytes, as `unpack`); with one, or when the system refuses
    // the thread, it follows.
    let alongside = (threads >= 2).then(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            let check = par::spawn(s, move || {
                let mut st = PackStats::default();
                let (back, _) = run(
                    pipe::StreamDecoder::new(rx),
                    None,
                    None,
                    input.len(),
                    &PackOptions::default(),
                    &mut st,
                    claimed,
                );
                !st.damaged && back.view() == input
            })?;
            let (copy, enc) =
                run(pipe::TeeEncoder::new(tx), Some(input), plans.take(), input.len(), opt, &mut stats, claimed);
            debug_assert!(stats.timed_out || copy.len() == input.len());
            let mut out = header(VERSION, input, claimed);
            out.extend_from_slice(&enc.finish());
            Some((out, check.join().unwrap_or(false)))
        })
    });
    let (out, restored) = match alongside.flatten() {
        Some(r) => r,
        None => {
            let (copy, enc) = run(Encoder::new(), Some(input), plans.take(), input.len(), opt, &mut stats, claimed);
            debug_assert!(stats.timed_out || copy.len() == input.len());
            let mut out = header(VERSION, input, claimed);
            out.extend_from_slice(&enc.finish());
            // The check accepts exactly what this pack declares, whatever the caller's budget was.
            let check_opt = UnpackOptions { max_decoded: claimed, ..UnpackOptions::default() };
            let restored = !stats.timed_out && matches!(unpack_with(&out, &check_opt), Ok(back) if back == input);
            (out, restored)
        }
    };
    if dbg {
        eprintln!("pack: planned, coded and restored in {:?} ({threads} threads)", clock::since(t));
    }
    // Out of time: the file as it is, which restores exactly and costs nothing more.
    if stats.timed_out {
        stats.damaged = false;
        stats.stored = true;
        return (stored(input), stats);
    }
    let out = if restored {
        out
    } else {
        stats.fallback = true;
        fallback(input)
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
    let mut out = header(2, input, 0);
    out.extend_from_slice(input);
    out
}

/// Why an archive wasn't restored. Never a wrong file: every case returns nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnpackError {
    /// The bytes don't start like a gpress archive.
    NotAnArchive,
    /// An archive version this build doesn't read (`VERSION`, 0 and 2 are read).
    UnknownVersion(u8),
    /// The header declares more than `UnpackOptions::max_len` bytes (nothing was decoded for it).
    TooLong { declared: u64, limit: u64 },
    /// The header declares parts claiming more than `UnpackOptions::max_decoded` bytes in all (nothing was
    /// decoded for it).
    TooMuchDecoded { declared: u64, limit: u64 },
    /// The deadline passed before the restore was done (nothing is returned; the file may well be fine).
    TimedOut,
    /// Parts made with the existing library (Lepton), which this build doesn't have.
    NeedsExisting,
    /// Damaged: what is in the file doesn't restore it (the detail says where it was found out).
    Damaged(&'static str),
}

impl std::fmt::Display for UnpackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnpackError::NotAnArchive => write!(f, "not a gpress archive"),
            UnpackError::UnknownVersion(v) => {
                write!(f, "gpress archive version {v} is not one this build reads (it reads version {VERSION})")
            }
            UnpackError::TooLong { declared, limit } => {
                write!(f, "the archive restores to {declared} bytes, more than the limit of {limit}")
            }
            UnpackError::TooMuchDecoded { declared, limit } => {
                write!(f, "the archive's parts claim {declared} decoded bytes, more than the limit of {limit}")
            }
            UnpackError::TimedOut => write!(f, "the deadline passed before the archive was restored"),
            UnpackError::NeedsExisting => write!(
                f,
                "this gpress archive has parts made with the existing library (Lepton); restoring it needs a build with it"
            ),
            UnpackError::Damaged(why) => write!(f, "damaged gpress archive ({why})"),
        }
    }
}

impl std::error::Error for UnpackError {}

/// Restores the original file; checks its length and hash. Takes any file up to `MAX_FILE`; `unpack_with`
/// sets a smaller limit.
pub fn unpack(a: &[u8]) -> Result<Vec<u8>, UnpackError> {
    unpack_with(a, &UnpackOptions::default())
}

/// `unpack` with the decoder's choices in `opt`: a file whose header declares more than `opt.max_len` bytes,
/// or parts claiming more than `opt.max_decoded` decoded bytes in all, is refused before anything is decoded
/// or reserved for it.
pub fn unpack_with(a: &[u8], opt: &UnpackOptions) -> Result<Vec<u8>, UnpackError> {
    use UnpackError::*;
    if a.len() < 6 || &a[..4] != MAGIC {
        return Err(NotAnArchive);
    }
    let ver = a[4];
    if ver != VERSION && ver != 0 && ver != 2 {
        return Err(UnknownVersion(ver));
    }
    let (len, i) = read_varint(a, 5).ok_or(Damaged("bad length"))?;
    let (claimed, i) = read_varint(a, i).ok_or(Damaged("bad decoded total"))?;
    if len > MAX_FILE {
        return Err(Damaged("impossible size"));
    }
    if len > opt.max_len {
        return Err(TooLong { declared: len, limit: opt.max_len });
    }
    if claimed > opt.max_decoded {
        return Err(TooMuchDecoded { declared: claimed, limit: opt.max_decoded });
    }
    let len = usize::try_from(len).map_err(|_| Damaged("too large for this platform"))?;
    let want = a.get(i..i + HASH_LEN).ok_or(Damaged("truncated header"))?.to_vec();
    let body = &a[i + HASH_LEN..];
    let out = match ver {
        VERSION => {
            let mut stats = PackStats::default();
            let dec_opt = PackOptions { deadline: opt.deadline, ..PackOptions::default() };
            let (out, _) = run(Decoder::new(body), None, None, len, &dec_opt, &mut stats, claimed);
            let out = out.into_file();
            if stats.timed_out {
                return Err(TimedOut);
            }
            if stats.needs_existing {
                return Err(NeedsExisting);
            }
            if stats.damaged {
                return Err(Damaged("a part doesn't restore"));
            }
            out
        }
        0 => fallback_decode(body, len).ok_or(Damaged("the data ran out"))?,
        _ => body.to_vec(),
    };
    if out.len() != len || sha256(&out)[..HASH_LEN] != want[..] {
        return Err(Damaged("the restored bytes don't match the checksum"));
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
    let mut out = header(0, input, 0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flate::zlibexact::{zlib_stream, Params};
    use crate::par::tests::RefuseSpawns;

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    #[test]
    fn plain_coding_only_for_data_without_structure() {
        let mut s = 7u64;
        let random: Vec<u8> = (0..100_000).map(|_| rng(&mut s) as u8).collect();
        let text: Vec<u8> = (0..20_000u32)
            .flat_map(|i| format!("BT /F1 {} Tf ({}) Tj ET\n", i % 17, i.wrapping_mul(7919)).into_bytes())
            .collect();
        assert!(plainly(&random, true));
        assert!(!plainly(&random, false), "only when allowed");
        assert!(!plainly(&text, true));
        // Image samples whose byte values are evenly spread, but with neighbours that are alike: an RGB
        // gradient, a ramp, a noisy photo-like picture. Large data is judged on windows.
        let (w, h) = (512usize, 512usize);
        let gradient: Vec<u8> = (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [(x / 2) as u8, (y / 2) as u8, ((x + y) / 4) as u8]))
            .collect();
        let ramp: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
        let photo: Vec<u8> =
            (0..w * h * 3).map(|i| ((i % (w * 3)) / 3 + i / (w * 3) + (rng(&mut s) % 81) as usize) as u8).collect();
        let big = [
            random.as_slice(),
            &gradient,
            &random,
            &random,
            &random,
            &random,
            &random,
            &random,
            &random,
            &random,
            &random,
            &random,
        ]
        .concat();
        for (name, x) in [("gradient", &gradient), ("ramp", &ramp), ("photo", &photo)] {
            assert!(looks_compressed(x, true), "{name}: its bytes alone look compressed");
            assert!(!plainly(x, true), "{name}");
        }
        assert!(big.len() > 16 << 16 && !plainly(&big, true));
        assert!(plainly(&[random.as_slice(); 12].concat(), true));
    }

    /// A small PDF with a content stream zlib wrote (rebuilt exactly) and an image.
    fn sample_pdf() -> Vec<u8> {
        let z = |d: &[u8]| zlib_stream(d, Params { level: 6, window_bits: 15, mem_level: 8, filtered: false });
        let content: Vec<u8> = (0..300)
            .flat_map(|i| format!("BT /F1 12 Tf 72 {} Td (Line {i}) Tj ET\n", 700 - i % 60 * 11).into_bytes())
            .collect();
        let px: Vec<u8> = (0..200 * 100).map(|i| (i % 200 + i / 200) as u8).collect();
        let mut pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        for (n, dict, data) in [(2, "/Filter /FlateDecode", z(&content)), (3, "/Subtype /Image /Width 200 /Height 100 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode", z(&px))] {
            pdf.extend_from_slice(format!("{n} 0 obj\n<< {dict} /Length {} >>\nstream\n", data.len()).as_bytes());
            pdf.extend_from_slice(&data);
            pdf.extend_from_slice(b"\nendstream\nendobj\n");
        }
        pdf.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
        pdf
    }

    /// A pack of the given parts as the encoder would write them, with the header declaring `declared`
    /// decoded bytes whatever the parts claim (the encoder notices afterwards that the parts don't restore
    /// the file, and stops; the pack ends there).
    fn pack_declaring(d: &[u8], plans: Vec<Plan>, declared: u64) -> Vec<u8> {
        let mut stats = PackStats::default();
        let (_, enc) =
            run(Encoder::new(), Some(d), Some(plans), d.len(), &PackOptions::default(), &mut stats, u64::MAX);
        let mut out = header(VERSION, d, declared);
        out.extend_from_slice(&enc.finish());
        out
    }

    /// A zlib stream of `n` zeros as a stream's source, and its decoded length.
    fn zeros_source(n: usize) -> (Source, usize) {
        (Source::Bytes(zlib_of_zeros(n)), n)
    }

    #[test]
    fn parts_beyond_what_the_header_declares_are_refused_at_once() {
        // A 100-byte file whose header declares 1000 decoded bytes. A pack of it that claims a 4 MiB decoded
        // stream (zeros, so it codes to almost nothing) is refused when the length is read, before any of it is
        // decoded: in a few milliseconds and without reserving the memory. The same for Lepton data, and for
        // Lepton data larger than what is left of the file.
        let d =
            b"%PDF-1.4\n1 0 obj\n<< /Length 4 >>\nstream\nABCD\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
        let a = d.windows(4).position(|w| w == b"ABCD").unwrap();
        let params = Params { level: 6, window_bits: 15, mem_level: 8, filtered: false };
        let huge = vec![0u8; 4 << 20];
        let (src, dlen) = zeros_source(4 << 20);
        let (small_src, small_len) = (Source::Bytes(zlibexact::zlib_stream(&[0xFFu8; 2048], params)), 2048);
        let cases: Vec<Vec<Plan>> = vec![
            vec![
                Plan::Syntax(0, a),
                Plan::Rebuilt { params, src, dlen, tail: Vec::new(), lep: None, looks: false, plain: false },
                Plan::Syntax(a + 4, d.len()),
            ],
            vec![
                Plan::Syntax(0, a),
                Plan::Lepton { src: Source::File(a, a + 4), data: huge.clone() },
                Plan::Syntax(a + 4, d.len()),
            ],
            vec![
                Plan::Syntax(0, a),
                Plan::Rebuilt {
                    params,
                    src: small_src,
                    dlen: small_len,
                    tail: Vec::new(),
                    lep: Some(huge.clone()),
                    looks: false,
                    plain: false,
                },
                Plan::Syntax(a + 4, d.len()),
            ],
        ];
        for (k, plans) in cases.into_iter().enumerate() {
            let packed = pack_declaring(d, plans, 1000);
            assert!(packed.len() < 4096, "case {k}: {} bytes", packed.len());
            let t = std::time::Instant::now();
            // The Lepton cases are refused as needing the existing library first in a build without it.
            let err = unpack(&packed).unwrap_err();
            assert!(
                matches!(err, UnpackError::Damaged(_)) || (k > 0 && err == UnpackError::NeedsExisting),
                "case {k}: {err}"
            );
            assert!(t.elapsed().as_millis() < 500, "case {k} took {:?}", t.elapsed());
        }
        // Honest parts of the same file are within what the header declares: nothing here is refused.
        let (packed, st) = pack(d);
        assert!(!st.damaged && !st.fallback);
        assert_eq!(unpack(&packed).unwrap(), &d[..]);
    }

    #[test]
    fn a_declared_total_over_the_limit_is_refused_before_decoding() {
        // The header of a crafted file declares 4 MiB of decoded bytes over a part of that size. Under a limit
        // below that, it is refused by the header alone, at once; at the limit the part is decoded and found
        // not to restore the file (the cost of a crafted file is the budget the caller allowed, no more).
        let d = b"%PDF-1.4\n1 0 obj\n<< /Length 4 >>\nstream\nABCD\nendstream\nendobj\n%%EOF\n";
        let claim = 4u64 << 20;
        let packed = crafted_claim(d, claim);
        assert!(packed.len() < 4096, "{} bytes", packed.len());
        let t = std::time::Instant::now();
        let under = UnpackOptions { max_decoded: claim - 1, ..UnpackOptions::default() };
        assert_eq!(
            unpack_with(&packed, &under).unwrap_err(),
            UnpackError::TooMuchDecoded { declared: claim, limit: claim - 1 }
        );
        assert!(t.elapsed().as_millis() < 50, "{:?}", t.elapsed());
        let at = UnpackOptions { max_decoded: claim, ..UnpackOptions::default() };
        assert_eq!(unpack_with(&packed, &at).unwrap_err(), UnpackError::Damaged("a part doesn't restore"));
        // A header saying 0 over a body whose part claims 4 MiB: refused when the length is read.
        let zero = pack_declaring(d, vec![Plan::Syntax(0, d.len())], 0);
        assert_eq!(unpack(&zero).unwrap(), &d[..], "an honest file declaring 0 restores");
        let (src, dlen) = zeros_source(4 << 20);
        let params = Params { level: 6, window_bits: 15, mem_level: 8, filtered: false };
        let lying = pack_declaring(
            d,
            vec![
                Plan::Syntax(0, d.len()),
                Plan::Rebuilt { params, src, dlen, tail: Vec::new(), lep: None, looks: false, plain: false },
            ],
            0,
        );
        assert_eq!(unpack(&lying).unwrap_err(), UnpackError::Damaged("a part doesn't restore"));
    }

    /// Codes `parts` with `Io::skip` (each with its kind, geometry and plain flag) and checks that each one's
    /// bytes are then in the model's history, where the decoder makes the zlib bytes again from (see
    /// `Decoded`). Returns the coder, whether every part read back, and the bytes coded by long repeats.
    fn skip_parts<C: Coder>(c: C, parts: &[(Kind, Geom, bool, &[u8])], encoding: bool) -> (C, bool, u64) {
        let mut m = primed(16);
        let mut io =
            Io { m: &mut m, c, damaged: false, plain: false, budget: 0, deadline: Deadline::NONE, timed_out: false };
        let mut same = true;
        for &(kind, geom, flat, x) in parts {
            io.m.set_kind(kind, geom, 0);
            io.m.set_flat(flat);
            let at = io.m.hist_len();
            io.skip(encoding.then_some(x), x.len());
            same &= io.m.history(at, x.len()) == Some(x) && io.m.hist_len() == at + x.len();
        }
        same &= io.m.history(io.m.hist_len(), 1).is_none() && io.m.history(usize::MAX, 2).is_none();
        let bypassed = io.m.bypassed;
        let Io { c, .. } = io;
        (c, same && !io.damaged, bypassed)
    }

    #[test]
    fn coded_bytes_are_read_back_from_the_history() {
        // Long repeats (one decision for a byte), plain coding, residual-coded image rows and exact ones: on
        // both sides every byte coded is added to the history once, in order.
        let mut s = 3u64;
        let text = b"BT /F1 12 Tf 72 700 Td (a line said again and again) Tj ET\n".repeat(300);
        let noise: Vec<u8> = (0..5000).map(|_| rng(&mut s) as u8).collect();
        let img: Vec<u8> = (0..60 * 41).map(|i| (i % 60 * 3 + i / 60) as u8 ^ (rng(&mut s) & 3) as u8).collect();
        let parts: [(Kind, Geom, bool, &[u8]); 4] = [
            (Kind::Content, Geom::default(), false, &text),
            (Kind::Raw, Geom::default(), true, &noise),
            (Kind::Image, Geom { stride: 60, bpp: 3, exact: false }, false, &img),
            (Kind::Image, Geom { stride: 60, bpp: 1, exact: true }, false, &img),
        ];
        let (enc, same, bypassed) = skip_parts(Encoder::new(), &parts, true);
        assert!(same && bypassed > 1000, "encoder: {bypassed} bytes by long repeats");
        let coded = enc.finish();
        let (_, same, _) = skip_parts(Decoder::new(&coded), &parts, false);
        assert!(same, "decoder");
    }

    #[test]
    fn a_stream_that_does_not_inflate_again_as_planned_gives_the_pass_up() {
        // The encoder inflates a rebuilt stream again as it codes it. A plan whose decoded length is not the
        // stream's (no planner makes one) is found out on the way, for each way the bytes are coded (as they
        // are, PNG rows, TIFF rows, a Type 1 font): the pass is marked damaged (the pack then falls back),
        // never a panic.
        let text: Vec<u8> = (0..200).flat_map(|i| format!("BT ({i}) Tj ET\n").into_bytes()).collect();
        let rows: Vec<u8> = (0..50).flat_map(|r| [(r % 5) as u8, 1, 2, 3, 4]).collect();
        let params = Params { level: 6, window_bits: 15, mem_level: 8, filtered: false };
        let gray = "/Subtype /Image /Height 50 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode";
        let dicts = [
            ("/Filter /FlateDecode".to_string(), &text),
            (format!("{gray} /Width 4 /DecodeParms << /Predictor 15 /Columns 4 >>"), &rows),
            (format!("{gray} /Width 5 /DecodeParms << /Predictor 2 /Columns 5 >>"), &rows),
            ("/Length1 10 /Length2 20 /Length3 0 /Filter /FlateDecode".to_string(), &text),
        ];
        for (dict, data) in dicts {
            let z = zlib_stream(data, params);
            let mut d = format!("%PDF-1.4\n1 0 obj\n<< {dict} /Length {} >>\nstream\n", z.len()).into_bytes();
            let (a, b) = (d.len(), d.len() + z.len());
            d.extend_from_slice(&z);
            d.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
            let (packed, st) = pack_with(&d, &PackOptions { threads: 1, ..PackOptions::default() });
            let rows_undone = dict.contains("Predictor") as usize;
            assert!(st.rebuilt == 1 && st.unpredicted_images == rows_undone && !st.fallback, "{dict}: {st:?}");
            assert_eq!(unpack(&packed).unwrap(), d);
            // One row less and one row more than the stream holds (whole rows either way).
            for dlen in [data.len() - 5, data.len() + 5] {
                let looks = looks_like_content(data);
                let plan = Plan::Rebuilt {
                    params,
                    src: Source::File(a, b),
                    dlen,
                    tail: Vec::new(),
                    lep: None,
                    looks,
                    plain: false,
                };
                let mut st = PackStats::default();
                let plans = vec![Plan::Syntax(0, a), plan, Plan::Syntax(b, d.len())];
                run(Encoder::new(), Some(&d), Some(plans), d.len(), &PackOptions::default(), &mut st, u64::MAX);
                assert!(st.damaged, "{dict}, {dlen} bytes planned");
            }
        }
    }

    #[test]
    fn window_bits_8_are_refused_as_damaged() {
        // No pack declares window bits 8 for a rebuilt part (`find_params` makes 9 to 15), and the zlib
        // re-creation can't run with so small a window: a file declaring them is refused where the settings
        // are read, never a panic (in a debug build too). 9, the smallest real one, packs and restores.
        let content: Vec<u8> = (0..400).flat_map(|i| format!("BT ({i}) Tj ET\n").into_bytes()).collect();
        let params = Params { level: 6, window_bits: 9, mem_level: 8, filtered: false };
        let z = zlib_stream(&content, params);
        let mut d = format!("%PDF-1.4\n1 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n", z.len()).into_bytes();
        let (a, b) = (d.len(), d.len() + z.len());
        d.extend_from_slice(&z);
        d.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
        let (packed, st) = pack_with(&d, &PackOptions { threads: 1, ..PackOptions::default() });
        assert!(st.rebuilt == 1 && !st.fallback, "{st:?}");
        assert_eq!(unpack(&packed).unwrap(), d);
        let params = Params { window_bits: 8, ..params };
        let src = Source::File(a, b);
        let rebuilt =
            Plan::Rebuilt { params, src, dlen: content.len(), tail: Vec::new(), lep: None, looks: true, plain: false };
        let packed = pack_declaring(&d, vec![Plan::Syntax(0, a), rebuilt, Plan::Syntax(b, d.len())], 1 << 20);
        assert_eq!(unpack(&packed).unwrap_err(), UnpackError::Damaged("a part doesn't restore"));
        // Refused where the settings are read, before any of the stream is decoded: a release build, which
        // doesn't panic, would otherwise fail only later, on the wrapped re-creation.
        let (len, i) = read_varint(&packed, 5).unwrap();
        let (claimed, i) = read_varint(&packed, i).unwrap();
        let mut st = PackStats::default();
        run(Decoder::new(&packed[i + HASH_LEN..]), None, None, len as usize, &PackOptions::default(), &mut st, claimed);
        assert!(st.damaged && st.decoded_bytes == 0 && st.rebuilt == 0, "{st:?}");
    }

    #[test]
    fn the_budget_keeps_decoded_parts_within_it_in_file_order() {
        // Three zlib streams of 300 KB, 200 KB and 100 KB of text (which the model codes far better decoded
        // than as zlib bytes). With a budget of 450 KB the first and the third fit (the second does not and
        // is stored as it is); the header declares 400 KB; the archive restores under a limit of 400 KB and
        // is refused under one of 399 KB. With a budget of 0 every stream is stored as it is, which still
        // restores exactly and is larger.
        let params = Params { level: 9, window_bits: 15, mem_level: 9, filtered: false };
        let text = |size: usize| -> Vec<u8> {
            let mut t = Vec::with_capacity(size + 64);
            for i in 0.. {
                if t.len() >= size {
                    break;
                }
                t.extend_from_slice(
                    format!("BT /F1 {} Tf 72 {} Td (Line {i} of page {}) Tj ET\n", i % 7 + 8, i % 60 * 11, i / 60)
                        .as_bytes(),
                );
            }
            t.truncate(size);
            t
        };
        let mut pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        for (n, size) in [(2, 300_000usize), (3, 200_000), (4, 100_000)] {
            let z = zlib_stream(&text(size), params);
            pdf.extend_from_slice(
                format!("{n} 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n", z.len()).as_bytes(),
            );
            pdf.extend_from_slice(&z);
            pdf.extend_from_slice(b"\nendstream\nendobj\n");
        }
        pdf.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
        let with =
            |max_decoded: u64| pack_with(&pdf, &PackOptions { threads: 1, max_decoded, ..PackOptions::default() });
        let (all, st) = with(DEFAULT_MAX_DECODED);
        assert!(st.rebuilt == 3 && st.claimed == 600_000 && !st.fallback && !st.stored, "{st:?}");
        let (some, st) = with(450_000);
        assert!(st.rebuilt == 2 && st.claimed == 400_000 && !st.fallback && !st.stored, "{st:?}");
        let (none, st) = with(0);
        assert!(st.rebuilt == 0 && st.claimed == 0 && !st.fallback, "{st:?}");
        assert!(all.len() < some.len() && some.len() < none.len(), "{} {} {}", all.len(), some.len(), none.len());
        let limit = |max_decoded: u64| UnpackOptions { max_decoded, ..UnpackOptions::default() };
        assert_eq!(unpack_with(&some, &limit(400_000)).unwrap(), pdf);
        assert_eq!(
            unpack_with(&some, &limit(399_999)).unwrap_err(),
            UnpackError::TooMuchDecoded { declared: 400_000, limit: 399_999 }
        );
        assert_eq!(unpack_with(&none, &limit(0)).unwrap(), pdf);
        assert_eq!(unpack(&all).unwrap(), pdf);
        // The same bytes with two threads (the check runs alongside the encoder).
        let (two, _) = pack_with(&pdf, &PackOptions { threads: 2, max_decoded: 450_000, ..PackOptions::default() });
        assert_eq!(two, some);
    }

    #[test]
    fn a_deadline_stops_a_restore_without_a_wrong_file() {
        let pdf = sample_pdf();
        let (packed, _) = pack_with(&pdf, &PackOptions { threads: 1, ..PackOptions::default() });
        let passed =
            UnpackOptions { deadline: Deadline::after(std::time::Duration::from_nanos(1)), ..UnpackOptions::default() };
        assert_eq!(unpack_with(&packed, &passed).unwrap_err(), UnpackError::TimedOut);
        let ample = UnpackOptions {
            deadline: Deadline::after(std::time::Duration::from_secs(600)),
            ..UnpackOptions::default()
        };
        assert_eq!(unpack_with(&packed, &ample).unwrap(), pdf);
    }

    /// A build without the existing library meets a part it made: the error says what the file needs instead
    /// of calling it damaged, and nothing of the part is decoded first.
    #[cfg(not(feature = "existing"))]
    #[test]
    fn parts_made_with_the_existing_libraries_say_so() {
        let d =
            b"%PDF-1.4\n1 0 obj\n<< /Length 4 >>\nstream\nABCD\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
        let a = d.windows(4).position(|w| w == b"ABCD").unwrap();
        let params = Params { level: 6, window_bits: 15, mem_level: 8, filtered: false };
        let small = Source::Bytes(zlibexact::zlib_stream(&[0xFFu8; 2048], params));
        let cases: Vec<Plan> = vec![
            Plan::Lepton { src: Source::File(a, a + 4), data: vec![1, 2, 3, 4] },
            Plan::Rebuilt {
                params,
                src: small,
                dlen: 2048,
                tail: Vec::new(),
                lep: Some(vec![5, 6, 7, 8]),
                looks: false,
                plain: false,
            },
        ];
        for (k, part) in cases.into_iter().enumerate() {
            let packed = pack_declaring(d, vec![Plan::Syntax(0, a), part, Plan::Syntax(a + 4, d.len())], 4096);
            let err = unpack(&packed).unwrap_err();
            assert_eq!(err, UnpackError::NeedsExisting, "case {k}: {err}");
        }
        assert!(!PackOptions::default().existing);
    }

    #[test]
    fn a_tiny_file_with_a_huge_stream_packs_through_its_decoded_form() {
        // 1 MiB of zeros in a zlib stream of about 1 KB: the decoded part is about 1000 times the file and
        // fits the default budget, so the file packs through its decoded form and restores.
        let zeros = vec![0u8; 1 << 20];
        let z = zlib_stream(&zeros, Params { level: 9, window_bits: 15, mem_level: 9, filtered: false });
        let mut pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        pdf.extend_from_slice(format!("2 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n", z.len()).as_bytes());
        pdf.extend_from_slice(&z);
        pdf.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
        assert!(z.len() < 2048);
        let (packed, st) = pack_with(&pdf, &PackOptions { threads: 1, ..PackOptions::default() });
        assert!(st.rebuilt == 1 && st.claimed == 1 << 20 && !st.fallback && !st.stored, "{st:?}");
        assert_eq!(unpack(&packed).unwrap(), pdf);
    }

    /// A /Length chosen so that `start + length` wraps to the offset of an earlier "endstream" on a 32-bit
    /// target (the WebAssembly build): the sum is checked, so the stream's end comes from the search and
    /// the file packs and restores on every target. (On 64 bits the sum does not wrap; the test keeps the
    /// shape of the file so the build for 32 bits runs it too.)
    #[test]
    fn a_length_that_would_wrap_on_32_bits_is_not_trusted() {
        let head = b"%PDF-1.4\n% endstream before the data\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let early = head.windows(9).position(|w| w == b"endstream").unwrap();
        let payload = b"BT /F1 12 Tf (not a zlib stream) Tj ET";
        let mut d = Vec::new();
        for length in [u32::MAX as i64 + 1, i64::MAX] {
            d.clear();
            d.extend_from_slice(head);
            let obj = format!("2 0 obj\n<< /Length {} >>\nstream\n", length);
            // The data starts at `start`; (start + length) mod 2^32 lands on the early "endstream" when
            // the length is 2^32 - start + early.
            let start = d.len() + obj.len();
            let length = if length == i64::MAX { i64::MAX } else { (1i64 << 32) - start as i64 + early as i64 };
            d.extend_from_slice(format!("2 0 obj\n<< /Length {length} >>\nstream\n").as_bytes());
            assert_eq!(d.len(), start, "the dictionary's length is the same for both lengths tried");
            d.extend_from_slice(payload);
            d.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
            let streams: Vec<(usize, usize)> = segments(&d)
                .iter()
                .filter_map(|s| if let Seg::Stream(a, b) = *s { Some((a, b)) } else { None })
                .collect();
            assert_eq!(streams, vec![(start, start + payload.len())], "length {length}");
            let (packed, st) = pack_with(&d, &PackOptions { threads: 1, ..PackOptions::default() });
            assert!(!st.fallback, "{st:?}");
            assert_eq!(unpack(&packed).unwrap(), d);
        }
    }

    #[test]
    fn a_refused_thread_is_no_loss() {
        let pdf = sample_pdf();
        let (one, st) = pack_with(&pdf, &PackOptions { effort: 100, threads: 1, ..PackOptions::default() });
        assert!(st.rebuilt == 2 && !st.fallback && !st.stored, "{st:?}");
        // No thread at all (the restore check runs after the encoder), or the check thread but no helper.
        for allowed in [0, 1] {
            let _refuse = RefuseSpawns::after(allowed);
            let (two, st) = pack_with(&pdf, &PackOptions { effort: 100, threads: 4, ..PackOptions::default() });
            assert!(two == one, "{allowed} threads allowed");
            assert!(!st.fallback && !st.stored);
        }
        assert_eq!(unpack(&one).unwrap(), pdf);
    }
}
