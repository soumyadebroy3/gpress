//! The peyaji format: any PDF, packed with our own PDF-aware model, restored byte for byte.
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
//! Layout: magic (4) | version (1) | original length (varint) | first 16 bytes of the SHA-256 of the original |
//! coded data. Every part of the coded data starts with a tag byte (T_*); the model's settings, the kinds of
//! parts and how each is coded are constants of the format version, so a pack restores the same anywhere.

use crate::cm::model::{Geom, Kind, Model};
use crate::cm::{Coder, Decoder, Encoder};
use crate::flate::{zlib, zlibexact};
use crate::par;
use crate::pdf::object::{Dict, Obj};
use crate::pdf::parse::Lexer;

pub const MAGIC: &[u8; 4] = b"\x89PYJ";
/// The format version this build writes and reads (plus 0 and 2, the fallback and stored forms). Version 6
/// is the first with Type 1 fonts, fax images and decrypted streams together; nothing earlier shipped, so
/// earlier versions are refused.
pub const VERSION: u8 = 6;
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
    h.iter().filter(|&&c| c > 0).map(|&c| {
        let p = c as f64 / n;
        -p * p.log2()
    }).sum()
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
    pub preflated: usize,
    pub leptons: usize,
    pub fallback: bool,
    pub stored: bool,
    /// Restore only: the file is damaged (it can't be restored; nothing wrong is ever returned).
    pub damaged: bool,
    /// Restore only: the file has parts made with the existing libraries (preflate, Lepton; see
    /// `PackOptions::existing`) and this build has none, so it can't be restored here.
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
                Some(b"DeviceN") => (a.get(1).and_then(|x| x.as_array()).map_or(1, |v| v.len().clamp(1, 32)), false),
                Some(b"Separation") | Some(b"CalGray") => (1, false),
                Some(b"CalRGB") | Some(b"Lab") => (3, false),
                // The real count is in the ICC stream (/N); the data length confirms it below.
                _ => (3, false),
            },
            _ => (1, false),
        };
        let row = w.saturating_mul(comps * bpc).div_ceil(8);
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
                let prow = cols.saturating_mul(colors * pbpc).div_ceil(8);
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
    /// Encoder only: code compressed-looking data plainly (see `PackOptions`).
    plain: bool,
    /// Bytes the parts of this file may still claim beyond what they restore (decoded streams, corrections,
    /// Lepton data): see `decode_budget`. A damaged file that claims more is refused where it does.
    budget: u64,
}

/// Largest single part a file may declare (decoded stream, corrections, Lepton data): 2 GiB.
const MAX_PART: usize = 1 << 31;
/// Largest file the format restores: 16 GiB (a u64, so the limit is the same on 32-bit targets, where a
/// file that doesn't fit in memory is refused by the header alone).
const MAX_FILE: u64 = 1 << 34;
/// Most bytes a Flate stream can decode to per byte of the stream (a 258-byte match in 2 bits), so the most
/// the decoded parts of a file can add up to per byte of the file: an honest pack never claims more, and a
/// damaged or crafted one can't make the decoder work, or reserve memory, beyond that.
const MAX_EXPANSION: u64 = 1032;
/// Memory reserved up front is capped; vectors grow with what is actually decoded.
fn reserve(n: usize) -> usize {
    n.min(1 << 22)
}

/// The decoded bytes a file of `total_len` bytes may claim in all: `MAX_EXPANSION` per byte for decoded
/// streams, one more each for Lepton data and corrections (each smaller than its stream), plus a little for
/// the headers of tiny files.
fn decode_budget(total_len: usize) -> u64 {
    (total_len as u64).saturating_mul(MAX_EXPANSION + 2).saturating_add(1 << 16)
}

