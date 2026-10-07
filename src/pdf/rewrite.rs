//! Rewriting a PDF smaller without changing its content.
//!
//! The file is read strictly; anything we can't be sure about leaves the file untouched (the "gate"). Every
//! object reachable from the trailer is kept with the same number and value. Streams are re-encoded with our
//! Flate encoder when that is smaller; the data the decoder gives back is identical. Small objects are packed
//! into compressed object streams grouped by type, and the cross-reference table becomes a compressed
//! cross-reference stream. Before anything is returned, the output is read back and compared object by object.

use super::filters::{chain, decode_stage, is_general, Stage};
use super::images;
use super::object::*;
use super::xref::{self, Doc, Entry};
use crate::flate;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Debug)]
pub struct Options {
    pub flate: flate::Options,
    /// Most objects per object stream.
    pub objstm_size: usize,
    /// Largest decoded stream we re-encode.
    pub max_stream: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { flate: flate::Options::max(), objstm_size: 200, max_stream: 64 << 20 }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub objects: usize,
    pub streams: usize,
    pub streams_reencoded: usize,
    pub streams_kept: usize,
    pub objstms: usize,
    pub dropped_unreachable: usize,
}

#[derive(Debug)]
pub enum Outcome {
    Smaller(Stats),
    /// Returned unchanged, with the reason.
    Untouched(String),
}

pub struct Optimized {
    pub data: Vec<u8>,
    pub outcome: Outcome,
}

pub fn optimize(input: &[u8], opt: &Options) -> Optimized {
    match rewrite(input, opt) {
        Ok((out, stats)) => {
            if out.len() < input.len() {
                Optimized { data: out, outcome: Outcome::Smaller(stats) }
            } else {
                Optimized { data: input.to_vec(), outcome: Outcome::Untouched("no saving possible".into()) }
            }
        }
        Err(reason) => Optimized { data: input.to_vec(), outcome: Outcome::Untouched(reason) },
    }
}

/// Everything reachable from the trailer, in a stable order, with its generation and value.
fn load_reachable(doc: &Doc) -> Result<BTreeMap<u32, (u16, Obj)>, String> {
    let mut all = BTreeMap::new();
    let mut todo: Vec<u32> = Vec::new();
    let mut seed = Vec::new();
    for (k, v) in &doc.trailer.0 {
        if !matches!(k.as_slice(), b"Prev" | b"XRefStm" | b"Size" | b"Encrypt") {
            v.refs(&mut seed);
        }
    }
    todo.extend(seed.iter().map(|r| r.0));
    while let Some(n) = todo.pop() {
        if all.contains_key(&n) {
            continue;
        }
        let (g, o) = match doc.entries.get(&n) {
            None | Some(Entry::Free) => continue, // a dangling reference stays a null reference
            Some(_) => doc.load(n).map_err(|e| format!("unreadable object {n}: {e:?}"))?,
        };
        let mut refs = Vec::new();
        match &o {
            Obj::Stream(s) => {
                for (k, v) in &s.dict.0 {
                    if k != b"Length" {
                        v.refs(&mut refs);
                    }
                }
            }
            _ => o.refs(&mut refs),
        }
        todo.extend(refs.iter().map(|r| r.0).filter(|r| !all.contains_key(r)));
        all.insert(n, (g, o));
    }
    Ok(all)
}

fn walk<'o>(o: &'o Obj, f: &mut dyn FnMut(&'o Obj)) {
    f(o);
    match o {
        Obj::Array(a) => a.iter().for_each(|x| walk(x, f)),
        Obj::Dict(d) => d.0.iter().for_each(|(_, v)| walk(v, f)),
        Obj::Stream(s) => s.dict.0.iter().for_each(|(_, v)| walk(v, f)),
        _ => {}
    }
}

