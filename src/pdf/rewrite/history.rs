//! Files that keep earlier saves (incremental updates): every revision is kept, compressed, unless the caller asks
//! for the earlier ones to be dropped.
//!
//! Where each save ends is found first (`xref::saves`). Every save is then read on its own, the file cut at its end,
//! the way a reader opens that cut file: it must open, load and pass the gate. A signature, a certification, usage
//! rights, PDF/A or PDF/X in any save leaves the file untouched whatever the options say. Then:
//!
//! - `keep` writes every revision. The first is written as a file of its own; each later one is appended as one
//!   cross-reference-stream section with the objects that revision adds or changes, re-encoded and packed like the
//!   first, and a free row for each object it takes away. Nothing that exists is renumbered: new objects (object
//!   streams, cross-reference streams) are numbered above every number any revision uses. Before anything is
//!   returned, each output revision is read back, the output cut at its end, and compared value for value with the
//!   input cut at its end (`check`).
//! - With `Options::drop_history`, `dropped` checks every save the same way and lists the earlier ones; `rewrite` then
//!   goes on with the newest as a file of one save.

use super::*;
use crate::pdf::xref::{Limits, Save};
use std::sync::Arc;

const UNLINKED: &str = "keeps earlier revisions: a save the newest doesn't link";
const OPENS: &str = "keeps earlier revisions: a save that doesn't open on its own";
const GENERATION: &str = "keeps earlier revisions: a reference to another generation";
const ABSENT: &str = "keeps earlier revisions: a reference to an object that isn't there";
const ELSEWHERE: &str = "keeps earlier revisions: a row that doesn't point at its object";
const LACKS: &str = "keeps earlier revisions: a trailer key that only an earlier save has";
const NOT_CATALOG: &str = "keeps earlier revisions: a /Root that isn't a catalog";
const TOO_HIGH: &str = "keeps earlier revisions: numbers too high for a revision's length";
const ENCRYPTION: &str = "encrypted: revisions differ in their encryption";

/// What a save's trailer reaches: each object's generation and value, by number.
type Reached = BTreeMap<u32, (u16, Obj)>;

/// An earlier save that `Options::drop_history` dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dropped {
    /// The input bytes it spans: from the end of the save before it to its own end.
    pub start: usize,
    pub end: usize,
    /// Objects it reaches with a value the output doesn't hold.
    pub versions: usize,
}

/// A sign of a signature, a certification, usage rights, PDF/X or PDF/A anywhere in the bytes, whether a save
/// reaches it or not; the gate then looks at what every save reaches. (Not /Perms, which an AES-256 encryption
/// dictionary has too, nor the PDF/A namespace alone, which files declare without claiming PDF/A.)
fn marker(input: &[u8]) -> Option<&'static str> {
    const MARKS: [(&[u8], &str); 7] = [
        (b"/DocMDP", "has usage rights or certification"),
        (b"/UR3", "has usage rights or certification"),
        (b"/ByteRange", "digitally signed"),
        (b"/FieldMDP", "digitally signed"),
        (b"GTS_PDFX", "PDF/X"),
        (b"pdfaid:", "PDF/A"),
        (b"GTS_PDFA", "PDF/A"),
    ];
    let mut found = [false; MARKS.len()];
    for at in 0..input.len() {
        if matches!(input[at], b'/' | b'G' | b'p') {
            for (k, (mark, _)) in MARKS.iter().enumerate() {
                found[k] |= input[at..].starts_with(mark);
            }
        }
    }
    MARKS.iter().zip(found).find(|(_, found)| *found).map(|(mark, _)| mark.1)
}

/// The file's saves, oldest first, when it has more than one and they may be read. With `drop` an earlier save the
/// newest doesn't link is allowed (it can be dropped, never kept).
fn find(input: &[u8], drop: bool) -> Result<Vec<Save>, String> {
    if let Some(why) = marker(input) {
        return Err(why.into());
    }
    let saves = xref::saves(input)?;
    // One save in a layout `open_with` doesn't take for one (a hybrid file, say): left as it is, as before.
    if saves.len() < 2 {
        return Err(xref::KEEPS.into());
    }
    if !drop && saves.iter().any(|s| !s.linked) {
        return Err(UNLINKED.into());
    }
    // (Written alone, the newest save would lose a key that such readers take from an earlier trailer.)
    if drop && saves.iter().any(|s| s.lacks_key) {
        return Err(LACKS.into());
    }
    // Each save is read as a file of its own: together they are at most 16 times the input.
    if saves.iter().map(|s| s.end as u64).sum::<u64>() > (input.len() as u64).saturating_mul(16) {
        return Err(xref::TOO_MANY.into());
    }
    Ok(saves)
}

