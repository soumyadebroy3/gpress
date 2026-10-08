//! Cross-reference tables and streams, and loading every object of a document — strictly.

use super::crypt::Crypt;
use super::filters::{chain, decode_stage, unpredict};
use super::object::*;
use super::parse::{parse_indirect, Lexer};
use std::cell::RefCell;
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

/// A decoded object stream: its data, the (object number, offset) pairs of its header, and where the first
/// object starts.
type ObjStm = (Vec<u8>, Vec<(u32, usize)>, usize);

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
    objstm_cache: RefCell<HashMap<u32, ObjStm>>,
    /// For each object stream, the highest index the cross-reference data points at: its header is read
    /// that far and no further, so a stream's /N can't size anything on its own.
    objstm_last: HashMap<u32, u32>,
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

pub fn open(data: &[u8]) -> Result<Doc<'_>, LoadError> {
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
    let off = parse_uint(lx.regular_token()).ok_or(LoadError::NoStartXref)?;
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
            let (extra, _) = read_section(data, *x as usize, &mut rows_left)?;
            let have: HashSet<u32> = entries.iter().map(|e| e.0).collect();
            entries.extend(extra.into_iter().filter(|e| !have.contains(&e.0)));
        }
        next = match trailer.get(b"Prev") {
            Some(Obj::Int(p)) if *p >= 0 => Some(*p as usize),
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
    let mut objstm_last: HashMap<u32, u32> = HashMap::new();
    for e in merged.values() {
        if let &Entry::InStream(sn, idx) = e {
            let last = objstm_last.entry(sn).or_insert(idx);
            *last = (*last).max(idx);
        }
    }
    let trailer = sections[0].1.clone();
    if !trailer.has(b"Root") {
        return Err(LoadError::BadXref("trailer without /Root"));
    }
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
        // Nothing read before the handler was complete is kept.
        doc.objstm_cache.borrow_mut().clear();
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
        Obj::Int(i) if *i >= 0 => Some(*i as usize),
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
                1 => Entry::Offset(f2 as usize, f3 as u16),
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
        let start = parse_uint(lx.regular_token()).ok_or(LoadError::BadXref("bad subsection"))?;
        lx.skip_ws();
        let count = parse_uint(lx.regular_token()).ok_or(LoadError::BadXref("bad subsection"))?;
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
            let off = parse_uint(&d[p..p + 10]).ok_or(LoadError::BadXref("bad table entry"))?;
            let gen = parse_uint(&d[p + 11..p + 16]).ok_or(LoadError::BadXref("bad table entry"))?;
            if d[p + 10] != b' ' || d[p + 16] != b' ' {
                return Err(LoadError::BadXref("bad table entry"));
            }
            let e = match d[p + 17] {
                b'n' => Entry::Offset(off, gen as u16),
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
                let resolver = |l: &Obj| -> Option<usize> {
                    match l {
                        Obj::Int(i) if *i >= 0 => Some(*i as usize),
                        Obj::Ref(rn, _) => match self.load_depth(*rn, depth + 1) {
                            Ok((_, Obj::Int(i))) if i >= 0 => Some(i as usize),
                            _ => None,
                        },
                        _ => None,
                    }
                };
                if off >= self.data.len() {
                    return Err(LoadError::BadObject(n, "offset past the end"));
                }
                let (pn, pg, o, _) =
                    parse_indirect(self.data, off, &resolver).map_err(|e| LoadError::BadObject(n, e.msg))?;
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
                if !self.objstm_cache.borrow().contains_key(&sn) {
                    let (_, so) = self.load_depth(sn, depth + 1)?;
                    let s = match so {
                        Obj::Stream(s) => s,
                        _ => return Err(LoadError::BadObject(sn, "object stream is not a stream")),
                    };
                    let raw = decode_fully(&s.dict, &s.data, 1 << 28)
                        .ok_or(LoadError::BadObject(sn, "object stream undecodable"))?;
                    let count = s.dict.get(b"N").and_then(|x| x.as_int()).ok_or(LoadError::BadObject(sn, "no /N"))?;
                    let first =
                        s.dict.get(b"First").and_then(|x| x.as_int()).ok_or(LoadError::BadObject(sn, "no /First"))?;
                    // Every header entry takes at least "n o " (two digits and two spaces, the last space
                    // optional), so /N beyond a quarter of the data, or /First past it, can only be wrong. Both
                    // are checked before anything is sized by them: the dictionary is not to be trusted.
                    if count < 0
                        || count as u64 > (raw.len() as u64 + 1) / 4
                        || first < 0
                        || first as u64 > raw.len() as u64
                    {
                        return Err(LoadError::BadObject(sn, "bad /N or /First"));
                    }
                    let (count, first) = (count as usize, first as usize);
                    // Every pair of the header must parse, but only the ones the cross-reference data points
                    // at are kept (the rows are bounded by the file's size, see `row_limit`): the memory
                    // follows the rows, the time the data, and /N on its own sizes nothing.
                    let keep = self.objstm_last.get(&sn).map_or(0, |&last| last as usize + 1);
                    let mut lx = Lexer::new(&raw, 0);
                    let mut heads = Vec::new();
                    for _ in 0..count {
                        lx.skip_ws();
                        let a = parse_uint(lx.regular_token())
                            .ok_or(LoadError::BadObject(sn, "bad object stream header"))?;
                        lx.skip_ws();
                        let b = parse_uint(lx.regular_token())
                            .ok_or(LoadError::BadObject(sn, "bad object stream header"))?;
                        let a = u32::try_from(a).map_err(|_| LoadError::BadObject(sn, "bad object stream header"))?;
                        if heads.len() < keep {
                            heads.push((a, b));
                        }
                    }
                    self.objstm_cache.borrow_mut().insert(sn, (raw, heads, first));
                }
                let cache = self.objstm_cache.borrow();
                let (raw, heads, first) = cache.get(&sn).unwrap();
                let &(num, off) = heads.get(idx as usize).ok_or(LoadError::BadObject(n, "index past object stream"))?;
                if num != n {
                    return Err(LoadError::BadObject(n, "object stream lists another number"));
                }
                let at = first
                    .checked_add(off)
                    .filter(|&p| p <= raw.len())
                    .ok_or(LoadError::BadObject(n, "offset past the object stream"))?;
                let mut lx = Lexer::new(raw, at);
                let o = lx.parse_obj().map_err(|e| LoadError::BadObject(n, e.msg))?;
                if lx.raw_cr {
                    return Err(LoadError::BadObject(n, super::parse::RAW_CR));
                }
                Ok((0, o))
            }
        }
    }
}