impl<'m, C: Coder> Io<'m, C> {
    fn byte(&mut self, b: Option<u8>) -> u8 {
        self.m.code_byte(&mut self.c, b)
    }
    /// The length of a part that isn't restored as it is (a decoded stream, corrections, Lepton data): at
    /// most `max`, and within what the file may still claim.
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

/// Choices of the encoder. The decoder needs none of them: what they decide is stored in the file, so a
/// file restores the same whatever they were (a file with parts made by the existing libraries needs a
/// build that has them, see `existing`).
#[derive(Clone, Debug)]
pub struct PackOptions {
    /// How hard to try, 0 to 100. Below `PLAIN_BELOW`, data that looks compressed already (no structure in
    /// its bytes or their differences, see `looks_compressed`) is coded plainly: on 3 MB of random bytes 17
    /// times faster (0.8 s instead of 14 s) and no larger; up to about 1% larger on compressed data the model
    /// still finds a little in. A Type 1 font program is never coded plainly: encrypted it looks random, but
    /// opened (see `type1`) it is text and charstrings the model codes to a third or less.
    pub effort: u8,
    /// Threads for looking at the streams (0: what the machine offers, at most 8). Never changes the output.
    pub threads: usize,
    /// Store the encrypted streams of a file the empty password opens decrypted, so the usual plan applies
    /// to them (see `sealed`); they are encrypted again exactly on restore. When the pack isn't clearly
    /// smaller than the encrypted bytes, the pack without decryption is made too and the smaller kept.
    pub decrypt: bool,
    /// Largest decoded stream stored in its decoded form; a Flate stream that decodes to more is stored as
    /// it is. (The decoder takes up to `MAX_PART` per part and `MAX_EXPANSION` bytes per byte of the file
    /// in all, whatever this was.)
    pub max_stream: usize,
    /// Try the existing libraries (preflate, Lepton) when the build includes them (`--features existing`).
    /// A file with parts they made restores only with such a build (`unpack` says so); the default is what
    /// the build has.
    pub existing: bool,
}

impl Default for PackOptions {
    fn default() -> Self {
        PackOptions { effort: 100, threads: 0, decrypt: true, max_stream: 1 << 28, existing: HAVE_EXISTING }
    }
}

/// Whether this build has the existing libraries (preflate, Lepton).
const HAVE_EXISTING: bool = cfg!(feature = "existing");

/// Choices of the decoder.
#[derive(Clone, Debug)]
pub struct UnpackOptions {
    /// Largest file restored, in bytes: a file whose header declares more is refused before anything is
    /// decoded. The decoder's work and memory follow the declared length (it is the one bound a crafted
    /// file can't escape), so a service that takes files from anyone sets this to what it can afford. At
    /// most `MAX_FILE` either way.
    pub max_len: u64,
}

impl Default for UnpackOptions {
    fn default() -> Self {
        UnpackOptions { max_len: MAX_FILE }
    }
}

/// Below this effort, compressed-looking data is coded plainly.
const PLAIN_BELOW: u8 = 50;

const T_SYNTAX: u8 = 0;
const T_RAW: u8 = 1;
const T_REBUILT: u8 = 2;
const T_PREFLATED: u8 = 3;
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

#[cfg_attr(not(feature = "existing"), allow(dead_code))]
enum Plan {
    Syntax(usize, usize),
    Raw(usize, usize),
    /// `lep`: the decoded data is a JPEG, recompressed with Lepton (when that beats the Flate bytes).
    Rebuilt { params: zlibexact::Params, data: Vec<u8>, tail: Vec<u8>, lep: Option<Vec<u8>> },
    /// Rebuilt with the corrections an existing library (preflate) records for non-zlib encoders.
    Preflated { header: [u8; 2], corrections: Vec<u8>, data: Vec<u8>, adler_ok: bool, rest: Vec<u8>, lep: Option<Vec<u8>> },
    /// A JPEG recompressed with an existing library (Lepton), restored bit for bit.
    Lepton { data: Vec<u8> },
    /// An encrypted stream's decrypted bytes that nothing rebuilds (coded like a raw stream).
    Plain(Vec<u8>),
    /// The file key of an encrypted file, before its first encrypted stream.
    CryptKey(Vec<u8>),
    /// An encrypted stream's settings; the next part is its decrypted payload.
    Crypt(sealed::Sealed),
    /// A CCITT fax stream: the image and every choice of its encoder (see `crate::fax::part`), the byte of the
    /// file its data starts at (the work budget counts from there), and where its bytes are when they are
    /// stored as they are (None: an encrypted stream's decrypted bytes, see `sealed`).
    Fax { parsed: Box<crate::fax::ccitt::Parsed>, at: usize, raw: Option<(usize, usize)> },
}

/// What to do with each part of the file. The streams are looked at in parallel (the biggest first); the
/// plans keep the file's order.
fn plan(d: &[u8], opt: &PackOptions) -> Vec<Plan> {
    let t = std::time::Instant::now();
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
        Seg::Stream(a, b) => stream_plan(&d[a..b], opt).or_else(|| fax(d, a, b, &objects)).unwrap_or(Plan::Raw(a, b)),
    });
    if std::env::var_os("PDFE_DEBUG").is_some() {
        eprintln!("pack: {} parts planned in {:?}", plans.len(), t.elapsed());
    }
    plans
}

