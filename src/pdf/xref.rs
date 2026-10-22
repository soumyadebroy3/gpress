//! Cross-reference tables and streams, and loading every object of a document — strictly.

use super::crypt::Crypt;
use super::filters::{chain, decode_stage, predictor, unpredict, FilterError};
use super::object::*;
use super::parse::{parse_indirect, Lexer};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    NoHeader,
    NoStartXref,
    BadXref(&'static str),
    BadObject(u32, &'static str),
    Missing(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Entry {
    Free,
    Offset(usize, u16),
    InStream(u32, u32),
}

/// One entry of an object stream's header, kept because the cross-reference data points that far: the object's
/// number, where it starts in the decoded data (`PAST` when that is beyond it), where its window ends (where
/// the next entry starts, of the ones kept and the first one after them, or the end of the data), whether an
/// entry listed after it in the header starts at the same place, and whether it has been read. An object is
/// parsed within its own bytes only, so the objects of a stream take one pass over it.
#[derive(Clone, Copy)]
struct Head {
    num: u32,
    at: u32,
    end: u32,
    shared: bool,
    read: bool,
}

/// `Head::at` of an entry whose offset is past the decoded data.
const PAST: u32 = u32::MAX;

/// What one header entry kept counts for against `Limits::held` (a `Head`; the same on every platform).
const HEAD_COST: u64 = 16;

/// Why an object stream is refused: past the per-stream limit, past what may be held at once, past the budget.
const TOO_LARGE: &str = "object stream past the stream limit";
const HELD_TOO_MUCH: &str = "object streams held at once past their limit";
const OVER_BUDGET: &str = "object streams past their decoding budget";

/// Why an object of an object stream is refused: it runs into the next one's bytes, or the entry after it
/// starts at the same place.
const RUNS_ON: &str = "object runs into the next one in its object stream";
const SHARED: &str = "object stream lists two objects at one offset";

/// Is this an object of an object stream refused for the limits or the rules above (rather than unreadable)?
fn is_refusal(e: &LoadError) -> bool {
    matches!(e, LoadError::BadObject(_, why) if [TOO_LARGE, HELD_TOO_MUCH, OVER_BUDGET, RUNS_ON, SHARED].contains(why))
}

/// A decoded object stream: its data, its header as far as the cross-reference data points, how many rows of
/// the cross-reference data point into it and have not been read yet, and what it counts for against
/// `Limits::held`.
struct ObjStm {
    raw: Vec<u8>,
    heads: Vec<Head>,
    left: usize,
    cost: u64,
}

/// The most an object stream decodes to, at each filter stage, whatever the limits.
pub const OBJSTM_MAX: usize = 1 << 28;

/// With `Limits::once`, the object streams held at once take at most this many times the per-stream limit.
pub const HELD_STREAMS: u64 = 4;

/// With `Limits::once`, the object streams of a file decode to at most this many bytes per byte of the file in
/// all, their stored bytes and every filter stage counted, or what they may hold at once when that is more: the
/// time follows the file's size, not the number of its object streams.
pub const OBJSTM_PER_BYTE: u64 = 64;

/// What reading object streams may take (see `open_with`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes an object stream may decode to, at each filter stage.
    pub objstm: usize,
    /// None: an object stream decoded is kept until `drop_cache`, for callers that read objects in any order
    /// and as often as they like. Some(cap): each object stream is decoded at most once (one read while an
    /// encrypted file is opened, once more after its key is complete), and let go as soon as
    /// every row of the cross-reference data that points into it has been read; the ones held at once take at
    /// most `cap` bytes, header entries counted, and all of them, stored bytes and every filter stage counted,
    /// decode to at most the larger of `cap` and `OBJSTM_PER_BYTE` per byte of the file. An integer read from
    /// one is remembered (a stream's /Length may be read again); any other object of a stream let go is refused.
    pub held: Option<u64>,
}

impl Limits {
    /// Object streams of up to `OBJSTM_MAX` bytes at each filter stage (and without filters), each kept once
    /// decoded.
    pub const KEEP: Limits = Limits { objstm: OBJSTM_MAX, held: None };

    /// For reading every object of a document once (optimize, the archive): object streams of up to
    /// `max_stream` bytes (and `OBJSTM_MAX`), `HELD_STREAMS` times that held at once. A file outside these is
    /// refused, and so left as it is.
    pub fn once(max_stream: usize) -> Limits {
        let objstm = max_stream.min(OBJSTM_MAX);
        Limits { objstm, held: Some((objstm as u64).saturating_mul(HELD_STREAMS)) }
    }
}

/// What reading object streams took, for measuring (see `Doc::objstm_stats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjStmStats {
    /// Object streams decoded.
    pub decoded: usize,
    /// Bytes they decoded to, every filter stage counted.
    pub decoded_bytes: u64,
    /// Their stored bytes, read out of the file.
    pub stored_bytes: u64,
    /// The largest one, decoded.
    pub largest: usize,
    /// The most bytes held at once (data and header entries).
    pub peak_held: u64,
    /// What they may decode to in all (`u64::MAX` without `Limits::held`), and what is left of it.
    pub budget: u64,
    pub budget_left: u64,
    /// The bytes held now.
    pub held: u64,
    /// Integers read again, from memory.
    pub memo_hits: usize,
    /// Header entries kept whose offset is not past the one before.
    pub unordered: usize,
    /// Header entries kept that start where an entry listed after them does.
    pub shared: usize,
}

/// Cross-reference rows a file may have: `ROWS_PER_BYTE` per byte of the file plus `SPARE_ROWS`. A row takes
/// at least a byte of cross-reference data (20 in a table), so honest files have far fewer rows than bytes,
/// while a crafted stream can hold a thousand rows per byte (Flate), each costing the reader far more than a
/// byte. The same bound holds the rows the writer makes (see `rewrite`), whatever numbers a file uses.
pub const ROWS_PER_BYTE: usize = 16;
pub const SPARE_ROWS: usize = 1 << 16;

/// The most cross-reference rows a file of `len` bytes may have, read or written.
pub fn row_limit(len: usize) -> usize {
    len.saturating_mul(ROWS_PER_BYTE).saturating_add(SPARE_ROWS)
}

pub struct Doc<'a> {
    pub data: &'a [u8],
    pub version: (u8, u8),
    pub trailer: Dict,
    pub entries: BTreeMap<u32, Entry>,
    /// Number of cross-reference sections (more than one: incremental updates or linearisation).
    pub sections: usize,
    /// Some object is defined differently in two sections: the file has earlier revisions.
    pub has_revisions: bool,
    /// Bytes after the last %%EOF.
    pub trailing: usize,
    /// Encrypted files: the security handler when the empty password opens the file (strings and streams are
    /// then decrypted as objects load), or why it doesn't (objects then load as stored).
    pub crypt: Option<Result<Crypt, String>>,
    /// Object streams decoded: held, or let go (None, with `Limits::held`).
    objstm_cache: RefCell<HashMap<u32, Option<ObjStm>>>,
    /// For each object stream, the highest index the cross-reference data points at (its header is read that
    /// far and no further, so a stream's /N can't size anything on its own), and how many rows point into it.
    objstm_last: HashMap<u32, (u32, usize)>,
    limits: Limits,
    /// Bytes the object streams held take (`ObjStm::cost`).
    held: Cell<u64>,
    /// Bytes object streams may still decode to (with `Limits::held`).
    budget: Cell<u64>,
    /// Integers read from object streams (with `Limits::held`).
    ints: RefCell<HashMap<u32, i64>>,
    stats: Cell<ObjStmStats>,
}