/// Reasons to leave a file exactly as it is.
fn gate(doc: &Doc, objs: &BTreeMap<u32, (u16, Obj)>) -> Result<(), String> {
    if doc.trailer.has(b"Encrypt") {
        return Err("encrypted".into());
    }
    if doc.has_revisions {
        return Err("keeps earlier revisions (incremental updates)".into());
    }
    if doc.trailing > 1024 {
        return Err("data after the end of the file".into());
    }
    // Objects numbered at or beyond /Size: readers disagree on whether they exist.
    if let Some(size) = doc.trailer.get(b"Size").and_then(|x| x.as_int()) {
        // Only objects the document uses count (an xref stream numbered at /Size is harmless).
        let max_used = objs.keys().copied().max().unwrap_or(0) as i64;
        if max_used >= size {
            return Err("object numbers beyond /Size".into());
        }
    }
    // A reference to an object missing from the cross-reference data is null — unless the object is
    // physically in the file, where some readers find it anyway.
    let mut dangling = Vec::new();
    for (_, (_, o)) in objs.iter() {
        let mut r = Vec::new();
        match o {
            // A stream's /Length is replaced by a direct number, so its target doesn't matter.
            Obj::Stream(st) => st.dict.0.iter().filter(|(k, _)| k != b"Length").for_each(|(_, v)| v.refs(&mut r)),
            _ => o.refs(&mut r),
        }
        dangling.extend(r.into_iter().filter(|(n, _)| !objs.contains_key(n)));
    }
    for (n, g) in dangling {
        let pat = format!("{n} {g} obj");
        if contains(doc.data, pat.as_bytes()) {
            return Err("an object in the file is missing from its cross-reference data".into());
        }
    }
    let mut why: Option<String> = None;
    for (_, (_, o)) in objs.iter() {
        walk(o, &mut |x| {
            if let Some(d) = x.as_dict() {
                if d.name(b"Type") == Some(b"Sig") || d.has(b"ByteRange") {
                    why.get_or_insert("digitally signed".into());
                }
                if d.has(b"Perms") && d.name(b"Type") == Some(b"Catalog") {
                    why.get_or_insert("has usage rights or certification".into());
                }
                if d.name(b"S") == Some(b"GTS_PDFX") {
                    why.get_or_insert("PDF/X".into());
                }
            }
        });
        if let Obj::Stream(s) = o {
            if s.dict.name(b"Type") == Some(b"Metadata") {
                let data = xref::decode_fully(&s.dict, &s.data, 16 << 20).unwrap_or_default();
                if contains(&data, b"pdfaid:part") || contains(&data, b"pdfaid:conformance") {
                    why.get_or_insert("PDF/A".into());
                }
            }
        }
    }
    match why {
        Some(w) => Err(w),
        None => Ok(()),
    }
}

fn contains(h: &[u8], n: &[u8]) -> bool {
    h.windows(n.len()).any(|w| w == n)
}

fn set_chain(dict: &mut Dict, stages: &[Stage]) {
    dict.remove(b"Filter");
    dict.remove(b"DecodeParms");
    match stages.len() {
        0 => {}
        1 => {
            dict.set(b"Filter", Obj::Name(stages[0].name.clone()));
            if let Some(p) = &stages[0].parms {
                dict.set(b"DecodeParms", Obj::Dict(p.clone()));
            }
        }
        _ => {
            dict.set(b"Filter", Obj::Array(stages.iter().map(|s| Obj::Name(s.name.clone())).collect()));
            if stages.iter().any(|s| s.parms.is_some()) {
                dict.set(
                    b"DecodeParms",
                    Obj::Array(stages.iter().map(|s| s.parms.clone().map_or(Obj::Null, Obj::Dict)).collect()),
                );
            }
        }
    }
}

