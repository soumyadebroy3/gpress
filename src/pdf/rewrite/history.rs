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

/// The reason `gate` gives for a file with earlier saves, and this module for one whose saves can't be told apart.
pub(super) const KEEPS: &str = xref::KEEPS;
const UNLINKED: &str = "keeps earlier revisions: a save the newest doesn't link";
const OPENS: &str = "keeps earlier revisions: a save that doesn't open on its own";
const GENERATION: &str = "keeps earlier revisions: a reference to another generation";
const TOO_HIGH: &str = "keeps earlier revisions: numbers too high for an early revision";
const ENCRYPTION: &str = "encrypted: revisions differ in their encryption";

/// An earlier save that `Options::drop_history` dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dropped {
    /// Its place among the file's saves, the oldest 0.
    pub index: usize,
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
        return Err(KEEPS.into());
    }
    if !drop && saves.iter().any(|s| !s.linked) {
        return Err(UNLINKED.into());
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

/// Every reference the objects make (a stream's /Length aside: it is written as a number).
fn references(objs: &BTreeMap<u32, (u16, Obj)>) -> Vec<(u32, u16)> {
    let mut refs = Vec::new();
    for (_, o) in objs.values() {
        match o {
            Obj::Stream(s) => s.dict.0.iter().filter(|(k, _)| k != b"Length").for_each(|(_, v)| v.refs(&mut refs)),
            _ => o.refs(&mut refs),
        }
    }
    refs
}

/// Save `k` of `of`, read on its own: the file cut at `end`, opened the way a reader opens it, with everything its
/// trailer reaches. It passes the gate as a file of one save, whatever the options say about conformance.
fn read<'a>(
    input: &'a [u8],
    end: usize,
    (k, of): (usize, usize),
    opt: &Options,
    budget: &mut Budget,
) -> Result<(Doc<'a>, BTreeMap<u32, (u16, Obj)>), String> {
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
    // A reference whose generation is not its object's: one reader gives null, another the object, a third looks
    // through the file's bytes, where an earlier save's object may still be.
    let mut refs = references(&objs);
    for (key, v) in &doc.trailer.0 {
        if !matches!(key.as_slice(), b"Prev" | b"XRefStm" | b"Size" | b"Encrypt") {
            v.refs(&mut refs);
        }
    }
    if refs.iter().any(|(n, g)| objs.get(n).is_some_and(|o| o.0 != *g)) {
        return Err(GENERATION.into());
    }
    Ok((doc, objs))
}