/// The last place `pat` starts within the final `window` bytes of `d`; None when `d` is shorter than `pat`.
fn find_last(d: &[u8], pat: &[u8], window: usize) -> Option<usize> {
    if d.len() < pat.len() {
        return None;
    }
    let start = d.len().saturating_sub(window);
    (start..=d.len() - pat.len()).rev().find(|&i| &d[i..i + pat.len()] == pat)
}

fn parse_uint(t: &[u8]) -> Option<usize> {
    if t.is_empty() || !t.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(t).ok()?.parse().ok()
}

/// `parse_uint` in 64 bits, the same on every platform.
fn parse_u64(t: &[u8]) -> Option<u64> {
    if t.is_empty() || !t.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(t).ok()?.parse().ok()
}

/// Would the object parsed from `raw[at..end]`, its parse ending at `pos`, read differently without the end
/// of its window: its last token (a name's slash included) running on into the next object's bytes, or an
/// integer that, read on, starts "n g R" or is followed by a comment the window ends inside? Readers that
/// parse an object stream as a whole and readers that cut it at the next offset disagree on such an object.
/// (Anything else the window ends inside fails to parse.)
fn runs_on(raw: &[u8], at: usize, end: usize, pos: usize, o: &Obj) -> bool {
    if end == 0 || end >= raw.len() {
        return false;
    }
    if pos == end && (is_regular(raw[end - 1]) || raw[end - 1] == b'/') && is_regular(raw[end]) {
        return true;
    }
    if !matches!(o, Obj::Int(_) | Obj::Real(_)) {
        return false;
    }
    let mut lx = Lexer::new(raw, at);
    lx.skip_ws();
    let t = lx.regular_token();
    if t.is_empty() || !t.iter().all(u8::is_ascii_digit) {
        return false;
    }
    // Read on as a reader of the whole stream does. A comment begun before the end of the window and running
    // past it hides the next object's bytes from such a reader (one whose end of line is the window's last byte
    // or the next one's first hides nothing); past the window, only white space and the next objects' own
    // comments are crossed, so the look-ahead takes no more than a pass over the stream in all.
    let skip = |lx: &mut Lexer| -> bool {
        while let Some(c) = lx.peek() {
            if is_white(c) {
                lx.pos += 1;
            } else if c == b'%' {
                let start = lx.pos;
                while lx.peek().is_some_and(|c| c != b'\n' && c != b'\r') {
                    lx.pos += 1;
                }
                if start < end && lx.pos > end {
                    return false;
                }
            } else {
                break;
            }
        }
        true
    };
    if !skip(&mut lx) {
        return true;
    }
    let g = lx.regular_token();
    if g.is_empty() || !g.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if !skip(&mut lx) {
        return true;
    }
    lx.regular_token() == b"R"
}