/// What reading a file's saves one after another may take in all: the cross-reference rows and the object-stream
/// bytes that one read of the whole file may take (each save is also held to its own limits, see `xref::Limits`).
/// Counted after each save, so the reads go past it by at most what the last one may take itself.
struct Budget {
    rows: usize,
    decoded: u64,
}

impl Budget {
    fn new(input: &[u8], opt: &Options) -> Budget {
        let held = Limits::once(opt.max_stream).held.unwrap_or(0);
        Budget {
            rows: xref::row_limit(input.len()),
            decoded: held.max((input.len() as u64).saturating_mul(xref::OBJSTM_PER_BYTE)),
        }
    }

    fn take(&mut self, doc: &Doc) -> Result<(), String> {
        let s = doc.objstm_stats();
        let decoded = s.decoded_bytes.saturating_add(s.stored_bytes);
        if doc.rows > self.rows || decoded > self.decoded {
            return Err(xref::TOO_MANY.into());
        }
        self.rows -= doc.rows;
        self.decoded -= decoded;
        Ok(())
    }
}

/// Whether the cross-reference data has `n` in use.
fn in_use(doc: &Doc, n: u32) -> bool {
    matches!(doc.entries.get(&n), Some(Entry::Offset(..) | Entry::InStream(..)))
}

/// Whether the header of object `n` at generation `g` stands at `off`, as `parse_indirect` reads one.
fn headed(d: &[u8], off: usize, n: u32, g: u16) -> bool {
    let mut lx = crate::pdf::parse::Lexer::new(d, off.min(d.len()));
    let mut number = || {
        lx.skip_ws();
        std::str::from_utf8(lx.regular_token()).ok().and_then(|t| t.parse::<u32>().ok())
    };
    number() == Some(n) && number() == Some(g as u32) && lx.expect_keyword(b"obj")
}

/// Save `k` of `of`, read on its own: the file cut at `end`, opened the way a reader opens it, with everything its
/// trailer reaches. It passes the gate as a file of one save, whatever the options say about conformance.
fn read<'a>(
    input: &'a [u8],
    end: usize,
    (k, of): (usize, usize),
    opt: &Options,
    budget: &mut Budget,
) -> Result<(Doc<'a>, Reached), String> {
    if opt.deadline.passed() {
        return Err(TIMED_OUT.into());
    }
    let mut doc = xref::open_with(&input[..end], Limits::once(opt.max_stream)).map_err(|_| OPENS.to_string())?;
    if let Some(Err(why)) = &doc.crypt {
        return Err(format!("encrypted: revision {k} of {of}: {why}"));
    }
    let objs = load_reachable(&doc);
    budget.take(&doc)?;
    let objs = objs.map_err(|e| format!("keeps earlier revisions: revision {k} of {of}: {e}"))?;
    // The saves are told apart already: this one is checked as a file of one save.
    doc.has_revisions = false;
    gate(&doc, &objs, true)?;
    // A /Root that names some other dictionary: PDFium opens no such file, and pypdf shows an earlier save's
    // catalog, which a file of the newest save alone no longer has.
    let root = match doc.trailer.get(b"Root") {
        Some(Obj::Ref(n, _)) => objs.get(n).map(|(_, o)| o),
        direct => direct,
    };
    if root.and_then(Obj::as_dict).and_then(|d| d.name(b"Type")) != Some(b"Catalog") {
        return Err(NOT_CATALOG.into());
    }
    // Every reference this save makes, a stream's /Length and the trailer's /Encrypt too.
    let mut refs = Vec::new();
    objs.values().for_each(|(_, o)| o.refs(&mut refs));
    for (key, v) in &doc.trailer.0 {
        if !matches!(key.as_slice(), b"Prev" | b"XRefStm" | b"Size") {
            v.refs(&mut refs);
        }
    }
    // The generation a number has in the rows (0 in an object stream), when it is in use.
    let generation = |n: u32| match doc.entries.get(&n) {
        Some(Entry::Offset(_, g)) => Some(*g),
        Some(Entry::InStream(..)) => Some(0),
        _ => None,
    };
    // A reference whose generation is not its object's: one reader gives null, another the object, a third looks
    // through the file's bytes, where an earlier save's object may still be. A row names its object stream by
    // number alone: readers that hold it to generation 0 keep an earlier save's stream of that number.
    let homes = doc.entries.values().filter_map(|e| match e {
        Entry::InStream(sn, _) => Some((*sn, 0)),
        _ => None,
    });
    if refs.iter().copied().chain(homes).any(|(n, g)| generation(n).is_some_and(|h| h != g)) {
        return Err(GENERATION.into());
    }
    // A reference to a number that is not in use: null here and for qpdf, an earlier save's object for readers
    // that keep its row, and Poppler looks through the bytes for it and then takes the last object of every
    // number, whatever the rows say.
    if refs.iter().any(|&(n, _)| generation(n).is_none()) {
        return Err(ABSENT.into());
    }
    // pypdf moves a table's rows to other numbers when one points at another number's object, and looks through
    // the bytes when one points at none.
    if doc.entries.iter().any(|(&n, e)| matches!(e, Entry::Offset(off, g) if !headed(&input[..end], *off, n, *g))) {
        return Err(ELSEWHERE.into());
    }
    Ok((doc, objs))
}