/// The gates of `keep` for a file whose earlier saves are to be dropped, and the list of those saves. `newest` is
/// what the whole file's trailer reaches.
pub(super) fn dropped(input: &[u8], opt: &Options, newest: &BTreeMap<u32, (u16, Obj)>) -> Result<Vec<Dropped>, String> {
    let saves = find(input, true)?;
    let mut budget = Budget::new(input, opt);
    let mut out = Vec::new();
    let mut start = 0;
    for (k, save) in saves.iter().enumerate() {
        let (_, objs) = read(input, save.end, (k, saves.len()), opt, &mut budget)?;
        if k + 1 < saves.len() {
            let versions = objs.iter().filter(|(n, v)| newest.get(n) != Some(v)).count();
            out.push(Dropped { index: k, start, end: save.end, versions });
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
    fn intern(&mut self, o: Obj) -> usize {
        if let Some(&i) = self.index.get(&o) {
            return i;
        }
        if let Obj::Stream(s) = &o {
            self.stored = self.stored.saturating_add(s.data.len());
        }
        let o = Arc::new(o);
        self.list.push(o.clone());
        self.index.insert(o, self.list.len() - 1);
        self.list.len() - 1
    }
}

/// What decides a stream's new encoding: its value, whether it is encrypted XMP (see `reencode_xmp`), and the
/// values its revision gives the objects its encoders look up (see `job_refs`). One stream whose lookups change
/// from a revision to the next is encoded for each, and written again where they change.
type JobKey = (usize, bool, Vec<(u32, usize)>);

/// An object of a revision: its generation, its value, and its job when it is a stream.
type Def = (u16, usize, Option<usize>);

/// One revision, as read.
struct Rev {
    /// Where the input cut at this revision ends.
    end: usize,
    trailer: Dict,
    /// What its trailer reaches.
    objs: BTreeMap<u32, Def>,
    /// Numbers the revisions before it reach that it no longer has in use.
    frees: Vec<u32>,
    /// The highest number its cross-reference data lists.
    listed: u32,
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
    let mut crypt: Option<Option<Crypt>> = None;
    let (mut top, mut reached, mut version) = (0u32, 0usize, (1, 5));
    for (k, save) in saves.iter().enumerate() {
        let (doc, objs) = read(input, save.end, (k, of), opt, &mut budget)?;
        // Every revision opens with the same encryption, or none is encrypted.
        let c = doc.crypt.as_ref().and_then(|c| c.as_ref().ok());
        match &crypt {
            None => crypt = Some(c.cloned()),
            Some(first) if first.as_ref() != c => return Err(ENCRYPTION.into()),
            Some(_) => {}
        }
        reached = reached.saturating_add(objs.len());
        if reached > xref::row_limit(input.len()) {
            return Err(xref::TOO_MANY.into());
        }
        let listed = doc.entries.keys().next_back().copied().unwrap_or(0);
        top = top.max(listed).max(highest_number(&doc, &objs, c));
        let frees: Vec<u32> = known.iter().copied().filter(|&n| !in_use(&doc, n)).collect();
        for n in &frees {
            known.remove(n);
        }
        known.extend(objs.keys());
        // What each stream's job is given, by number, before the values move into the table.
        let looked: Vec<(u32, bool, Vec<u32>)> = objs
            .iter()
            .filter_map(|(&n, (_, o))| {
                let Obj::Stream(s) = o else { return None };
                let xmp = s.dict.name(b"Type") == Some(b"Metadata")
                    && c.is_some_and(|c| c.stream_method(n, &s.dict).is_ok_and(|m| m != Method::Identity));
                Some((n, xmp, job_refs(&s.dict, &objs).iter().map(|r| r.0).collect()))
            })
            .collect();
        let mut table: BTreeMap<u32, Def> =
            objs.into_iter().map(|(n, (g, o))| (n, (g, values.intern(o), None))).collect();
        if values.stored > input.len() {
            return Err(xref::TOO_MANY.into());
        }
        for (n, xmp, refs) in looked {
            let value = |n: &u32| table.get(n).map(|d| d.1);
            let Some(v) = value(&n) else { continue };
            let key: JobKey = (v, xmp, refs.iter().filter_map(|m| Some((*m, value(m)?))).collect());
            let job = *jobs_by_key.entry(key.clone()).or_insert_with(|| {
                keys.push((n, key));
                keys.len() - 1
            });
            if let Some(d) = table.get_mut(&n) {
                d.2 = Some(job);
            }
        }
        version = doc.version;
        revs.push(Rev { end: save.end, trailer: doc.trailer.clone(), objs: table, frees, listed });
    }
    drop(jobs_by_key);
    let crypt = crypt.flatten();
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
            xmp: key.1,
            shares: false,
            refs: key
                .2
                .iter()
                .map(|&(m, v)| match &*values.list[v] {
                    Obj::Stream(s) => (m, Looked::Dict(&s.dict)),
                    o => (m, Looked::Obj(o)),
                })
                .collect(),
        })
        .collect();
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    order.sort_by_key(|&u| (std::cmp::Reverse(jobs[u].s.data.len()), u));
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
    // What the output defines so far, by the input value each number was written from.
    let mut defined: BTreeMap<u32, Def> = BTreeMap::new();
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
        for (&n, &def) in &rev.objs {
            if defined.get(&n) == Some(&def) || k > 0 && defined.contains_key(&n) && planted(Fault::Unwritten) {
                continue;
            }
            defined.insert(n, def);
            stats.objects += 1;
            let (g, v, job) = def;
            match &*values.list[v] {
                Obj::Stream(s) => {
                    stats.streams += 1;
                    let (dict, mut data) = match job.and_then(|j| encoded[j].clone()) {
                        Some((d, z, change)) => {
                            stats.streams_reencoded += 1;
                            stats.jpegs_retabled += change.jpeg_retabled as usize;
                            stats.fax_to_jbig2 += change.fax_to_jbig2 as usize;
                            (d, z)
                        }
                        None => {
                            stats.streams_kept += 1;
                            (s.dict.clone(), s.data.clone())
                        }
                    };
                    if k == 0 && planted(Fault::StreamByte) {
                        if let Some(b) = data.last_mut() {
                            *b ^= 1;
                        }
                    }
                    // Encrypted again with this object's key.
                    let (mut dict, data) = match crypt {
                        Some(c) => {
                            c.seal_stream_at(k as u32, n, g, &dict, &data).map_err(|e| format!("object {n}: {e}"))?
                        }
                        None => (dict, data),
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
        packable.sort_by(|a, b| kind_key(a.1).cmp(&kind_key(b.1)).then(a.0.cmp(&b.0)));
        let mut in_stm: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
        let mut raws = Vec::new();
        for chunk in packable.chunks(opt.objstm_size.max(1)) {
            let sn = number()?;
            let mut head = Vec::new();
            let mut body = Vec::new();
            for (i, (n, o)) in chunk.iter().enumerate() {
                if i > 0 {
                    body.push(b'\n');
                }
                head.extend_from_slice(format!("{} {} ", n, body.len()).as_bytes());
                body.extend_from_slice(&to_bytes(o));
                in_stm.insert(*n, (sn, i as u32));
            }
            let first = head.len();
            head.extend_from_slice(&body);
            raws.push((sn, chunk.len(), first, head));
        }
        objstm_bytes += raws.iter().map(|r| r.3.len()).sum::<usize>();
        let structure = structure_flate(opt, objstm_bytes, input.len());
        let zs = par::map(pool, raws.len(), None, |i| flate::deflate_zlib(&raws[i].3, &structure));
        for (&(sn, count, first, _), z) in raws.iter().zip(zs) {
            let mut d = Dict::default();
            d.set(b"Type", Obj::Name(b"ObjStm".to_vec()));
            d.set(b"N", Obj::Int(count as i64));
            d.set(b"First", Obj::Int(first as i64));
            d.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
            // Encrypted as a stream, under its own number; the objects inside are not encrypted on their own.
            let (mut d, z) = match crypt {
                Some(c) => c.seal_stream_at(k as u32, sn, 0, &d, &z).map_err(|e| format!("object stream: {e}"))?,
                None => (d, z),
            };
            d.set(b"Length", Obj::Int(z.len() as i64));
            let mut body = Vec::new();
            Writer { out: &mut body }.dict(&d);
            body.extend_from_slice(b"stream\n");
            body.extend_from_slice(&z);
            body.extend_from_slice(b"\nendstream");
            direct.insert(sn, (0, body));
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

        // The rows of this revision's section: one for each object written, each object taken away, and itself.
        let mut rows: Vec<(u32, u8, u64, u64)> = Vec::new();
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
        rows.extend(in_stm.iter().map(|(&n, &(sn, i))| (n, 2, sn as u64, i as u64)));
        let xn = number()?;
        let xref_off = out.len();
        rows.push((xn, 1, xref_off as u64, 0));
        rows.sort_unstable();
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
        // With PNG "Up" prediction when that is smaller, its parameters counted (as in `rewrite`).
        let mut up = Vec::with_capacity(plain.len() + rows.len());
        for (r, row) in plain.chunks(cols).enumerate() {
            up.push(2u8);
            for (i, &b) in row.iter().enumerate() {
                let above = if r == 0 { 0 } else { plain[(r - 1) * cols + i] };
                up.push(b.wrapping_sub(above));
            }
        }
        let structure = structure_flate(opt, objstm_bytes + plain.len() + up.len(), input.len());
        let mut both = par::map(pool, 2, None, |i| flate::deflate_zlib(if i == 0 { &plain } else { &up }, &structure));
        let (Some(z_up), Some(z_plain)) = (both.pop(), both.pop()) else {
            return Err("cross-reference stream not encoded".into());
        };
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
        xd.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
        let mut p = Dict::default();
        p.set(b"Predictor", Obj::Int(12));
        p.set(b"Columns", Obj::Int(cols as i64));
        let parms = to_bytes(&Obj::Dict(p.clone())).len() + b"/DecodeParms".len();
        let zx = if z_up.len() + parms < z_plain.len() {
            xd.set(b"DecodeParms", Obj::Dict(p));
            z_up
        } else {
            z_plain
        };
        xd.set(b"Length", Obj::Int(zx.len() as i64));
        out.extend_from_slice(format!("{xn} 0 obj\n").as_bytes());
        Writer { out: &mut out }.dict(&xd);
        out.extend_from_slice(b"stream\n");
        out.extend_from_slice(&zx);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out.extend_from_slice(format!("startxref\n{xref_off}\n%%EOF\n").as_bytes());
        if out.len() >= input.len() {
            return Err("no saving possible".into());
        }
        // Readers refuse a file that lists a number past a third of its length (qpdf, without recovery). The new
        // numbers lie above every revision's, so a small early revision under a large later one would list one:
        // the file cut there must still open wherever the input cut there does.
        let too_high = |number: u32, len: usize| (number as u64 + 1) * 3 > len as u64;
        if too_high(xn, out.len()) && !too_high(rev.listed, rev.end) {
            return Err(TOO_HIGH.into());
        }
        ends.push(out.len());
        prev = Some(xref_off);
    }
    drop(encoded);

    let taken = check(input, &out, &ends, &revs, &values, &keys, &proofs, first_new, opt, pool)?;
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
    values: &Values,
    keys: &[(u32, JobKey)],
    proofs: &[Option<Proof>],
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
        let missing: HashSet<(u32, u16)> =
            references(&objs).into_iter().filter(|(n, _)| !objs.contains_key(n)).collect();
        if holds_header(cut, &missing) {
            return Err(fail("an object in the file is missing from its cross-reference data"));
        }

        let proven: Vec<(u32, Proof)> = objs
            .iter()
            .filter_map(|(&n, (_, o))| {
                let job = rev.objs.get(&n)?.2?;
                let p = proofs.get(job)?.as_ref()?;
                (*values.list[keys.get(job)?.1 .0] == *o)
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
    fn the_gate_gives_the_reason_this_module_takes_over_for() {
        let pdf = two_saves();
        let doc = xref::open(&pdf).unwrap();
        assert_eq!(gate(&doc, &load_reachable(&doc).unwrap(), true), Err(KEEPS.to_string()));
        assert!(matches!(outcome(&pdf, None), Outcome::Smaller(s) if s.history_kept == 1));
    }

    #[test]
    fn the_check_catches_each_mistake_of_the_writer() {
        let pdf = two_saves();
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