/// Opens a document, with object streams kept once decoded (`Limits::KEEP`).
pub fn open(data: &[u8]) -> Result<Doc<'_>, LoadError> {
    open_with(data, Limits::KEEP)
}

/// Opens a document, with object streams read within `limits`.
pub fn open_with(data: &[u8], limits: Limits) -> Result<Doc<'_>, LoadError> {
    if !data.starts_with(b"%PDF-") || data.len() < 8 {
        return Err(LoadError::NoHeader);
    }
    let (ma, mi) = (data[5], data[7]);
    if !ma.is_ascii_digit() || data[6] != b'.' || !mi.is_ascii_digit() {
        return Err(LoadError::NoHeader);
    }
    let version = (ma - b'0', mi - b'0');
    let sx = find_last(data, b"startxref", 2048).ok_or(LoadError::NoStartXref)?;
    let mut lx = Lexer::new(data, sx + 9);
    lx.skip_ws();
    // An offset past what this platform addresses is past the end of the file on every platform.
    let off = parse_u64(lx.regular_token()).ok_or(LoadError::NoStartXref)?;
    let off = usize::try_from(off).unwrap_or(usize::MAX);
    let eof = find_last(data, b"%%EOF", 2048).ok_or(LoadError::NoStartXref)?;
    let trailing = data.len() - (eof + 5);

    let mut sections: Vec<(Vec<(u32, Entry)>, Dict)> = Vec::new();
    let mut seen = HashSet::new();
    let mut next = Some(off);
    // Rows are counted against the file's size as they are read, before anything holds them.
    let mut rows_left = row_limit(data.len());
    while let Some(off) = next {
        if !seen.insert(off) || sections.len() > 1000 {
            return Err(LoadError::BadXref("loop in /Prev"));
        }
        let (mut entries, trailer) = read_section(data, off, &mut rows_left)?;
        if let Some(Obj::Int(x)) = trailer.get(b"XRefStm") {
            // Hybrid file: the stream supplements the table; table entries win.
            let (extra, _) = read_section(data, usize::try_from(*x).unwrap_or(usize::MAX), &mut rows_left)?;
            let have: HashSet<u32> = entries.iter().map(|e| e.0).collect();
            entries.extend(extra.into_iter().filter(|e| !have.contains(&e.0)));
        }
        next = match trailer.get(b"Prev") {
            Some(Obj::Int(p)) if *p >= 0 => Some(usize::try_from(*p).unwrap_or(usize::MAX)),
            None => None,
            _ => return Err(LoadError::BadXref("bad /Prev")),
        };
        sections.push((entries, trailer));
    }
    let mut merged: BTreeMap<u32, Entry> = BTreeMap::new();
    let mut has_revisions = false;
    for (entries, _) in &sections {
        for &(n, e) in entries {
            if n == 0 {
                continue;
            }
            match merged.get(&n) {
                None => {
                    merged.insert(n, e);
                }
                Some(&prev) => {
                    if prev != e {
                        has_revisions = true;
                    }
                }
            }
        }
    }
    let mut objstm_last: HashMap<u32, (u32, usize)> = HashMap::new();
    for e in merged.values() {
        if let &Entry::InStream(sn, idx) = e {
            let (last, rows) = objstm_last.entry(sn).or_insert((idx, 0));
            *last = (*last).max(idx);
            *rows = rows.saturating_add(1);
        }
    }
    let trailer = sections[0].1.clone();
    if !trailer.has(b"Root") {
        return Err(LoadError::BadXref("trailer without /Root"));
    }
    let budget = limits.held.map_or(u64::MAX, |cap| cap.max((data.len() as u64).saturating_mul(OBJSTM_PER_BYTE)));
    let mut doc = Doc {
        data,
        version,
        trailer,
        entries: merged,
        sections: sections.len(),
        has_revisions,
        trailing,
        crypt: None,
        objstm_cache: RefCell::new(HashMap::new()),
        objstm_last,
        limits,
        held: Cell::new(0),
        budget: Cell::new(budget),
        ints: RefCell::new(HashMap::new()),
        stats: Cell::new(ObjStmStats { budget, ..Default::default() }),
    };
    if doc.trailer.has(b"Encrypt") {
        doc.crypt = Some(security(&doc));
        // Metadata left unencrypted is the catalog's (found with the key in place).
        let md = match &doc.crypt {
            Some(Ok(c)) if !c.encrypt_metadata => Some(root_metadata(&doc)),
            _ => None,
        };
        match md {
            Some(Ok(m)) => {
                if let Some(Ok(c)) = doc.crypt.as_mut() {
                    c.metadata = m;
                }
            }
            Some(Err(e)) => doc.crypt = Some(Err(e)),
            None => {}
        }
        // Nothing read before the handler was complete is kept: object streams are read again from the start
        // (what they decoded to stays counted).
        doc.objstm_cache.borrow_mut().clear();
        doc.ints.borrow_mut().clear();
        doc.held.set(0);
    }
    Ok(doc)
}