/// The gates of `keep` for a file whose earlier saves are to be dropped, and the list of those saves. `newest` is
/// what the whole file's trailer reaches.
pub(super) fn dropped(input: &[u8], opt: &Options, newest: &Reached) -> Result<Vec<Dropped>, String> {
    let saves = find(input, true)?;
    let mut budget = Budget::new(input, opt);
    let mut out = Vec::new();
    let mut start = 0;
    for (k, save) in saves.iter().enumerate() {
        let (_, objs) = read(input, save.end, (k, saves.len()), opt, &mut budget)?;
        if k + 1 < saves.len() {
            let versions = objs.iter().filter(|(n, v)| newest.get(n) != Some(v)).count();
            out.push(Dropped { start, end: save.end, versions });
        }
        start = save.end;
    }
    Ok(out)
}

/// Each distinct value of the file's objects, held once however many revisions reach it. Values are found by value
/// in a map that is only looked up, never walked, so its random hash keys can't reach the output.
#[derive(Default)]
struct Values {
    list: Vec<Arc<Obj>>,
    index: HashMap<Arc<Obj>, usize>,
    /// Stored bytes of the distinct streams.
    stored: usize,
}

impl Values {
    /// Where `o` is held. `before` is a value it is likely to be: compared first, which costs less than hashing.
    fn intern(&mut self, o: Obj, before: Option<usize>) -> usize {
        if let Some(i) = before.filter(|&i| *self.list[i] == o) {
            return i;
        }
        match self.index.entry(Arc::new(o)) {
            std::collections::hash_map::Entry::Occupied(e) => *e.get(),
            std::collections::hash_map::Entry::Vacant(e) => {
                if let Obj::Stream(s) = &**e.key() {
                    self.stored = self.stored.saturating_add(s.data.len());
                }
                self.list.push(e.key().clone());
                *e.insert(self.list.len() - 1)
            }
        }
    }
}

/// What decides a stream's new encoding: its value, and the values its revision gives the objects its encoders
/// look up (see `job_refs`). One stream whose lookups change from a revision to the next is encoded for each, and
/// written again where they change.
type JobKey = (usize, Vec<(u32, usize)>);

/// An object of a revision: its generation, its value, and its job when it is a stream.
type Def = (u16, usize, Option<usize>);

/// Where object `n` is in a revision's objects, which are in ascending order of number.
fn at(objs: &[(u32, Def)], n: u32) -> Option<usize> {
    objs.binary_search_by_key(&n, |o| o.0).ok()
}

/// One revision, as read.
struct Rev {
    /// Where the input cut at this revision ends.
    end: usize,
    trailer: Dict,
    /// What its trailer reaches, in ascending order of number.
    objs: Vec<(u32, Def)>,
    /// Numbers the revisions before it reach that it no longer has in use.
    frees: Vec<u32>,
}

/// A writer's mistake that the tests plant, to see `check` catch it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fault {
    /// One byte of a stream's data changed in the first revision.
    StreamByte,
    /// An object a later revision changes left as the revision before wrote it.
    Unwritten,
    /// The free row of an object a later revision takes away left out.
    NoFreeRow,
    /// A key left out of the first revision's trailer.
    LostKey,
    /// A key added to a later revision's trailer.
    AddedKey,
    /// A row of a later revision pointing one byte early.
    RowOff,
}

#[cfg(test)]
fn planted(f: Fault) -> bool {
    tests::FAULT.with(|c| c.get() == Some(f))
}

#[cfg(not(test))]
fn planted(_: Fault) -> bool {
    false
}