/// The plan for a stream's bytes, wherever they come from (as stored in the file, or an encrypted stream's
/// bytes decrypted): a zlib rebuild, preflate or Lepton, or None when none of them applies. Every way of
/// storing a stream's bytes other than as they are goes through here, so a decrypted stream gets the same
/// choices as a plain one.
fn stream_plan(raw: &[u8], opt: &PackOptions) -> Option<Plan> {
    if let Some((params, data, tail)) = rebuild_params(raw, opt.max_stream) {
        let lep = if opt.existing { nested_lepton(&data, raw.len()) } else { None };
        return Some(Plan::Rebuilt { params, data, tail, lep });
    }
    if !opt.existing {
        return None;
    }
    preflated(raw, opt.max_stream).or_else(|| lepton(raw))
}

/// The plans of a file with the encoder's choices in `opt`: `plan`, with the encrypted streams of a file the
/// empty password opens decrypted when `opt.decrypt` (see `sealed`).
fn plans(d: &[u8], opt: &PackOptions) -> Vec<Plan> {
    let p = plan(d, opt);
    let p = if opt.decrypt { sealed::crypt_plans(d, p, opt) } else { p };
    fax_budget(p)
}

/// For measuring only: the file with every stream we can rebuild replaced by its decoded bytes (Type 1 fonts
/// opened) and the JPEGs Lepton takes left out, plus the size of what a restore needs on top (rebuild
/// settings, preflate corrections, Lepton data). A general compressor on the first, plus the second, is the
/// fair "decompress first, then compress" baseline for the peyaji format: the same preparation, without our
/// model.
pub fn expand(d: &[u8]) -> (Vec<u8>, usize) {
    // Type 1 font programs are opened, as in a pack.
    fn put(out: &mut Vec<u8>, data: &[u8]) -> usize {
        let (x, extra) = type1::expanded(out, data);
        out.extend_from_slice(&x);
        extra
    }
    let mut out = Vec::with_capacity(d.len() * 2);
    let mut extra = 0;
    for p in plans(d, &PackOptions::default()) {
        match p {
            Plan::Syntax(a, b) => out.extend_from_slice(&d[a..b]),
            Plan::Raw(a, b) => extra += put(&mut out, &d[a..b]),
            Plan::Plain(v) => extra += put(&mut out, &v),
            // The key once; per stream its settings and initialization vector.
            Plan::CryptKey(k) => extra += 1 + k.len(),
            Plan::Crypt(_) => extra += 18,
            Plan::Rebuilt { data, tail, lep, .. } => {
                match lep {
                    Some(l) => extra += l.len(),
                    None => extra += put(&mut out, &data),
                }
                out.extend_from_slice(&tail);
                extra += 2;
            }
            Plan::Preflated { corrections, data, rest, lep, .. } => {
                match lep {
                    Some(l) => extra += l.len(),
                    None => extra += put(&mut out, &data),
                }
                out.extend_from_slice(&rest);
                extra += corrections.len();
            }
            Plan::Lepton { data } => extra += data.len(),
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
fn preflated(raw: &[u8], max_stream: usize) -> Option<Plan> {
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
    if plain.text().len() > max_stream {
        return None;
    }
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
fn preflated(_raw: &[u8], _max_stream: usize) -> Option<Plan> {
    None
}

#[cfg(feature = "existing")]
fn lepton_features() -> lepton_jpeg::EnabledFeatures {
    let mut f = lepton_jpeg::EnabledFeatures::compat_lepton_vector_write();
    f.max_processor_threads = 1;
    f
}

#[cfg(feature = "existing")]
fn lepton(raw: &[u8]) -> Option<Plan> {
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
fn lepton(_raw: &[u8]) -> Option<Plan> {
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

/// Bytes of the checksum stored with a preflate part.
const PREFLATE_SUM_LEN: usize = 4;

/// The checksum of a preflate part's corrections and decoded data (the first bytes of their SHA-256).
fn preflate_checksum(corrections: &[u8], data: &[u8]) -> [u8; PREFLATE_SUM_LEN] {
    let mut all = Vec::with_capacity(corrections.len() + data.len());
    all.extend_from_slice(corrections);
    all.extend_from_slice(data);
    let h = sha256(&all);
    let mut out = [0u8; PREFLATE_SUM_LEN];
    out.copy_from_slice(&h[..PREFLATE_SUM_LEN]);
    out
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
/// model sees exactly the same sequence of events. `opt` only guides the encoder's choices, which are stored
/// (among them whether encrypted streams are stored decrypted, see `sealed`).
fn run<C: Coder>(c: C, d: Option<&[u8]>, total_len: usize, opt: &PackOptions, stats: &mut PackStats) -> (Vec<u8>, C) {
    let plans = d.map(|d| plans(d, opt));
    run_plans(c, d, plans, total_len, opt, stats)
}

fn run_plans<C: Coder>(c: C, d: Option<&[u8]>, plans: Option<Vec<Plan>>, total_len: usize, opt: &PackOptions, stats: &mut PackStats) -> (Vec<u8>, C) {
    run_within(c, d, plans, total_len, opt, stats, decode_budget(total_len))
}

/// `run_plans` with the decode budget given (tests write parts beyond it to see them refused).
fn run_within<C: Coder>(c: C, d: Option<&[u8]>, plans: Option<Vec<Plan>>, total_len: usize, opt: &PackOptions, stats: &mut PackStats, budget: u64) -> (Vec<u8>, C) {
    let mut model = primed(table_bits(total_len));
    let mut io = Io { m: &mut model, c, damaged: false, plain: opt.effort < PLAIN_BELOW, budget };
    let mut out: Vec<u8> = Vec::with_capacity(reserve(total_len));
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
            Some(Plan::Preflated { .. }) => T_PREFLATED,
            Some(Plan::Lepton { .. }) => T_LEPTON,
            Some(Plan::CryptKey(..)) => T_CRYPT_KEY,
            Some(Plan::Crypt(..)) => T_CRYPT,
            Some(Plan::Fax { .. }) => T_FAX,
        });
        if io.damaged || out.len() > total_len || io.c.exhausted() {
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
            T_RAW | T_REBUILT | T_PREFLATED | T_LEPTON | T_FAX => prev != Some(T_CRYPT_KEY),
            _ => sealing.is_none() && prev != Some(T_CRYPT_KEY),
        };
        if !in_order {
            io.damaged = true;
            break;
        }
        let before = out.len();
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
                let font = if tag == T_RAW { type1::raw_font_dict(&out) } else { None };
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
                    (Some(dict), false) => type1::code(&mut io, src, n, Some(&dict), Kind::Raw, &mut out, stats),
                    _ => io.bytes(src, n, &mut out),
                }
                io.m.set_flat(false);
            }
            T_REBUILT => {
                let (p_in, data_in, tail_in, lep_in) = match cur.flatten() {
                    Some(Plan::Rebuilt { params, data, tail, lep }) => (Some(*params), Some(data.as_slice()), Some(tail.as_slice()), Some(lep.as_deref())),
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
            // Parts the existing libraries made restore only where they are (see `PackOptions::existing`).
            T_PREFLATED | T_LEPTON if !HAVE_EXISTING => {
                stats.needs_existing = true;
                io.damaged = true;
                break;
            }
            T_PREFLATED => {
                let cur_p = match cur.flatten() {
                    Some(Plan::Preflated { header, corrections, data, adler_ok, rest, lep }) => Some((*header, corrections.as_slice(), data.as_slice(), *adler_ok, rest.as_slice(), lep.as_deref())),
                    _ => None,
                };
                let h0 = io.byte(cur_p.map(|p| p.0[0]));
                let h1 = io.byte(cur_p.map(|p| p.0[1]));
                // The corrections are at most a third of the stream (see `preflated`), so never more than
                // what is left of the file.
                let clen = io.part(cur_p.map(|p| p.1.len() as u64), left);
                io.m.set_kind(Kind::Data, Geom::default(), 0);
                let mut corrections = Vec::with_capacity(reserve(clen));
                io.bytes(cur_p.map(|p| p.1), clen, &mut corrections);
                io.m.set_kind(Kind::Meta, Geom::default(), 0);
                let adler_ok = io.byte(cur_p.map(|p| p.3 as u8)) == 1;
                let rlen = io.length(cur_p.map(|p| p.4.len() as u64), left);
                let mut rest = Vec::with_capacity(reserve(rlen));
                io.bytes(cur_p.map(|p| p.4), rlen, &mut rest);
                // A checksum of the corrections and the decoded data, checked before they go to the existing
                // library: it isn't ours, and damaged corrections can keep it busy without end.
                let sum_in = cur_p.map(|p| preflate_checksum(p.1, p.2));
                let mut sum = Vec::with_capacity(PREFLATE_SUM_LEN);
                io.bytes(sum_in.as_ref().map(|x| &x[..]), PREFLATE_SUM_LEN, &mut sum);
                let decoded = payload(&mut io, cur_p.map(|p| p.2), cur_p.map(|p| p.5), &out, stats);
                if io.damaged || sum[..] != preflate_checksum(&corrections, &decoded)[..] {
                    io.damaged = true;
                    break;
                }
                // The stream must fit in what is left of the file, with its header and trailer.
                let z = match recreate(&decoded, &corrections) {
                    Some(z) if z.len() <= left.saturating_sub(2 + rest.len() + if adler_ok { 4 } else { 0 }) => z,
                    _ => {
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
                // Lepton's data is smaller than the JPEG it restores (see `lepton_bytes`), so never more than
                // what is left of the file.
                let n = io.part(src.map(|x| x.len() as u64), left);
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
                match sealed::code_settings(&mut io, s_in, &out, crypt_key.as_deref()) {
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
                    Some(bytes) => out.extend_from_slice(&bytes),
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
        // The payload of an encrypted stream is now in `out`, decrypted: encrypt it again. An empty payload is
        // never stored this way.
        if tag != T_CRYPT {
            if let Some((s, mark)) = sealing.take() {
                let plain = out.split_off(mark);
                match crypt_key.as_deref().filter(|_| !plain.is_empty()).and_then(|k| sealed::seal_payload(&s, k, &plain)) {
                    Some(z) => {
                        stats.decrypted += 1;
                        stats.decrypted_bytes += z.len();
                        out.extend_from_slice(&z);
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
fn payload<C: Coder>(io: &mut Io<C>, data_in: Option<&[u8]>, lep_in: Option<Option<&[u8]>>, out: &[u8], stats: &mut PackStats) -> Vec<u8> {
    let dlen = io.part(data_in.map(|x| x.len() as u64), MAX_PART);
    // A JPEG behind Flate (as normal-PDF output writes them): Lepton for the JPEG inside.
    if io.byte(lep_in.map(|l| l.is_some() as u8)) == 1 {
        if !HAVE_EXISTING {
            stats.needs_existing = true;
            io.damaged = true;
            return Vec::new();
        }
        let src = lep_in.flatten();
        let n = io.part(src.map(|x| x.len() as u64), MAX_PART);
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
        // Decoded data can itself be compressed (a JPEG behind Flate, say); a Type 1 font program is opened
        // instead, never coded plainly (see `PackOptions::effort`).
        let font = type1::is_type1(dict.as_ref());
        let flat = io.byte(data_in.map(|x| (inf.png.is_none() && !font && plainly(x, io.plain)) as u8)) == 1;
        io.m.set_kind(inf.kind, inf.geom, 0);
        io.m.set_flat(flat);
        if font && !flat {
            type1::code(io, data_in, dlen, dict.as_ref(), inf.kind, &mut decoded, stats);
        } else {
            io.bytes(data_in, dlen, &mut decoded);
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
    raw.len() >= 8 && raw[0] & 0x0f == 8 && raw[0] >> 4 <= 7 && ((raw[0] as u16) << 8 | raw[1] as u16).is_multiple_of(31) && raw[1] & 0x20 == 0
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
pub(super) fn fax_parse(raw: &[u8], mut dict: Dict, resolve: &dyn Fn(&Obj) -> Option<Obj>) -> Option<crate::fax::ccitt::Parsed> {
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
    let t = std::time::Instant::now();
    let threads = if opt.threads == 0 { par::default_threads() } else { opt.threads };
    // Never hand over something we can't restore exactly. With a second thread the restore runs while the
    // encoder writes (the same `run`, on the same bytes, as `unpack`); with one, or when the system refuses
    // the thread, it follows.
    let alongside = (threads >= 2).then(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            let check = par::spawn(s, move || {
                let mut st = PackStats::default();
                let (back, _) = run(pipe::StreamDecoder::new(rx), None, input.len(), &PackOptions::default(), &mut st);
                !st.damaged && back == input
            })?;
            let (copy, enc) = run(pipe::TeeEncoder::new(tx), Some(input), input.len(), opt, &mut stats);
            debug_assert_eq!(copy, input);
            let mut out = header(VERSION, input);
            out.extend_from_slice(&enc.finish());
            Some((out, check.join().unwrap_or(false)))
        })
    });
    let (out, restored) = match alongside.flatten() {
        Some(r) => r,
        None => {
            let (copy, enc) = run(Encoder::new(), Some(input), input.len(), opt, &mut stats);
            debug_assert_eq!(copy, input);
            let mut out = header(VERSION, input);
            out.extend_from_slice(&enc.finish());
            let restored = matches!(unpack(&out), Ok(back) if back == input);
            (out, restored)
        }
    };
    if dbg {
        eprintln!("pack: planned, coded and restored in {:?} ({threads} threads)", t.elapsed());
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
    let mut out = header(2, input);
    out.extend_from_slice(input);
    out
}

/// Restores the original file; checks its length and hash. Takes any file up to `MAX_FILE`; `unpack_with`
/// sets a smaller limit.
pub fn unpack(a: &[u8]) -> Result<Vec<u8>, String> {
    unpack_with(a, &UnpackOptions::default())
}

/// `unpack` with the decoder's choices in `opt`: a file whose header declares more than `opt.max_len` bytes
/// is refused before anything is decoded or reserved for it.
pub fn unpack_with(a: &[u8], opt: &UnpackOptions) -> Result<Vec<u8>, String> {
    if a.len() < 6 || &a[..4] != MAGIC {
        return Err("not a peyaji file".into());
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
    if len > MAX_FILE {
        return Err("damaged peyaji file (impossible size)".into());
    }
    if len > opt.max_len {
        return Err(format!("the file restores to {len} bytes, more than the limit of {}", opt.max_len));
    }
    let len = usize::try_from(len).map_err(|_| "file too large for this platform")?;
    let want = a.get(i..i + HASH_LEN).ok_or("truncated header")?.to_vec();
    let body = &a[i + HASH_LEN..];
    let out = match ver {
        VERSION => {
            let mut stats = PackStats::default();
            let (out, _) = run(Decoder::new(body), None, len, &PackOptions::default(), &mut stats);
            if stats.needs_existing {
                return Err("this peyaji file has parts made with the existing libraries (preflate, Lepton); restoring it needs a build with them".into());
            }
            if stats.damaged {
                return Err("damaged peyaji file".into());
            }
            out
        }
        0 => fallback_decode(body, len).ok_or("damaged peyaji file")?,
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
        let text: Vec<u8> = (0..20_000u32).flat_map(|i| format!("BT /F1 {} Tf ({}) Tj ET\n", i % 17, i.wrapping_mul(7919)).into_bytes()).collect();
        assert!(plainly(&random, true));
        assert!(!plainly(&random, false), "only when allowed");
        assert!(!plainly(&text, true));
        // Image samples whose byte values are evenly spread, but with neighbours that are alike: an RGB
        // gradient, a ramp, a noisy photo-like picture. Large data is judged on windows.
        let (w, h) = (512usize, 512usize);
        let gradient: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [(x / 2) as u8, (y / 2) as u8, ((x + y) / 4) as u8])).collect();
        let ramp: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
        let photo: Vec<u8> = (0..w * h * 3).map(|i| ((i % (w * 3)) / 3 + i / (w * 3) + (rng(&mut s) % 81) as usize) as u8).collect();
        let big = [random.as_slice(), &gradient, &random, &random, &random, &random, &random, &random, &random, &random, &random, &random].concat();
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
        let content: Vec<u8> = (0..300).flat_map(|i| format!("BT /F1 12 Tf 72 {} Td (Line {i}) Tj ET\n", 700 - i % 60 * 11).into_bytes()).collect();
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

    /// A pack of the given parts as the encoder would write them, but with no limit on what they claim (the
    /// encoder notices afterwards that the parts don't restore the file, and stops; the pack ends there).
    fn pack_unbounded(d: &[u8], plans: Vec<Plan>) -> Vec<u8> {
        let mut stats = PackStats::default();
        let (_, enc) = run_within(Encoder::new(), Some(d), Some(plans), d.len(), &PackOptions::default(), &mut stats, u64::MAX);
        let mut out = header(VERSION, d);
        out.extend_from_slice(&enc.finish());
        out
    }

    #[test]
    fn parts_beyond_what_the_file_can_hold_are_refused_at_once() {
        // A 100-byte file: its decoded parts may add up to about 170 KB (MAX_EXPANSION per byte and a little).
        // A pack of it that claims a 4 MiB decoded stream (zeros, so it codes to almost nothing) is refused
        // when the length is read, before any of it is decoded: in a few milliseconds and without reserving
        // the memory. The same for Lepton data or corrections larger than what is left of the file.
        let d = b"%PDF-1.4\n1 0 obj\n<< /Length 4 >>\nstream\nABCD\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
        let a = d.windows(4).position(|w| w == b"ABCD").unwrap();
        assert!(decode_budget(d.len()) < 4 << 20);
        let params = Params { level: 6, window_bits: 15, mem_level: 8, filtered: false };
        let huge = vec![0u8; 4 << 20];
        let cases: Vec<Vec<Plan>> = vec![
            vec![Plan::Syntax(0, a), Plan::Rebuilt { params, data: huge.clone(), tail: Vec::new(), lep: None }, Plan::Syntax(a + 4, d.len())],
            vec![Plan::Syntax(0, a), Plan::Lepton { data: huge.clone() }, Plan::Syntax(a + 4, d.len())],
            vec![Plan::Syntax(0, a), Plan::Rebuilt { params, data: vec![0xFFu8; 2048], tail: Vec::new(), lep: Some(huge.clone()) }, Plan::Syntax(a + 4, d.len())],
            vec![Plan::Syntax(0, a), Plan::Preflated { header: [0x78, 0x9c], corrections: huge.clone(), data: vec![1u8; 16], adler_ok: false, rest: Vec::new(), lep: None }, Plan::Syntax(a + 4, d.len())],
        ];
        for (k, plans) in cases.into_iter().enumerate() {
            let packed = pack_unbounded(d, plans);
            assert!(packed.len() < 4096, "case {k}: {} bytes", packed.len());
            let t = std::time::Instant::now();
            assert!(unpack(&packed).is_err(), "case {k}");
            assert!(t.elapsed().as_millis() < 500, "case {k} took {:?}", t.elapsed());
        }
        // Honest parts of the same file are within the budget: nothing here is refused.
        let (packed, st) = pack(d);
        assert!(!st.damaged && !st.fallback);
        assert_eq!(unpack(&packed).unwrap(), &d[..]);
    }

    /// A build without the existing libraries meets a part they made: the error says what the file needs
    /// instead of calling it damaged, and nothing of the part is decoded first.
    #[cfg(not(feature = "existing"))]
    #[test]
    fn parts_made_with_the_existing_libraries_say_so() {
        let d = b"%PDF-1.4\n1 0 obj\n<< /Length 4 >>\nstream\nABCD\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
        let a = d.windows(4).position(|w| w == b"ABCD").unwrap();
        let params = Params { level: 6, window_bits: 15, mem_level: 8, filtered: false };
        let cases: Vec<Plan> = vec![
            Plan::Preflated { header: [0x78, 0x9c], corrections: vec![1, 2, 3], data: b"ABCD".to_vec(), adler_ok: false, rest: Vec::new(), lep: None },
            Plan::Lepton { data: vec![1, 2, 3, 4] },
            Plan::Rebuilt { params, data: vec![0xFFu8; 2048], tail: Vec::new(), lep: Some(vec![5, 6, 7, 8]) },
        ];
        for (k, part) in cases.into_iter().enumerate() {
            let packed = pack_unbounded(d, vec![Plan::Syntax(0, a), part, Plan::Syntax(a + 4, d.len())]);
            let err = unpack(&packed).unwrap_err();
            assert!(err.contains("existing libraries"), "case {k}: {err}");
        }
        assert!(!PackOptions::default().existing);
    }

    #[test]
    fn the_budget_fits_what_flate_can_hold() {
        // 1 MiB of zeros in a zlib stream of about 1 KB: the decoded part is about 1000 times the file and
        // fits the budget, so the file packs through its decoded form and restores.
        let zeros = vec![0u8; 1 << 20];
        let z = zlib_stream(&zeros, Params { level: 9, window_bits: 15, mem_level: 9, filtered: false });
        let mut pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        pdf.extend_from_slice(format!("2 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n", z.len()).as_bytes());
        pdf.extend_from_slice(&z);
        pdf.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
        assert!(z.len() < 2048 && pdf.len() as u64 * MAX_EXPANSION > zeros.len() as u64);
        let (packed, st) = pack_with(&pdf, &PackOptions { threads: 1, ..PackOptions::default() });
        assert!(st.rebuilt == 1 && !st.fallback && !st.stored, "{st:?}");
        assert_eq!(unpack(&packed).unwrap(), pdf);
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