/// The standard security handler of an encrypted file, with the empty user password.
fn security(doc: &Doc) -> Result<Crypt, String> {
    // Read as stored: the encryption dictionary is never encrypted, nor in an object stream.
    let plain = |n: u32, g: u16| -> Result<Obj, String> {
        if !matches!(doc.entries.get(&n), Some(&Entry::Offset(_, eg)) if eg == g) {
            return Err("encryption dictionary not stored as a plain object".into());
        }
        doc.load(n).map(|x| x.1).map_err(|_| "unreadable encryption dictionary".to_string())
    };
    let (dict, obj) = match doc.trailer.get(b"Encrypt") {
        Some(Obj::Dict(d)) => (d.clone(), None),
        Some(&Obj::Ref(n, g)) => match plain(n, g)? {
            Obj::Dict(d) => (d, Some((n, g))),
            _ => return Err("bad /Encrypt".into()),
        },
        _ => return Err("bad /Encrypt".into()),
    };
    // Values given by reference (a /CF dictionary, say) are read as stored too, a few levels deep, each object
    // once and without strings, so readers can't disagree on whether they are encrypted.
    fn resolve(
        o: &Obj,
        depth: usize,
        own: Option<u32>,
        parts: &mut Vec<(u32, u16, Obj)>,
        plain: &dyn Fn(u32, u16) -> Result<Obj, String>,
    ) -> Result<Obj, String> {
        Ok(match o {
            &Obj::Ref(n, g) => {
                if depth > 4 || own == Some(n) || parts.iter().any(|p| p.0 == n) {
                    return Err("encryption dictionary with unusual references".into());
                }
                let x = plain(n, g)?;
                if super::crypt::has_string(&x) || matches!(x, Obj::Stream(_)) {
                    return Err("encryption dictionary with unusual references".into());
                }
                parts.push((n, g, x.clone()));
                resolve(&x, depth + 1, own, parts, plain)?
            }
            Obj::Array(a) => {
                Obj::Array(a.iter().map(|x| resolve(x, depth, own, parts, plain)).collect::<Result<_, _>>()?)
            }
            Obj::Dict(d) => Obj::Dict(Dict(
                d.0.iter()
                    .map(|(k, v)| Ok((k.clone(), resolve(v, depth, own, parts, plain)?)))
                    .collect::<Result<_, String>>()?,
            )),
            x => x.clone(),
        })
    }
    let mut parts: Vec<(u32, u16, Obj)> = Vec::new();
    let open = match resolve(&Obj::Dict(dict.clone()), 0, obj.map(|o| o.0), &mut parts, &plain)? {
        Obj::Dict(d) => d,
        _ => unreachable!(),
    };
    let mut c = Crypt::open(&open, obj, doc.trailer.get(b"ID"))?;
    c.dict = dict;
    c.parts = parts;
    Ok(c)
}

/// The object number of the catalog's /Metadata stream, if any.
fn root_metadata(doc: &Doc) -> Result<Option<u32>, String> {
    let root = match doc.trailer.get(b"Root") {
        Some(&Obj::Ref(n, _)) => n,
        _ => return Err("bad /Root".into()),
    };
    match doc.load(root) {
        Ok((_, Obj::Dict(cat))) => match cat.get(b"Metadata") {
            None => Ok(None),
            Some(&Obj::Ref(m, _)) => Ok(Some(m)),
            _ => Err("bad /Metadata in the catalog".into()),
        },
        Err(e) if is_refusal(&e) => Err(format!("unreadable catalog: {e:?}")),
        _ => Err("unreadable catalog".into()),
    }
}

/// A row of cross-reference data, counted against what the file may have (see `row_limit`).
fn count_row(rows_left: &mut usize) -> Result<(), LoadError> {
    if *rows_left == 0 {
        return Err(LoadError::BadXref("more cross-reference rows than the file can hold"));
    }
    *rows_left -= 1;
    Ok(())
}