/// The best encoding of one stream: (new dict, new data), or None to keep it exactly as it is.
fn reencode(s: &Stream, opt: &Options, resolve: &dyn Fn(&Obj) -> Option<Obj>) -> Option<(Dict, Vec<u8>)> {
    if s.dict.name(b"Type") == Some(b"Metadata") || s.dict.has(b"F") {
        return None; // XMP stays readable by other tools; external file streams are left alone
    }
    let stages = chain(&s.dict)?;
    let k = stages.iter().take_while(|st| is_general(&st.name)).count();
    if stages[k..].iter().any(|st| is_general(&st.name)) {
        return None;
    }
    if stages[k..].iter().any(|st| !matches!(st.name.as_slice(), b"DCTDecode" | b"JPXDecode" | b"JBIG2Decode" | b"CCITTFaxDecode")) {
        return None; // unknown or Crypt filters
    }
    // Predictor parameters may only sit on the last general stage (they stay with the data).
    for st in &stages[..k.saturating_sub(1)] {
        if st.parms.as_ref().map_or(false, |p| p.has(b"Predictor")) {
            return None;
        }
    }
    let mut data = s.data.clone();
    for st in &stages[..k] {
        data = decode_stage(st, &data, opt.max_stream).ok()?;
    }
    let carried: Option<Dict> = if k > 0 {
        stages[k - 1].parms.clone().and_then(|mut p| {
            p.remove(b"EarlyChange");
            if p.has(b"Predictor") {
                Some(p)
            } else {
                None
            }
        })
    } else {
        None
    };
    let rest = &stages[k..];
    let mut best: Option<(Dict, Vec<u8>)> = None;
    let mut best_len = s.data.len();
    // Some readers decide how to read JPEG 2000 and JBIG2 images from the first filter's name (pdf.js reads
    // a JPEG 2000 image's colour and depth from the image data only when JPXDecode comes first). So for
    // those the chain keeps its shape: a lone codec stays alone, a codec behind general filters stays behind
    // one.
    let shape_matters = rest.iter().any(|st| matches!(st.name.as_slice(), b"JPXDecode" | b"JBIG2Decode"));
    // Our Flate in front of whatever image codec follows. Putting a filter in front of a codec that had
    // none (a JPEG, say) is done only when it saves at least 1%.
    let adds_layer = k == 0 && !rest.is_empty();
    if !(adds_layer && shape_matters) {
        let z = flate::deflate_zlib(&data, &opt.flate);
        let mut with_flate = vec![Stage { name: b"FlateDecode".to_vec(), parms: carried.clone() }];
        with_flate.extend_from_slice(rest);
        let mut d = s.dict.clone();
        set_chain(&mut d, &with_flate);
        let limit = if adds_layer { best_len - best_len / 100 } else { best_len };
        if z.len() < limit {
            best_len = z.len();
            best = Some((d, z));
        }
    }
    // No general filter at all (e.g. a JPEG that Flate can't shrink), only without predictor bytes.
    if carried.is_none() && k > 0 && !shape_matters && data.len() < best_len {
        let mut d = s.dict.clone();
        set_chain(&mut d, rest);
        best_len = data.len();
        best = Some((d, data.clone()));
    }
    // Images: try storing the samples with a different (or no) PNG predictor.
    if rest.is_empty() && data.len() <= 48 << 20 {
        if let Some((d, z)) = repredict(s, &data, carried.as_ref(), opt, resolve) {
            if z.len() < best_len {
                best = Some((d, z));
            }
        }
    }
    best
}