/// Every revision kept, compressed and checked.
pub(super) fn keep(input: &[u8], opt: &Options) -> Result<(Vec<u8>, Stats), String> {
    let saves = find(input, false)?;
    let of = saves.len();
    let mut budget = Budget::new(input, opt);

    // Every revision is read, one at a time: only the distinct values and each revision's table of them stay.
    let mut values = Values::default();
    let mut revs: Vec<Rev> = Vec::new();
    let mut keys: Vec<(u32, JobKey)> = Vec::new();
    let mut jobs_by_key: HashMap<JobKey, usize> = HashMap::new();
    // The numbers the revisions so far reach, so the ones the output defines.
    let mut known: BTreeSet<u32> = BTreeSet::new();
    let mut crypt: Option<Crypt> = None;
    let (mut top, mut version) = (0u32, (1, 5));
    for (k, save) in saves.iter().enumerate() {
        let (doc, objs) = read(input, save.end, (k, of), opt, &mut budget)?;
        // Every revision opens with the same encryption, or none is encrypted.
        let c = doc.crypt.as_ref().and_then(|c| c.as_ref().ok());
        if k == 0 {
            crypt = c.cloned();
        } else if crypt.as_ref() != c {
            return Err(ENCRYPTION.into());
        }
        let listed = doc.entries.keys().next_back().copied().unwrap_or(0);
        top = top.max(listed).max(highest_number(&doc, &objs, c));
        let (kept, frees): (Vec<u32>, Vec<u32>) = known.iter().partition(|&&n| in_use(&doc, n));
        known = kept.into_iter().chain(objs.keys().copied()).collect();
        // What each stream's job is given, by number, before the values move into the table.
        let looked: Vec<(u32, Vec<u32>)> = objs
            .iter()
            .filter_map(|(&n, (_, o))| match o {
                Obj::Stream(s) => Some((n, job_refs(&s.dict, &objs).iter().map(|r| r.0).collect())),
                _ => None,
            })
            .collect();
        // Most objects are the ones the revision before has under their number.
        let before = |n: u32| revs.last().and_then(|r| Some(r.objs[at(&r.objs, n)?].1 .1));
        let mut table: Vec<(u32, Def)> =
            objs.into_iter().map(|(n, (g, o))| (n, (g, values.intern(o, before(n)), None))).collect();
        if values.stored > input.len() {
            return Err(xref::TOO_MANY.into());
        }
        for (n, refs) in looked {
            let value = |n: u32| at(&table, n).map(|i| table[i].1 .1);
            let Some(v) = value(n) else { continue };
            let key: JobKey = (v, refs.iter().filter_map(|&m| Some((m, value(m)?))).collect());
            let job = *jobs_by_key.entry(key.clone()).or_insert_with(|| {
                keys.push((n, key));
                keys.len() - 1
            });
            if let Some(i) = at(&table, n) {
                table[i].1 .2 = Some(job);
            }
        }
        version = doc.version;
        revs.push(Rev { end: save.end, trailer: doc.trailer.clone(), objs: table, frees });
    }
    drop(jobs_by_key);
    let crypt = crypt.as_ref();

    // One pool of threads, one plan and one store of encoded outputs for the streams of every revision, as for a
    // file of one save (see `rewrite`).
    let workers = par::Pool::new(if opt.threads == 0 { par::default_threads() } else { opt.threads });
    let pool = Some(&*workers);
    let opt = &Options {
        flate: flate::Options {
            shared: Some(flate::deflate::Shared::with_pool(workers.clone(), false)),
            deadline: opt.deadline,
            ..opt.flate.clone()
        },
        ..opt.clone()
    };
    let streams: Vec<(u32, &Stream)> = keys
        .iter()
        .filter_map(|(n, key)| if let Obj::Stream(s) = &*values.list[key.0] { Some((*n, &**s)) } else { None })
        .collect();
    let unique: Vec<usize> = (0..streams.len()).collect();
    let settings = effort_plan(&streams, &unique, opt, input.len(), pool);
    let jobs: Vec<Job> = streams
        .iter()
        .zip(&keys)
        .zip(settings)
        .map(|((&(n, s), (_, key)), settings)| Job {
            n,
            s,
            settings,
            // XMP stays as it is unless it is encrypted, which its dictionary and the encryption decide (see
            // `rewrite`): the same in every revision.
            xmp: s.dict.name(b"Type") == Some(b"Metadata")
                && crypt.is_some_and(|c| c.stream_method(n, &s.dict).is_ok_and(|m| m != Method::Identity)),
            shares: false,
            // `may_index` reads one object table; a kept revision's images stay as they are coloured.
            palette: false,
            refs: key
                .1
                .iter()
                .map(|&(m, v)| match &*values.list[v] {
                    Obj::Stream(s) => (m, Looked::Dict(&s.dict)),
                    o => (m, Looked::Obj(o)),
                })
                .collect(),
        })
        .collect();
    let order = largest_first(&jobs);
    let dbg = std::env::var_os("PDFE_DEBUG").is_some();
    let store = flate::deflate::Shared::with_store(workers.clone(), opt.store_budget);
    let done = par::map(pool, jobs.len(), Some(&order), |u| run_job(&jobs[u], &store, opt, dbg));
    drop((jobs, store));
    let (encoded, proofs): (Vec<_>, Vec<_>) = done.into_iter().unzip();
    if opt.deadline.passed() {
        return Err(TIMED_OUT.into());
    }

    // The file: each revision after the one before it.
    let mut stats = Stats { history_kept: of - 1, ..Default::default() };
    stats.encryption = crypt.map(|c| c.describe()).unwrap_or_default();
    let minor = if version.0 == 1 { version.1.max(5) } else { version.1 };
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(format!("%PDF-{}.{}\n", version.0, minor).as_bytes());
    out.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
    // New objects are numbered above every number any revision lists, holds or refers to, within the rows a
    // file of this size may have (see `gate`).
    let first_new = top as u64 + 1;
    let mut next = first_new;
    let mut number = || -> Result<u32, String> {
        let n = u32::try_from(next).ok().filter(|_| next < xref::row_limit(input.len()) as u64);
        next += 1;
        n.ok_or_else(|| "object numbers far apart".to_string())
    };
    // What the output defines so far, by the input value each number was written from. Only looked up.
    let mut defined: HashMap<u32, Def> = HashMap::new();
    let mut ends = Vec::with_capacity(of);
    let mut prev = None;
    let mut objstm_bytes = 0usize;
    for (k, rev) in revs.iter().enumerate() {
        if opt.deadline.passed() {
            return Err(TIMED_OUT.into());
        }
        for n in &rev.frees {
            defined.remove(n);
        }
        // Every object this revision reaches that the output doesn't define yet, or defines from another value.
        let mut direct: BTreeMap<u32, (u16, Vec<u8>)> = BTreeMap::new();
        let mut packable: Vec<(u32, &Obj)> = Vec::new();
        for &(n, def) in &rev.objs {
            if defined.get(&n) == Some(&def) || k > 0 && defined.contains_key(&n) && planted(Fault::Unwritten) {
                continue;
            }
            defined.insert(n, def);
            stats.objects += 1;
            let (g, v, job) = def;
            match &*values.list[v] {
                Obj::Stream(s) => {
                    stats.streams += 1;
                    let (dict, mut data) = match job.and_then(|j| encoded[j].as_ref()) {
                        Some((d, z, change)) => {
                            stats.streams_reencoded += 1;
                            stats.jpegs_retabled += change.jpeg_retabled as usize;
                            stats.fax_to_jbig2 += change.fax_to_jbig2 as usize;
                            (d, Cow::Borrowed(z.as_slice()))
                        }
                        None => {
                            stats.streams_kept += 1;
                            (&s.dict, Cow::Borrowed(s.data.as_slice()))
                        }
                    };
                    if k == 0 && planted(Fault::StreamByte) {
                        if let Some(b) = data.to_mut().last_mut() {
                            *b ^= 1;
                        }
                    }
                    // Encrypted again with this object's key.
                    let (mut dict, data) = match crypt {
                        Some(c) => {
                            let sealed = c.seal_stream_at(k as u32, n, g, dict, &data);
                            let (d, z) = sealed.map_err(|e| format!("object {n}: {e}"))?;
                            (d, Cow::Owned(z))
                        }
                        None => (dict.clone(), data),
                    };
                    dict.set(b"Length", Obj::Int(data.len() as i64));
                    let mut body = Vec::with_capacity(data.len() + 64);
                    Writer { out: &mut body }.dict(&dict);
                    body.extend_from_slice(b"stream\n");
                    body.extend_from_slice(&data);
                    body.extend_from_slice(b"\nendstream");
                    direct.insert(n, (g, body));
                }
                o if g != 0 => {
                    let body = match crypt {
                        Some(c) => {
                            to_bytes(&c.seal_object_at(k as u32, n, g, o).map_err(|e| format!("object {n}: {e}"))?)
                        }
                        None => to_bytes(o),
                    };
                    direct.insert(n, (g, body));
                }
                o => packable.push((n, o)),
            }
        }

        // Its own object streams, grouped by kind.
        by_kind(&mut packable);
        // The rows of this revision's section: one for each object written, each object taken away, and itself.
        let mut rows: Vec<(u32, u8, u64, u64)> = Vec::new();
        let mut raws = Vec::new();
        for chunk in packable.chunks(opt.objstm_size.max(1)) {
            let sn = number()?;
            rows.extend(chunk.iter().enumerate().map(|(i, (n, _))| (*n, 2, sn as u64, i as u64)));
            let (raw, first) = objstm_raw(chunk);
            raws.push((sn, chunk.len(), first, raw));
        }
        objstm_bytes += raws.iter().map(|r| r.3.len()).sum::<usize>();
        let structure = structure_flate(opt, objstm_bytes, input.len());
        let zs = deflate_each(pool, &raws.iter().map(|r| r.3.as_slice()).collect::<Vec<_>>(), &structure);
        for (&(sn, count, first, _), z) in raws.iter().zip(zs) {
            direct.insert(sn, (0, objstm_body(crypt, k as u32, sn, (count, first), z)?));
            stats.objstms += 1;
        }
        drop(raws);
        // The encryption dictionary and its objects as they were, once: every revision has the same.
        if let Some(c) = crypt.filter(|_| k == 0) {
            if let Some((n, g)) = c.obj {
                direct.insert(n, (g, to_bytes(&Obj::Dict(c.dict.clone()))));
            }
            for (n, g, o) in &c.parts {
                direct.insert(*n, (*g, to_bytes(o)));
            }
        }

        if k == 0 {
            rows.push((0, 0, 0, 65535));
        }
        if !planted(Fault::NoFreeRow) {
            rows.extend(rev.frees.iter().map(|&n| (n, 0, 0, 0)));
        }
        for (n, (g, body)) in direct {
            let at = out.len() - (k > 0 && planted(Fault::RowOff)) as usize;
            rows.push((n, 1, at as u64, g as u64));
            out.extend_from_slice(format!("{n} {g} obj\n").as_bytes());
            out.extend_from_slice(&body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xn = number()?;
        let xref_off = out.len();
        rows.push((xn, 1, xref_off as u64, 0));
        rows.sort_unstable_by_key(|r| r.0);
        let w1 = bytes_for(rows.iter().map(|r| r.2).max().unwrap_or(0));
        let w2 = bytes_for(rows.iter().map(|r| r.3).max().unwrap_or(0));
        let cols = 1 + w1 + w2;
        let mut plain = Vec::with_capacity(rows.len() * cols);
        // /Index: the runs of numbers in a row, ascending.
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for &(n, t, f2, f3) in &rows {
            plain.push(t);
            plain.extend_from_slice(&f2.to_be_bytes()[8 - w1..]);
            plain.extend_from_slice(&f3.to_be_bytes()[8 - w2..]);
            match runs.last_mut() {
                Some((first, count)) if *first + *count == n => *count += 1,
                _ => runs.push((n, 1)),
            }
        }
        let mut xd = Dict::default();
        xd.set(b"Type", Obj::Name(b"XRef".to_vec()));
        xd.set(b"Size", Obj::Int(xn as i64 + 1));
        xd.set(b"W", Obj::Array(vec![Obj::Int(1), Obj::Int(w1 as i64), Obj::Int(w2 as i64)]));
        xd.set(
            b"Index",
            Obj::Array(
                runs.iter().flat_map(|&(first, count)| [Obj::Int(first as i64), Obj::Int(count as i64)]).collect(),
            ),
        );
        if let Some(prev) = prev {
            xd.set(b"Prev", Obj::Int(prev as i64));
        }
        // Every entry of this revision's trailer that isn't about the old cross-reference structure carries over.
        for (key, v) in &rev.trailer.0 {
            if !xref::STRUCTURAL.contains(&key.as_slice()) && !(k == 0 && key != b"Root" && planted(Fault::LostKey)) {
                xd.set(key, v.clone());
            }
        }
        if k > 0 && planted(Fault::AddedKey) {
            xd.set(b"Added", Obj::Null);
        }
        let structure = |more| structure_flate(opt, objstm_bytes + more, input.len());
        write_xref(&mut out, xn, xd, (&plain, cols), &structure, pool)?;
        if out.len() >= input.len() {
            return Err("no saving possible".into());
        }
        // qpdf 12 reads an object only when its number is below a third of the file's length (rounded down), and
        // can't open a file whose cross-reference stream has a higher one. The new numbers lie above every
        // revision's, so a short early revision under a long later one could have one: the output cut at each
        // revision's end stays below.
        if xn as usize >= out.len() / 3 {
            return Err(TOO_HIGH.into());
        }
        ends.push(out.len());
        prev = Some(xref_off);
    }
    drop(encoded);

    // Of the values, the check needs only the streams whose jobs checked their own encoding.
    let proved: Vec<Option<(Arc<Obj>, Proof)>> =
        keys.iter().zip(proofs).map(|((_, key), proof)| Some((values.list[key.0].clone(), proof?))).collect();
    drop(values);
    let taken = check(input, &out, &ends, &revs, &proved, first_new, opt, pool)?;
    if dbg {
        eprintln!("revisions: {of} kept; self-check: {taken} stream comparisons taken on their jobs' checks");
    }
    // A rewrite either finishes in time or gives nothing (see `rewrite`).
    if opt.deadline.passed() {
        return Err(TIMED_OUT.into());
    }
    Ok((out, stats))
}

/// Reads the output back, revision by revision, and compares each with the input's: the output cut at a revision's
/// end against the input cut at that revision's end, read again from the file (not what `keep` wrote from). The
/// output has the same revisions, ending where they were written; each section lists every number once, in
/// ascending order, points exactly at its objects, covers them with its /Size and has no trailer entry the input's
/// trailer lacks; no object of the input's numbers is in use that the input doesn't have in use; no object that is
/// missing from a revision can be found in its bytes; and everything a revision reaches has the input's generation
/// and value (`self_check`). A stream is taken on its job's check only when the input's stream, compared here by
/// value, is the one the job encoded. Gives how many streams were.
#[allow(clippy::too_many_arguments)]
fn check(
    input: &[u8],
    out: &[u8],
    ends: &[usize],
    revs: &[Rev],
    proved: &[Option<(Arc<Obj>, Proof)>],
    first_new: u64,
    opt: &Options,
    pool: Option<&par::Pool>,
) -> Result<usize, String> {
    let of = revs.len();
    let found = xref::saves(out).map_err(|_| "self-check: the output's revisions can't be told apart".to_string())?;
    if found.len() != of || ends.len() != of || found.iter().zip(ends).any(|(f, &end)| !f.linked || f.end != end) {
        return Err("self-check: the output's revisions are not the input's".into());
    }
    let mut budget = Budget::new(input, opt);
    let (mut taken, mut size_before) = (0, 0);
    for (k, rev) in revs.iter().enumerate() {
        let fail = |why: &str| format!("self-check: revision {k} of {of}: {why}");
        if opt.deadline.passed() {
            return Err(TIMED_OUT.into());
        }
        let doc =
            xref::open_with(&input[..rev.end], Limits::once(opt.max_stream)).map_err(|_| fail("input unreadable"))?;
        let objs = load_reachable(&doc);
        budget.take(&doc)?;
        let objs = objs.map_err(|e| fail(&e))?;
        let cut = &out[..ends[k]];

        let (rows, dict) = xref::section(cut, found[k].section).ok_or_else(|| fail("section unreadable"))?;
        if !rows.windows(2).all(|w| w[0].0 < w[1].0) {
            return Err(fail("a number listed twice or out of order"));
        }
        let points_at = |off: usize, n: u32, g: u16| {
            cut.get(off..).is_some_and(|t| t.starts_with(format!("{n} {g} obj\n").as_bytes()))
                && !off.checked_sub(1).and_then(|b| cut.get(b)).is_some_and(u8::is_ascii_digit)
        };
        if rows.iter().any(|&(n, e)| matches!(e, Entry::Offset(off, g) if !points_at(off, n, g))) {
            return Err(fail("a row that doesn't point at its object"));
        }
        let size = dict.get(b"Size").and_then(Obj::as_int).unwrap_or(0);
        if rows.last().is_some_and(|r| r.0 as i64 >= size) || size < size_before {
            return Err(fail("/Size too small"));
        }
        size_before = size;
        // `self_check` finds every entry of the input's trailer in the output's; nothing else may be there.
        if dict.0.iter().any(|(key, _)| !xref::STRUCTURAL.contains(&key.as_slice()) && !doc.trailer.has(key)) {
            return Err(fail("trailer differs"));
        }
        let doc2 = xref::open(cut).map_err(|_| fail("output unreadable"))?;
        if doc2.entries.keys().any(|&n| (n as u64) < first_new && in_use(&doc2, n) && !in_use(&doc, n)) {
            return Err(fail("an object the input doesn't have"));
        }
        drop(doc2);

        let proven: Vec<(u32, Proof)> = objs
            .iter()
            .filter_map(|(&n, (_, o))| {
                let (encoded, p) = proved.get(rev.objs[at(&rev.objs, n)?].1 .2?)?.as_ref()?;
                (**encoded == *o)
                    .then(|| (n, Proof { of: n, sha: p.sha, filter: p.filter.clone(), parms: p.parms.clone() }))
            })
            .collect();
        let proven: Vec<(u32, &Proof)> = proven.iter().map(|(n, p)| (*n, p)).collect();
        let crypt = doc.crypt.as_ref().and_then(|c| c.as_ref().ok());
        taken += self_check(&objs, cut, &doc.trailer, crypt, &proven, pool)
            .map_err(|e| fail(e.strip_prefix("self-check: ").unwrap_or(&e)))?;
    }
    Ok(taken)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        /// The mistake `keep` makes on this thread (see `planted`).
        pub(super) static FAULT: Cell<Option<Fault>> = const { Cell::new(None) };
    }

    /// A table with a row for each (number, offset) and a free row for each of `free`, then its trailer.
    fn table(d: &mut Vec<u8>, rows: &[(u32, usize)], free: &[u32], trailer: &str) -> usize {
        let x = d.len();
        d.extend_from_slice(b"xref\n");
        for (n, off) in rows {
            d.extend_from_slice(format!("{n} 1\n{off:010} 00000 n \n").as_bytes());
        }
        for n in free {
            d.extend_from_slice(format!("{n} 1\n0000000000 00000 f \n").as_bytes());
        }
        d.extend_from_slice(format!("trailer\n<< {trailer} >>\nstartxref\n{x}\n%%EOF\n").as_bytes());
        x
    }

    /// A file of two saves: a page with its content (4) and font (5), then an update that writes the page (3) and
    /// its content again, adds document information (6) and takes the font away.
    fn two_saves() -> Vec<u8> {
        let content = |t: &str| {
            let text = format!("BT /F1 12 Tf 10 10 Td ({t}) Tj ET\n").repeat(50);
            format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len())
        };
        let mut d = b"%PDF-1.5\n".to_vec();
        let put = |d: &mut Vec<u8>, objs: &[(u32, String)]| -> Vec<(u32, usize)> {
            objs.iter()
                .map(|(n, body)| {
                    let at = d.len();
                    d.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
                    (*n, at)
                })
                .collect()
        };
        let page = "/Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R";
        let first = put(
            &mut d,
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
                (3, format!("<< {page} /Resources << /Font << /F1 5 0 R >> >> >>")),
                (4, content("FIRST SAVE")),
                (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
            ],
        );
        let x = table(&mut d, &first, &[], "/Size 6 /Root 1 0 R /ID [<01> <02>]");
        let second = put(
            &mut d,
            &[(3, format!("<< {page} >>")), (4, content("SECOND SAVE")), (6, "<< /Title (Second save) >>".into())],
        );
        table(&mut d, &second, &[5], &format!("/Size 7 /Root 1 0 R /ID [<01> <02>] /Info 6 0 R /Prev {x}"));
        d
    }

    fn outcome(pdf: &[u8], fault: Option<Fault>) -> Outcome {
        FAULT.set(fault);
        let r = optimize(pdf, &Options { threads: 1, ..Options::effort(0) });
        FAULT.set(None);
        r.outcome
    }

    #[test]
    fn the_check_catches_each_mistake_of_the_writer() {
        let pdf = two_saves();
        assert!(matches!(outcome(&pdf, None), Outcome::Smaller(s) if s.history_kept == 1));
        let faults = [
            (Fault::StreamByte, 0),
            (Fault::Unwritten, 1),
            (Fault::NoFreeRow, 1),
            (Fault::LostKey, 0),
            (Fault::AddedKey, 1),
            (Fault::RowOff, 1),
        ];
        for (fault, revision) in faults {
            let found = outcome(&pdf, Some(fault));
            let why = format!("self-check: revision {revision} of 2: ");
            assert!(matches!(&found, Outcome::Untouched(w) if w.starts_with(&why)), "{fault:?}: {found:?}");
        }
    }

    #[test]
    fn a_marker_anywhere_in_the_bytes_is_found() {
        assert_eq!(marker(b"<< /Type /Sig /ByteRange [0 1 2 3] >>"), Some("digitally signed"));
        assert_eq!(marker(b"/ByteRange and /DocMDP"), Some("has usage rights or certification"));
        assert_eq!(marker(b"<rdf:Description xmlns:pdfaid=\"x\"><pdfaid:part>1"), Some("PDF/A"));
        assert_eq!(marker(b"/OutputIntents [<< /S /GTS_PDFX >>]"), Some("PDF/X"));
        // An AES-256 file's /Perms and a namespace nobody claims are not claims.
        assert_eq!(marker(b"<< /Perms <00> >> xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\""), None);
    }
}