fn read_section(d: &[u8], off: usize, rows_left: &mut usize) -> Result<(Vec<(u32, Entry)>, Dict), LoadError> {
    if off >= d.len() {
        return Err(LoadError::BadXref("offset past the end"));
    }
    if d[off..].starts_with(b"xref") {
        return read_table(d, off + 4, rows_left);
    }
    // An xref stream.
    let (_, _, o, _) = parse_indirect(d, off, &|l| match l {
        Obj::Int(i) if *i >= 0 => Some(usize::try_from(*i).unwrap_or(usize::MAX)),
        _ => None,
    })
    .map_err(|_| LoadError::BadXref("xref stream unreadable"))?;
    let s = match o {
        Obj::Stream(s) => s,
        _ => return Err(LoadError::BadXref("xref offset is not a stream")),
    };
    if s.dict.name(b"Type") != Some(b"XRef") {
        return Err(LoadError::BadXref("stream is not /Type /XRef"));
    }
    let w: Vec<usize> = match s.dict.get(b"W") {
        Some(Obj::Array(a)) if a.len() == 3 => {
            let mut v = Vec::new();
            for x in a {
                match x {
                    Obj::Int(i) if (0..=8).contains(i) => v.push(*i as usize),
                    _ => return Err(LoadError::BadXref("bad /W")),
                }
            }
            v
        }
        _ => return Err(LoadError::BadXref("bad /W")),
    };
    let size = s.dict.get(b"Size").and_then(|x| x.as_int()).ok_or(LoadError::BadXref("no /Size"))?;
    // Object numbers are 32-bit; a count is only checked against the data below, row by row.
    let fits = |v: i64| u32::try_from(v).map_err(|_| LoadError::BadXref("bad /Index"));
    let index: Vec<(u32, u32)> = match s.dict.get(b"Index") {
        None => vec![(0, fits(size)?)],
        Some(Obj::Array(a)) if a.len() % 2 == 0 => {
            let mut v = Vec::new();
            for p in a.chunks(2) {
                match (&p[0], &p[1]) {
                    (Obj::Int(x), Obj::Int(y)) => v.push((fits(*x)?, fits(*y)?)),
                    _ => return Err(LoadError::BadXref("bad /Index")),
                }
            }
            v
        }
        _ => return Err(LoadError::BadXref("bad /Index")),
    };
    let rec = w[0] + w[1] + w[2];
    if rec == 0 {
        return Err(LoadError::BadXref("empty /W"));
    }
    // Decoded only as far as the rows the file may have reach (see `row_limit`): a stream of more is
    // refused without holding it.
    let most = row_limit(d.len()).saturating_mul(rec).min(1 << 28);
    let raw = decode_fully(&s.dict, &s.data, most)
        .ok_or(LoadError::BadXref("xref stream undecodable or more rows than the file can hold"))?;
    let field = |b: &[u8]| b.iter().fold(0u64, |a, &x| a << 8 | x as u64);
    let mut out = Vec::new();
    let mut p = 0;
    for (start, count) in index {
        for k in 0..count {
            if p + rec > raw.len() {
                return Err(LoadError::BadXref("xref stream too short"));
            }
            count_row(rows_left)?;
            let r = &raw[p..p + rec];
            p += rec;
            let t = if w[0] == 0 { 1 } else { field(&r[..w[0]]) };
            let f2 = field(&r[w[0]..w[0] + w[1]]);
            let f3 = field(&r[w[0] + w[1]..]);
            let e = match t {
                0 => Entry::Free,
                // An offset past what this platform addresses is past the end of the file on every platform.
                1 => Entry::Offset(usize::try_from(f2).unwrap_or(usize::MAX), f3 as u16),
                2 => Entry::InStream(f2 as u32, f3 as u32),
                _ => Entry::Free, // unknown types are treated as null references by the spec
            };
            out.push((start.checked_add(k).ok_or(LoadError::BadXref("object number beyond 32 bits"))?, e));
        }
    }
    Ok((out, s.dict))
}

fn read_table(d: &[u8], mut pos: usize, rows_left: &mut usize) -> Result<(Vec<(u32, Entry)>, Dict), LoadError> {
    let mut out = Vec::new();
    loop {
        let mut lx = Lexer::new(d, pos);
        lx.skip_ws();
        if lx.expect_keyword(b"trailer") {
            let t = lx.parse_obj().map_err(|_| LoadError::BadXref("bad trailer"))?;
            if lx.raw_cr {
                return Err(LoadError::BadXref(super::parse::RAW_CR));
            }
            return match t {
                Obj::Dict(t) => Ok((out, t)),
                _ => Err(LoadError::BadXref("trailer is not a dictionary")),
            };
        }
        // In 64 bits, so that every platform decides alike.
        let start = parse_u64(lx.regular_token()).ok_or(LoadError::BadXref("bad subsection"))?;
        lx.skip_ws();
        let count = parse_u64(lx.regular_token()).ok_or(LoadError::BadXref("bad subsection"))?;
        // Entries start after the end of line.
        let mut p = lx.pos;
        while p < d.len() && (d[p] == b' ' || d[p] == b'\r' || d[p] == b'\n') {
            p += 1;
        }
        for k in 0..count {
            if p + 18 > d.len() {
                return Err(LoadError::BadXref("table past the end"));
            }
            count_row(rows_left)?;
            let off = parse_u64(&d[p..p + 10]).ok_or(LoadError::BadXref("bad table entry"))?;
            let gen = parse_uint(&d[p + 11..p + 16]).ok_or(LoadError::BadXref("bad table entry"))?;
            if d[p + 10] != b' ' || d[p + 16] != b' ' {
                return Err(LoadError::BadXref("bad table entry"));
            }
            let e = match d[p + 17] {
                // An offset past what this platform addresses is past the end of the file on every platform.
                b'n' => Entry::Offset(usize::try_from(off).unwrap_or(usize::MAX), gen as u16),
                b'f' => Entry::Free,
                _ => return Err(LoadError::BadXref("bad table entry type")),
            };
            let num = start
                .checked_add(k)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or(LoadError::BadXref("object number beyond 32 bits"))?;
            out.push((num, e));
            p += 18;
            // Two end-of-line bytes in well-formed files; tolerate one.
            let mut eol = 0;
            while p < d.len() && (d[p] == b' ' || d[p] == b'\r' || d[p] == b'\n') && eol < 2 {
                p += 1;
                eol += 1;
            }
        }
        pos = p;
    }
}