fn repredict(s: &Stream, data: &[u8], carried: Option<&Dict>, opt: &Options, resolve: &dyn Fn(&Obj) -> Option<Obj>) -> Option<(Dict, Vec<u8>)> {
    let g = images::geometry(&s.dict, resolve)?;
    let (row, bpp) = (g.row(), g.bpp());
    // The samples, whatever predictor they are stored with now.
    let px = match carried {
        None => data.to_vec(),
        Some(p) => {
            let pred = p.get(b"Predictor").and_then(|x| x.as_int()).unwrap_or(1);
            let pc = p.get(b"Colors").and_then(|x| x.as_int()).unwrap_or(1) as usize;
            let pb = p.get(b"BitsPerComponent").and_then(|x| x.as_int()).unwrap_or(8) as usize;
            let pcol = p.get(b"Columns").and_then(|x| x.as_int()).unwrap_or(1) as usize;
            if (pc, pb, pcol) != (g.colors, g.bpc, g.columns) {
                return None; // predictor geometry differs from the image's: leave it
            }
            if pred >= 10 {
                images::png_unfilter(data, row, bpp)?
            } else if pred == 2 && g.bpc == 8 {
                images::tiff_unfilter(data, row, bpp)
            } else {
                return None;
            }
        }
    };
    if px.is_empty() || px.len() % row != 0 || px.len() < 1024 {
        return None;
    }
    let quick = flate::Options::quick();
    let mut cands: Vec<(Option<Dict>, Vec<u8>)> = vec![(None, px.clone())];
    for how in [images::Choice::MinSum, images::Choice::Fixed(2), images::Choice::Fixed(1), images::Choice::Fixed(4)] {
        let f = images::png_filter(&px, row, bpp, how);
        let mut p = Dict::default();
        p.set(b"Predictor", Obj::Int(15));
        p.set(b"Colors", Obj::Int(g.colors as i64));
        p.set(b"BitsPerComponent", Obj::Int(g.bpc as i64));
        p.set(b"Columns", Obj::Int(g.columns as i64));
        cands.push((Some(p), f));
    }
    let (parms, bytes) = cands.into_iter().min_by_key(|(_, b)| flate::deflate_zlib(b, &quick).len())?;
    let z = flate::deflate_zlib(&bytes, &opt.flate);
    let mut d = s.dict.clone();
    set_chain(&mut d, &[Stage { name: b"FlateDecode".to_vec(), parms }]);
    Some((d, z))
}

fn kind_key(o: &Obj) -> (u8, Vec<u8>, Vec<u8>) {
    match o {
        Obj::Dict(d) => (
            0,
            d.name(b"Type").map(|x| x.to_vec()).unwrap_or_default(),
            d.name(b"Subtype").map(|x| x.to_vec()).or_else(|| d.name(b"S").map(|x| x.to_vec())).unwrap_or_default(),
        ),
        Obj::Array(_) => (1, Vec::new(), Vec::new()),
        _ => (2, Vec::new(), Vec::new()),
    }
}