/// Decodes all filters of a stream (general filters only), predictors included.
pub fn decode_fully(dict: &Dict, data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let stages = chain(dict)?;
    let mut cur = data.to_vec();
    for st in &stages {
        cur = decode_stage(st, &cur, limit).ok()?;
        if st.name == b"FlateDecode" || st.name == b"LZWDecode" {
            cur = unpredict(st, &cur).ok()?;
        }
    }
    Some(cur)
}

impl<'a> Doc<'a> {
    /// Loads one object by number. `depth` guards against /Length chains.
    pub fn load(&self, n: u32) -> Result<(u16, Obj), LoadError> {
        self.load_depth(n, 0)
    }

    fn load_depth(&self, n: u32, depth: usize) -> Result<(u16, Obj), LoadError> {
        if depth > 8 {
            return Err(LoadError::BadObject(n, "reference chain too deep"));
        }
        match self.entries.get(&n) {
            None | Some(Entry::Free) => Err(LoadError::Missing(n)),
            Some(&Entry::Offset(off, gen)) => {
                // A /Length in an object stream refused for its limits or rules gives that reason.
                let refused = RefCell::new(None);
                let resolver = |l: &Obj| -> Option<usize> {
                    match l {
                        Obj::Int(i) if *i >= 0 => Some(usize::try_from(*i).unwrap_or(usize::MAX)),
                        Obj::Ref(rn, _) => match self.load_depth(*rn, depth + 1) {
                            Ok((_, Obj::Int(i))) if i >= 0 => Some(usize::try_from(i).unwrap_or(usize::MAX)),
                            Err(e) if is_refusal(&e) => {
                                *refused.borrow_mut() = Some(e);
                                None
                            }
                            _ => None,
                        },
                        _ => None,
                    }
                };
                if off >= self.data.len() {
                    return Err(LoadError::BadObject(n, "offset past the end"));
                }
                let (pn, pg, o, _) = parse_indirect(self.data, off, &resolver)
                    .map_err(|e| refused.borrow_mut().take().unwrap_or(LoadError::BadObject(n, e.msg)))?;
                if pn != n || pg != gen {
                    return Err(LoadError::BadObject(n, "object number at offset differs"));
                }
                match &self.crypt {
                    // Encrypted with this object's key (the encryption dictionary itself never is).
                    Some(Ok(c)) if !c.is_own(n) => {
                        Ok((gen, c.decrypt_object(n, gen, o).map_err(|e| LoadError::BadObject(n, e))?))
                    }
                    _ => Ok((gen, o)),
                }
            }
            Some(&Entry::InStream(sn, idx)) => {
                if let Some(&v) = self.ints.borrow().get(&n) {
                    self.note(|s| s.memo_hits += 1);
                    return Ok((0, Obj::Int(v)));
                }
                let state = self.objstm_cache.borrow().get(&sn).map(Option::is_some);
                match state {
                    Some(true) => {}
                    Some(false) => return Err(LoadError::BadObject(n, "object stream already read through")),
                    None => self.decode_objstm(sn, depth)?,
                }
                let mut cache = self.objstm_cache.borrow_mut();
                let slot = cache.get_mut(&sn).ok_or(LoadError::BadObject(sn, "object stream not decoded"))?;
                let h = slot.as_mut().ok_or(LoadError::BadObject(n, "object stream already read through"))?;
                let head = *h.heads.get(idx as usize).ok_or(LoadError::BadObject(n, "index past object stream"))?;
                if head.num != n {
                    return Err(LoadError::BadObject(n, "object stream lists another number"));
                }
                if head.at == PAST {
                    return Err(LoadError::BadObject(n, "offset past the object stream"));
                }
                // Of the entries that start at one place, the last listed is read: readers that cut each object
                // at the next entry's offset give the others no bytes, readers of the whole stream don't.
                if head.shared {
                    return Err(LoadError::BadObject(n, SHARED));
                }
                let (at, end) = (head.at as usize, head.end as usize);
                let mut lx = Lexer::new(&h.raw[..end], at);
                let o = lx.parse_obj().map_err(|e| LoadError::BadObject(n, e.msg))?;
                if lx.raw_cr {
                    return Err(LoadError::BadObject(n, super::parse::RAW_CR));
                }
                if runs_on(&h.raw, at, end, lx.pos, &o) {
                    return Err(LoadError::BadObject(n, RUNS_ON));
                }
                if self.limits.held.is_some() {
                    if let Obj::Int(v) = o {
                        self.ints.borrow_mut().insert(n, v);
                    }
                    if !head.read {
                        h.heads[idx as usize].read = true;
                        h.left = h.left.saturating_sub(1);
                        if h.left == 0 {
                            self.held.set(self.held.get().saturating_sub(h.cost));
                            *slot = None;
                        }
                    }
                }
                Ok((0, o))
            }
        }
    }

    /// Decodes object stream `sn` within the limits and reads its header as far as the cross-reference data
    /// points into it.
    fn decode_objstm(&self, sn: u32, depth: usize) -> Result<(), LoadError> {
        let (_, so) = self.load_depth(sn, depth + 1)?;
        let s = match so {
            Obj::Stream(s) => s,
            _ => return Err(LoadError::BadObject(sn, "object stream is not a stream")),
        };
        if self.objstm_cache.borrow().contains_key(&sn) {
            return Ok(());
        }
        // The stored bytes were copied out of the file to read it: they count against the budget too, so streams
        // nested in each other's bytes can't make the time grow faster than the file.
        self.charge_stored(sn, s.data.len())?;
        let mut raw = self.objstm_data(sn, &s)?;
        // What is held is what is counted: no room left over from decoding.
        raw.shrink_to_fit();
        let count = s.dict.get(b"N").and_then(|x| x.as_int()).ok_or(LoadError::BadObject(sn, "no /N"))?;
        let first = s.dict.get(b"First").and_then(|x| x.as_int()).ok_or(LoadError::BadObject(sn, "no /First"))?;
        drop(s);
        // Every header entry takes at least "n o " (two digits and two spaces, the last space optional), so /N
        // beyond a quarter of the data, or /First past it, can only be wrong. Both are checked before anything
        // is sized by them: the dictionary is not to be trusted.
        if count < 0 || count as u64 > (raw.len() as u64 + 1) / 4 || first < 0 || first as u64 > raw.len() as u64 {
            return Err(LoadError::BadObject(sn, "bad /N or /First"));
        }
        let (count, first) = (count as usize, first as usize);
        let len = u32::try_from(raw.len()).map_err(|_| LoadError::BadObject(sn, TOO_LARGE))?;
        // Every pair of the header must parse, but only the ones the cross-reference data points at are kept
        // (the rows are bounded by the file's size, see `row_limit`): the memory follows the rows, the time the
        // data, and /N on its own sizes nothing.
        let (keep, rows) =
            self.objstm_last.get(&sn).map_or((0, 0), |&(last, rows)| ((u64::from(last) + 1).min(count as u64), rows));
        let holdable = self.limits.held.map(|cap| cap.saturating_sub(self.held.get()));
        let mut cost = raw.len() as u64;
        let mut stats = self.stats.get();
        let mut lx = Lexer::new(&raw, 0);
        let mut heads: Vec<Head> = Vec::new();
        // Where the first entry after the ones kept starts: it ends the window of the one before it too.
        let mut beyond = PAST;
        for i in 0..count as u64 {
            lx.skip_ws();
            let a = parse_u64(lx.regular_token()).ok_or(LoadError::BadObject(sn, "bad object stream header"))?;
            lx.skip_ws();
            let b = parse_u64(lx.regular_token()).ok_or(LoadError::BadObject(sn, "bad object stream header"))?;
            let a = u32::try_from(a).map_err(|_| LoadError::BadObject(sn, "bad object stream header"))?;
            if i > keep {
                continue;
            }
            let at = (first as u64).checked_add(b).filter(|&p| p <= raw.len() as u64).map_or(PAST, |p| p as u32);
            if let Some(h) = heads.last_mut().filter(|h| h.at != PAST && at != PAST) {
                stats.unordered += usize::from(at <= h.at);
            }
            if i == keep {
                beyond = at;
                continue;
            }
            cost += HEAD_COST;
            if holdable.is_some_and(|h| cost > h) {
                return Err(LoadError::BadObject(sn, HELD_TOO_MUCH));
            }
            heads.push(Head { num: a, at, end: len, shared: false, read: false });
        }
        heads.shrink_to_fit();
        // Of the entries kept (up to the last one the cross-reference data points at) and the first one after
        // them, only the last listed at each place may be read, wherever the others are listed: an entry is
        // shared when one listed after it among those starts there too. Entries further on are never read, so
        // they are not compared.
        let mut order: Vec<(u32, usize)> =
            heads.iter().enumerate().filter(|(_, h)| h.at != PAST).map(|(i, h)| (h.at, i)).collect();
        order.sort_unstable();
        for (k, &(at, i)) in order.iter().enumerate() {
            heads[i].shared = at == beyond || order.get(k + 1).is_some_and(|&(next, _)| next == at);
            stats.shared += usize::from(heads[i].shared);
        }
        drop(order);
        // Each object kept is read up to where the next one starts, in whatever order the header lists them.
        let mut starts: Vec<u32> = heads.iter().map(|h| h.at).chain([beyond]).filter(|&a| a != PAST).collect();
        starts.sort_unstable();
        for h in heads.iter_mut().filter(|h| h.at != PAST) {
            h.end = starts.get(starts.partition_point(|&s| s <= h.at)).copied().unwrap_or(len);
        }
        drop(starts);
        self.held.set(self.held.get().saturating_add(cost));
        stats.decoded += 1;
        stats.largest = stats.largest.max(raw.len());
        stats.peak_held = stats.peak_held.max(self.held.get());
        self.stats.set(stats);
        self.objstm_cache.borrow_mut().insert(sn, Some(ObjStm { raw, heads, left: rows, cost }));
        Ok(())
    }