fn rewrite(input: &[u8], opt: &Options) -> Result<(Vec<u8>, Stats), String> {
    let doc = xref::open(input).map_err(|e| format!("unreadable: {e:?}"))?;
    let objs = load_reachable(&doc)?;
    gate(&doc, &objs)?;
    let mut stats = Stats { objects: objs.len(), dropped_unreachable: doc.entries.values().filter(|e| !matches!(e, Entry::Free)).count().saturating_sub(objs.len()), ..Default::default() };

    // Streams: re-encode; every stream gets a direct /Length.
    let mut direct: BTreeMap<u32, (u16, Vec<u8>)> = BTreeMap::new(); // serialised indirect objects
    let mut packable: Vec<(u32, &Obj)> = Vec::new();
    for (&n, (g, o)) in &objs {
        match o {
            Obj::Stream(s) => {
                stats.streams += 1;
                let resolve = |o: &Obj| -> Option<Obj> {
                    match o {
                        Obj::Ref(n, _) => objs.get(n).map(|x| x.1.clone()),
                        x => Some(x.clone()),
                    }
                };
                let (mut dict, data) = match reencode(s, opt, &resolve) {
                    Some(x) => {
                        stats.streams_reencoded += 1;
                        x
                    }
                    None => {
                        stats.streams_kept += 1;
                        (s.dict.clone(), s.data.clone())
                    }
                };
                dict.set(b"Length", Obj::Int(data.len() as i64));
                let mut body = Vec::with_capacity(data.len() + 64);
                Writer { out: &mut body }.dict(&dict);
                body.extend_from_slice(b"stream\n");
                body.extend_from_slice(&data);
                body.extend_from_slice(b"\nendstream");
                direct.insert(n, (*g, body));
            }
            _ if *g != 0 => {
                direct.insert(n, (*g, to_bytes(o)));
            }
            _ => packable.push((n, o)),
        }
    }

    // Object streams, grouped by kind so similar objects sit together.
    packable.sort_by(|a, b| kind_key(a.1).cmp(&kind_key(b.1)).then(a.0.cmp(&b.0)));
    let max_num = objs.keys().copied().max().unwrap_or(0).max(doc.trailer.get(b"Size").and_then(|x| x.as_int()).unwrap_or(0) as u32);
    let mut next_num = max_num + 1;
    let mut in_stm: HashMap<u32, (u32, u32)> = HashMap::new();
    for chunk in packable.chunks(opt.objstm_size.max(1)) {
        let sn = next_num;
        next_num += 1;
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
        let mut raw = head;
        raw.extend_from_slice(&body);
        let z = flate::deflate_zlib(&raw, &opt.flate);
        let mut d = Dict::default();
        d.set(b"Type", Obj::Name(b"ObjStm".to_vec()));
        d.set(b"N", Obj::Int(chunk.len() as i64));
        d.set(b"First", Obj::Int(first as i64));
        d.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
        d.set(b"Length", Obj::Int(z.len() as i64));
        let mut out = Vec::new();
        Writer { out: &mut out }.dict(&d);
        out.extend_from_slice(b"stream\n");
        out.extend_from_slice(&z);
        out.extend_from_slice(b"\nendstream");
        direct.insert(sn, (0, out));
        stats.objstms += 1;
    }

    // The file.
    let minor = if doc.version.0 == 1 { doc.version.1.max(5) } else { doc.version.1 };
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(format!("%PDF-{}.{}\n", doc.version.0, minor).as_bytes());
    // A comment of high bytes tells transfer tools the file is binary.
    out.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
    let mut offsets: HashMap<u32, (usize, u16)> = HashMap::new();
    for (n, (g, body)) in &direct {
        offsets.insert(*n, (out.len(), *g));
        out.extend_from_slice(format!("{n} {g} obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    // Cross-reference stream.
    let xn = next_num;
    let size = xn + 1;
    let xref_off = out.len();
    offsets.insert(xn, (xref_off, 0));
    let max_off = offsets.values().map(|v| v.0).max().unwrap_or(0).max(xref_off);
    let max_f3 = in_stm.values().map(|v| v.1 as usize).max().unwrap_or(0).max(offsets.values().map(|v| v.1 as usize).max().unwrap_or(0)).max(65535);
    let max_f2 = max_off.max(next_num as usize);
    let w1 = bytes_for(max_f2 as u64);
    let w2 = bytes_for(max_f3 as u64);
    let mut rows = Vec::with_capacity(size as usize * (1 + w1 + w2));
    for n in 0..size {
        let (t, f2, f3): (u8, u64, u64) = if n == 0 {
            (0, 0, 65535)
        } else if let Some(&(off, g)) = offsets.get(&n) {
            (1, off as u64, g as u64)
        } else if let Some(&(sn, i)) = in_stm.get(&n) {
            (2, sn as u64, i as u64)
        } else {
            (0, 0, 0)
        };
        rows.push(t);
        rows.extend_from_slice(&f2.to_be_bytes()[8 - w1..]);
        rows.extend_from_slice(&f3.to_be_bytes()[8 - w2..]);
    }
    // PNG "Up" prediction usually helps the column-structured rows; keep whichever is smaller.
    let cols = 1 + w1 + w2;
    let mut up = Vec::with_capacity(rows.len() + rows.len() / cols + 1);
    for (r, row) in rows.chunks(cols).enumerate() {
        up.push(2u8);
        for (i, &b) in row.iter().enumerate() {
            let above = if r == 0 { 0 } else { rows[(r - 1) * cols + i] };
            up.push(b.wrapping_sub(above));
        }
    }
    let z_plain = flate::deflate_zlib(&rows, &opt.flate);
    let z_up = flate::deflate_zlib(&up, &opt.flate);
    let mut xd = Dict::default();
    xd.set(b"Type", Obj::Name(b"XRef".to_vec()));
    xd.set(b"Size", Obj::Int(size as i64));
    xd.set(b"W", Obj::Array(vec![Obj::Int(1), Obj::Int(w1 as i64), Obj::Int(w2 as i64)]));
    // Every trailer entry that isn't about the old cross-reference structure carries over.
    for (k, v) in &doc.trailer.0 {
        if !STRUCTURAL.contains(&k.as_slice()) {
            xd.set(k, v.clone());
        }
    }
    xd.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
    let zx = if z_up.len() < z_plain.len() {
        let mut p = Dict::default();
        p.set(b"Predictor", Obj::Int(12));
        p.set(b"Columns", Obj::Int(cols as i64));
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

    // Read our own output back and compare everything reachable.
    self_check(&objs, &out, &doc.trailer)?;
    Ok((out, stats))
}

/// Trailer keys that describe the cross-reference structure itself (rewritten, not copied).
const STRUCTURAL: [&[u8]; 9] = [b"Size", b"Prev", b"XRefStm", b"Index", b"W", b"Type", b"Filter", b"DecodeParms", b"Length"];

fn bytes_for(v: u64) -> usize {
    let mut n = 1;
    while n < 8 && v >> (8 * n) != 0 {
        n += 1;
    }
    n
}

/// Decoded content of a stream through its general filters, predictors undone, plus the image-codec stages
/// that remain (their bytes are compared as they are).
fn content_of(s: &Stream) -> Option<(Vec<u8>, Vec<Stage>)> {
    let stages = chain(&s.dict)?;
    let mut data = s.data.clone();
    let mut rest = Vec::new();
    for st in stages {
        if is_general(&st.name) && rest.is_empty() {
            data = decode_stage(&st, &data, 1 << 30).ok()?;
            if st.name == b"FlateDecode" || st.name == b"LZWDecode" {
                let pred = st.parms.as_ref().and_then(|p| p.get(b"Predictor")).and_then(|x| x.as_int()).unwrap_or(1);
                if pred > 1 {
                    data = super::filters::unpredict(&st, &data).ok()?;
                }
            }
        } else {
            rest.push(st);
        }
    }
    Some((data, rest))
}


fn same_except_encoding(a: &Dict, b: &Dict) -> bool {
    let strip = |d: &Dict| -> Vec<(Vec<u8>, Obj)> {
        d.0.iter().filter(|(k, _)| !matches!(k.as_slice(), b"Length" | b"Filter" | b"DecodeParms")).cloned().collect()
    };
    strip(a) == strip(b)
}

fn self_check(objs: &BTreeMap<u32, (u16, Obj)>, out: &[u8], trailer: &Dict) -> Result<(), String> {
    let doc2 = xref::open(out).map_err(|e| format!("self-check: output unreadable: {e:?}"))?;
    for (k, v) in &trailer.0 {
        if !STRUCTURAL.contains(&k.as_slice()) && doc2.trailer.get(k) != Some(v) {
            return Err("self-check: trailer differs".into());
        }
    }
    let objs2 = load_reachable(&doc2)?;
    let a: BTreeSet<u32> = objs.keys().copied().collect();
    let b: BTreeSet<u32> = objs2.keys().copied().collect();
    if a != b {
        if std::env::var_os("PDFE_DEBUG").is_some() {
            eprintln!("only in input: {:?}", a.difference(&b).take(10).collect::<Vec<_>>());
            eprintln!("only in output: {:?}", b.difference(&a).take(10).collect::<Vec<_>>());
        }
        return Err("self-check: different objects reachable".into());
    }
    for (n, (g, o)) in objs {
        let (g2, o2) = &objs2[n];
        if g != g2 {
            return Err(format!("self-check: generation of {n} differs"));
        }
        match (o, o2) {
            (Obj::Stream(s1), Obj::Stream(s2)) => {
                if !same_except_encoding(&s1.dict, &s2.dict) {
                    return Err(format!("self-check: dictionary of stream {n} differs"));
                }
                if s1.data == s2.data && s1.dict.get(b"Filter") == s2.dict.get(b"Filter") && s1.dict.get(b"DecodeParms") == s2.dict.get(b"DecodeParms") {
                    continue;
                }
                let c1 = content_of(s1).ok_or(format!("self-check: stream {n} undecodable"))?;
                let c2 = content_of(s2).ok_or(format!("self-check: stream {n} undecodable in output"))?;
                if c1 != c2 {
                    return Err(format!("self-check: content of stream {n} differs"));
                }
            }
            (x, y) => {
                if x != y {
                    return Err(format!("self-check: object {n} differs"));
                }
            }
        }
    }
    Ok(())
}