    /// The decoded data of an object stream. Each filter stage decodes to at most what the limits leave (see
    /// `room`), and what it decodes to is counted against the budget. The stored data is borrowed, and a stage
    /// without a predictor is not copied again: what is refused or accepted is what `decode_fully` refuses or
    /// accepts, within the limits.
    fn objstm_data(&self, sn: u32, s: &Stream) -> Result<Vec<u8>, LoadError> {
        let undecodable = LoadError::BadObject(sn, "object stream undecodable");
        let stages = chain(&s.dict).ok_or(undecodable.clone())?;
        let mut cur = Cow::Borrowed(&s.data[..]);
        if stages.is_empty() {
            self.charge(sn, cur.len())?;
        }
        for st in &stages {
            let (room, why) = self.room();
            cur = Cow::Owned(match decode_stage(st, &cur, room) {
                Ok(out) => out,
                Err(FilterError::TooLarge) => return Err(LoadError::BadObject(sn, why)),
                Err(_) => return Err(undecodable),
            });
            self.charge(sn, cur.len())?;
            if st.name == b"FlateDecode" || st.name == b"LZWDecode" {
                let (pred, ..) = predictor(st).ok_or(undecodable.clone())?;
                if pred != 1 {
                    cur = Cow::Owned(unpredict(st, &cur).map_err(|_| undecodable.clone())?);
                }
            }
        }
        Ok(cur.into_owned())
    }

    /// The most bytes the next filter stage of an object stream may decode to: the per-stream limit, what may
    /// still be held, and what is left of the budget, whichever is least; and the reason a stage past it gives.
    fn room(&self) -> (usize, &'static str) {
        let mut room = (self.limits.objstm as u64, TOO_LARGE);
        if let Some(cap) = self.limits.held {
            let held = cap.saturating_sub(self.held.get());
            if held < room.0 {
                room = (held, HELD_TOO_MUCH);
            }
            if self.budget.get() < room.0 {
                room = (self.budget.get(), OVER_BUDGET);
            }
        }
        // At most `limits.objstm`, a usize.
        (room.0 as usize, room.1)
    }

    /// Counts `bytes` decoded for object stream `sn` against the budget, after checking them against `room`
    /// (a decoder may return more than it was allowed).
    fn charge(&self, sn: u32, bytes: usize) -> Result<(), LoadError> {
        let (room, why) = self.room();
        if bytes > room {
            return Err(LoadError::BadObject(sn, why));
        }
        if self.limits.held.is_some() {
            self.budget.set(self.budget.get().saturating_sub(bytes as u64));
        }
        self.note(|s| s.decoded_bytes = s.decoded_bytes.saturating_add(bytes as u64));
        Ok(())
    }

    /// Counts the stored bytes of object stream `sn`, read out of the file, against the budget.
    fn charge_stored(&self, sn: u32, bytes: usize) -> Result<(), LoadError> {
        if self.limits.held.is_some() {
            if bytes as u64 > self.budget.get() {
                return Err(LoadError::BadObject(sn, OVER_BUDGET));
            }
            self.budget.set(self.budget.get() - bytes as u64);
        }
        self.note(|s| s.stored_bytes = s.stored_bytes.saturating_add(bytes as u64));
        Ok(())
    }

    fn note(&self, f: impl FnOnce(&mut ObjStmStats)) {
        let mut s = self.stats.get();
        f(&mut s);
        self.stats.set(s);
    }

    /// Lets go of every object stream held. With `Limits::held` the objects of those streams are refused from
    /// then on, integers aside; otherwise a stream is decoded again when one of its objects is read.
    pub fn drop_cache(&self) {
        let mut cache = self.objstm_cache.borrow_mut();
        if self.limits.held.is_some() {
            cache.values_mut().for_each(|s| *s = None);
        } else {
            cache.clear();
        }
        self.held.set(0);
    }

    /// What reading object streams took so far.
    pub fn objstm_stats(&self) -> ObjStmStats {
        ObjStmStats { budget_left: self.budget.get(), held: self.held.get(), ..self.stats.get() }
    }
}
