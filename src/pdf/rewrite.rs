//! Rewriting a PDF smaller without changing its content.
//!
//! The file is read strictly; anything we can't be sure about leaves the file untouched (the "gate"). Every
//! object reachable from the trailer is kept with the same number and value. Streams are re-encoded with our
//! Flate encoder when that is smaller; the data the decoder gives back is identical. Small objects are packed
//! into compressed object streams grouped by type, and the cross-reference table becomes a compressed
//! cross-reference stream. Before anything is returned, the output is read back and compared object by object.

use super::crypt::{Crypt, Method};
use super::filters::{chain, decode_stage, is_general, Stage};
use super::images;
use super::object::*;
use super::xref::{self, Doc, Entry};
use crate::clock::{self, Deadline};
use crate::flate;
use crate::par;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Debug)]
pub struct Options {
    pub flate: flate::Options,
    /// Most objects per object stream.
    pub objstm_size: usize,
    /// Largest decoded stream we re-encode.
    pub max_stream: usize,
    /// How hard to try, 0 to 100. Below 20 (`FAST_BELOW`) the fast encoder (`flate::fast`): effort 0 is the
    /// fast path, with the fast encoder's near-optimal top level for page content, fonts and the rest (a faster
    /// near-optimal level for one that decodes to more than 4 MiB, `BIG_STREAM`), a lazy level for images
    /// (`IMAGE_LEVEL`), image predictors chosen from an estimate and only the cheap steps.
    /// 1 to 19 bridge to the strong encoder: images get closer parses and from 10 (`STRONG_FROM`) the streams
    /// that take the most room get the strong encoder's quick settings within a budget that doubles every
    /// point, except a Flate stream that effort 0's settings can't shrink, which is kept as it is. From
    /// 20 on the strong encoder: at 100 every stream gets `flate` and the full predictor search; below, the
    /// streams that take the most room come first and get `flate`, or a cheaper step of 20, while the work
    /// stays within a budget proportional to the file's size, the others get the quick settings, and image
    /// predictors are chosen from a sample of rows (see `effort_plan`). Below 100 the decoded bytes have a
    /// budget too (16 per byte of the file at effort 0, twice as much every 10 points, at least 4 MiB): a
    /// stream beyond it is kept as it is. Decided from sizes only, never from time, so the output is repeatable.
    pub effort: u8,
    /// Threads to use (0: every core the machine offers). Never changes the output.
    pub threads: usize,
    /// Leave files that claim a conformance or carry a signature untouched: PDF/A, PDF/X, signed, certified,
    /// usage rights (the safe default). With `false` every object's value is still kept, but those
    /// guarantees are given up (a rewritten signed file no longer verifies).
    pub respect_conformance: bool,
    /// Also try our Flate in front of a JPEG image (never in front of JPEG 2000 or JBIG2).
    pub flate_over_jpeg: bool,
    /// Write a CCITT fax image as JBIG2 when that is clearly smaller (the same pixels; see `crate::fax::pdf`
    /// for what else must hold). Off, every fax image keeps its CCITT bytes.
    pub jbig2: bool,
    /// Give JPEG images optimal Huffman tables (the same coefficients; see `crate::jpeg`). Off, every JPEG
    /// keeps its bytes.
    pub jpeg_tables: bool,
    /// Rewrite files the empty user password opens, keeping their encryption (see `crate::pdf::crypt`). Off,
    /// every encrypted file is left untouched.
    pub rewrite_encrypted: bool,
    /// Stop when this passes (checked between streams and Flate segments): the outcome is then `TimedOut`
    /// and nothing is returned. A rewrite that finishes gives the same bytes with or without a deadline.
    pub deadline: Deadline,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            flate: flate::Options::max(),
            objstm_size: 200,
            max_stream: 64 << 20,
            effort: 100,
            threads: 0,
            respect_conformance: true,
            flate_over_jpeg: true,
            jbig2: true,
            jpeg_tables: true,
            rewrite_encrypted: true,
            deadline: Deadline::NONE,
        }
    }
}

impl Options {
    /// The settings for an effort from 0 (fast) to 100 (our strongest).
    pub fn effort(effort: u8) -> Options {
        let effort = effort.min(100);
        Options { flate: flate::Options::effort(effort), effort, ..Default::default() }
    }
}

/// The reason `rewrite` gives up when its deadline passes (see `Outcome::TimedOut`).
const TIMED_OUT: &str = "out of time";

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub objects: usize,
    pub streams: usize,
    pub streams_reencoded: usize,
    pub streams_kept: usize,
    pub objstms: usize,
    pub dropped_unreachable: usize,
    /// JPEG images written again with optimal Huffman tables (the same coefficients).
    pub jpegs_retabled: usize,
    /// CCITT fax images written as JBIG2 (the same pixels).
    pub fax_to_jbig2: usize,
    /// How the file is encrypted (kept exactly as it was), e.g. "R4/AESV2"; empty when it isn't.
    pub encryption: String,
}

#[derive(Debug)]
pub enum Outcome {
    Smaller(Stats),
    /// Returned unchanged, with the reason.
    Untouched(String),
    /// The deadline passed before the work was done; the input is returned unchanged.
    TimedOut,
}

/// What a re-encoded stream went through besides our Flate, for the statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// The JPEG was given optimal Huffman tables.
    pub jpeg_retabled: bool,
    /// The CCITT fax image became JBIG2.
    pub fax_to_jbig2: bool,
}

/// A stream's new dictionary and data, with what changed in it.
type Reencoded = (Dict, Vec<u8>, Change);

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
        Err(reason) if reason == TIMED_OUT => Optimized { data: input.to_vec(), outcome: Outcome::TimedOut },
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

/// Reasons to leave a file exactly as it is. With `conformance` false, a claimed conformance (PDF/A, PDF/X), a
/// signature, certification or usage rights don't count.
fn gate(doc: &Doc, objs: &BTreeMap<u32, (u16, Obj)>, conformance: bool) -> Result<(), String> {
    match &doc.crypt {
        Some(Err(why)) => return Err(format!("encrypted: {why}")),
        Some(Ok(c)) => encrypted_gate(c, objs)?,
        None => {}
    }
    if doc.has_revisions {
        return Err("keeps earlier revisions (incremental updates)".into());
    }
    if doc.trailing > 1024 {
        return Err("data after the end of the file".into());
    }
    // Only objects the document uses count (an xref stream numbered at /Size is harmless), and the encryption
    // dictionary's.
    let crypt = doc.crypt.as_ref().and_then(|c| c.as_ref().ok());
    let enc = crypt_objects(crypt);
    let max_used = objs.keys().copied().chain(enc.iter().map(|o| o.0)).max().unwrap_or(0);
    // Objects numbered at or beyond /Size: readers disagree on whether they exist.
    if let Some(size) = doc.trailer.get(b"Size").and_then(|x| x.as_int()) {
        if max_used as i64 >= size {
            return Err("object numbers beyond /Size".into());
        }
    }
    // The cross-reference stream written has a row for every number up to the highest one the document
    // uses, so a file whose numbers go beyond what its size allows for rows (the reader's bound on the rows
    // a file may have) would cost rows out of all proportion to its size. Such a file is left untouched.
    if highest_number(doc, objs, crypt) as u64 + 1 > xref::row_limit(doc.data.len()) as u64 {
        return Err("object numbers far apart".into());
    }
    // A reference to an object missing from the cross-reference data is null — unless the object is
    // physically in the file, where some readers find it anyway.
    let mut dangling = Vec::new();
    for (_, o) in objs.values() {
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
    if !conformance {
        return Ok(());
    }
    let mut why: Option<String> = None;
    for (_, o) in objs.values() {
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

/// Encrypted files we rewrite: the encryption dictionary and the objects it refers to stay apart from the
/// document (they are written as they are, unencrypted). Trailer strings need nothing: readers read the trailer
/// before decrypting anything, and it is copied as it is into the cross-reference stream, which is never
/// encrypted.
fn encrypted_gate(c: &Crypt, objs: &BTreeMap<u32, (u16, Obj)>) -> Result<(), String> {
    if objs.keys().any(|&n| c.is_own(n)) {
        return Err("encrypted: the encryption dictionary is also part of the document".into());
    }
    Ok(())
}

/// The highest object number the document uses: its reachable objects, the encryption dictionary's, and
/// every number the trailer or a reachable object refers to. New objects are numbered above it: a reference
/// to a missing object is a null reference, and stays one only while nothing takes its number (a stream's
/// /Length is written as a direct number, so its target doesn't count).
fn highest_number(doc: &Doc, objs: &BTreeMap<u32, (u16, Obj)>, crypt: Option<&Crypt>) -> u32 {
    let mut refs = Vec::new();
    doc.trailer.0.iter().for_each(|(_, v)| v.refs(&mut refs));
    for (_, o) in objs.values() {
        match o {
            Obj::Stream(st) => st.dict.0.iter().filter(|(k, _)| k != b"Length").for_each(|(_, v)| v.refs(&mut refs)),
            _ => o.refs(&mut refs),
        }
    }
    let top = objs.keys().copied().max().unwrap_or(0);
    crypt_objects(crypt).iter().map(|o| o.0).chain(refs.iter().map(|r| r.0)).fold(top, u32::max)
}

/// Object numbers of the encryption dictionary and the objects it refers to.
fn crypt_objects(c: Option<&Crypt>) -> Vec<(u32, u16)> {
    c.map_or(Vec::new(), |c| c.obj.into_iter().chain(c.parts.iter().map(|p| (p.0, p.1))).collect())
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

/// The best encoding of one stream: (new dict, new data, what changed), or None to keep it exactly as it is.
fn reencode(s: &Stream, opt: &Options, resolve: &dyn Fn(&Obj) -> Option<Obj>) -> Option<Reencoded> {
    if s.dict.name(b"Type") == Some(b"Metadata") || s.dict.has(b"F") {
        return None; // XMP stays readable by other tools; external file streams are left alone
    }
    // A scanned black-and-white image: JBIG2 instead of CCITT fax when that is clearly smaller (same pixels).
    if opt.jbig2 {
        if let Some((d, z)) = crate::fax::pdf::jbig2_candidate(s, resolve) {
            return Some((d, z, Change { fax_to_jbig2: true, ..Change::default() }));
        }
    }
    let stages = chain(&s.dict)?;
    let k = stages.iter().take_while(|st| is_general(&st.name)).count();
    if stages[k..].iter().any(|st| is_general(&st.name)) {
        return None;
    }
    if stages[k..]
        .iter()
        .any(|st| !matches!(st.name.as_slice(), b"DCTDecode" | b"JPXDecode" | b"JBIG2Decode" | b"CCITTFaxDecode"))
    {
        return None; // unknown or Crypt filters
    }
    // Predictor parameters may only sit on the last general stage (they stay with the data).
    for st in &stages[..k.saturating_sub(1)] {
        if st.parms.as_ref().is_some_and(|p| p.has(b"Predictor")) {
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
    // On the bridge to 20, what won't shrink is left alone: a stream the plan gives the strong encoder (from
    // `STRONG_FROM`), other than an image and already compressed by Flate, whose re-encode at effort 0's
    // settings is not smaller is kept as it is, and the strong encoder is not spent on it. Decided from sizes
    // alone. (An image goes on: a predictor may still make it smaller.) The quick settings the plans of 20 and
    // above give carry effort 0, so the range test keeps them exactly as they were.
    if opt.flate.fast == 0
        && (STRONG_FROM..FAST_BELOW).contains(&opt.effort)
        && rest.is_empty()
        && carried.is_none()
        && stages[..k].iter().any(|st| st.name == b"FlateDecode")
        && s.dict.name(b"Subtype") != Some(b"Image")
        // The data without any filter is an alternative too (below); when it is smaller, go on.
        && data.len() >= s.data.len()
    {
        let fast = flate::Options {
            shared: opt.flate.shared.clone(),
            deadline: opt.flate.deadline,
            ..flate::Options::effort(0)
        };
        if !opt.flate.same_output(&fast) && flate::deflate_zlib(&data, &fast).len() >= s.data.len() {
            return None;
        }
    }
    let mut best: Option<(Dict, Vec<u8>)> = None;
    let mut best_len = s.data.len();
    // Whether `best` holds the JPEG with its new tables (alone, or behind Flate).
    let mut best_retabled = false;
    // A JPEG with optimal Huffman tables: the same coefficients in fewer bytes, read back and compared before
    // it is used (anything unusual is left as it is).
    let jpeg = match rest {
        [st] if opt.jpeg_tables && st.name == b"DCTDecode" && carried.is_none() && data.len() <= opt.max_stream => {
            crate::jpeg::optimize(&data).ok()
        }
        _ => None,
    };
    if let Some(j) = &jpeg {
        if j.len() < best_len {
            let mut d = s.dict.clone();
            set_chain(&mut d, rest);
            best_len = j.len();
            best = Some((d, j.clone()));
            best_retabled = true;
        }
    }
    // Some readers decide how to read JPEG 2000 and JBIG2 images from the first filter's name (pdf.js reads
    // a JPEG 2000 image's colour and depth from the image data only when JPXDecode comes first). So for
    // those the chain keeps its shape: a lone codec stays alone, a codec behind general filters stays behind
    // one.
    let shape_matters = rest.iter().any(|st| matches!(st.name.as_slice(), b"JPXDecode" | b"JBIG2Decode"));
    // Our Flate in front of whatever image codec follows. Putting a filter in front of a codec that had
    // none (a JPEG, say) is done only when it saves at least 1% (over the optimised JPEG, when there is one),
    // and in front of a JPEG only when `flate_over_jpeg` asks for it. A JPEG goes into Flate in its optimised
    // form. Only when Flate shrinks that by a tenth or more (very regular data) and the original is at least
    // a tenth larger (poor tables) does the original get a turn too: its own byte patterns can suit Flate
    // better then (2 of the 284 test JPEGs, 1.8 KB). Trying it for every JPEG would double the Flate time,
    // so that second pass belongs to effort 100 alone.
    let adds_layer = k == 0 && !rest.is_empty();
    let over_jpeg = adds_layer && rest.iter().any(|st| st.name == b"DCTDecode");
    if !(adds_layer && shape_matters) && !(over_jpeg && !opt.flate_over_jpeg) {
        let bare_len = best_len;
        let mut with_flate = vec![Stage { name: b"FlateDecode".to_vec(), parms: carried.clone() }];
        with_flate.extend_from_slice(rest);
        let mut d = s.dict.clone();
        set_chain(&mut d, &with_flate);
        let z = flate::deflate_zlib(jpeg.as_ref().unwrap_or(&data), &opt.flate);
        let again = opt.effort >= 100
            && jpeg.as_ref().is_some_and(|j| z.len() * 10 <= j.len() * 9 && j.len() * 10 <= data.len() * 9);
        // The first candidate is Flate over the JPEG with its new tables (when there is one), the second
        // over the original bytes.
        for (z, retabled) in
            std::iter::once((z, jpeg.is_some())).chain(again.then(|| (flate::deflate_zlib(&data, &opt.flate), false)))
        {
            let limit = if adds_layer { best_len.min(bare_len - bare_len / 100) } else { best_len };
            if z.len() < limit {
                best_len = z.len();
                best = Some((d.clone(), z));
                best_retabled = retabled;
            }
        }
    }
    // No general filter at all (e.g. a JPEG that Flate can't shrink), only without predictor bytes.
    if carried.is_none() && k > 0 && !shape_matters && data.len() < best_len {
        let mut d = s.dict.clone();
        set_chain(&mut d, rest);
        best_len = data.len();
        best = Some((d, data.clone()));
        best_retabled = false;
    }
    // Images: try storing the samples with a different (or no) PNG predictor. The fast path tries one,
    // chosen without encoding, and compares whole sizes (the dictionaries counted).
    if rest.is_empty() && data.len() <= 48 << 20 && opt.flate.fast > 0 && opt.effort < TRIAL_PREDICTORS {
        let dict = best.as_ref().map_or(&s.dict, |b| &b.0);
        let plain = best_len + to_bytes(&Obj::Dict(dict.clone())).len();
        if let Some((d, z)) = repredict_fast(s, &data, carried.as_ref(), opt, resolve, plain) {
            best = Some((d, z));
            best_retabled = false;
        }
    } else if rest.is_empty() && data.len() <= 48 << 20 {
        if let Some((d, z)) = repredict(s, &data, carried.as_ref(), opt, resolve) {
            if z.len() < best_len {
                best = Some((d, z));
                best_retabled = false;
            }
        }
    }
    best.map(|(d, z)| (d, z, Change { jpeg_retabled: best_retabled, ..Change::default() }))
}

/// An encrypted XMP metadata stream (no tool reads it from the file without decrypting it): our Flate when
/// that is smaller. Only general filters without parameters are undone; anything else stays as it is.
fn reencode_xmp(s: &Stream, opt: &Options) -> Option<Reencoded> {
    let stages = chain(&s.dict)?;
    if s.dict.has(b"F") || stages.iter().any(|st| !is_general(&st.name) || st.parms.is_some()) {
        return None;
    }
    let mut data = s.data.clone();
    for st in &stages {
        data = decode_stage(st, &data, opt.max_stream).ok()?;
    }
    let z = flate::deflate_zlib(&data, &opt.flate);
    if z.len() >= s.data.len() {
        return None;
    }
    let mut d = s.dict.clone();
    set_chain(&mut d, &[Stage { name: b"FlateDecode".to_vec(), parms: None }]);
    Some((d, z, Change::default()))
}

/// An image's geometry and its samples, whatever predictor they are stored with now; None when it is not an
/// image we can work on (unknown geometry, a predictor that doesn't fit it, under 1 KiB, not whole rows).
fn samples(
    s: &Stream,
    data: &[u8],
    carried: Option<&Dict>,
    resolve: &dyn Fn(&Obj) -> Option<Obj>,
) -> Option<(images::Geometry, Vec<u8>)> {
    let g = images::geometry(&s.dict, resolve)?;
    let (row, bpp) = (g.row(), g.bpp());
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
    Some((g, px))
}

/// The PNG predictor parameters for an image of geometry `g`.
fn png_parms(g: &images::Geometry) -> Dict {
    let mut p = Dict::default();
    p.set(b"Predictor", Obj::Int(15));
    p.set(b"Colors", Obj::Int(g.colors as i64));
    p.set(b"BitsPerComponent", Obj::Int(g.bpc as i64));
    p.set(b"Columns", Obj::Int(g.columns as i64));
    p
}

/// The fast path's predictor choice (efforts below `TRIAL_PREDICTORS`): one candidate, chosen without
/// encoding (`images::estimate_choice`), and encoded once: the predictor the estimate prefers, else the plain
/// samples when they are stored with a predictor now. `plain` is the best size `reencode` has so far with its
/// stream dictionary counted; the candidate is kept only when it is smaller with its own dictionary.
fn repredict_fast(
    s: &Stream,
    data: &[u8],
    carried: Option<&Dict>,
    opt: &Options,
    resolve: &dyn Fn(&Obj) -> Option<Obj>,
    plain: usize,
) -> Option<(Dict, Vec<u8>)> {
    let (g, px) = samples(s, data, carried, resolve)?;
    let (row, bpp) = (g.row(), g.bpp());
    let nrows = px.len() / row;
    let bands = predictor_sample(nrows, row, 0).unwrap_or_else(|| vec![(0, nrows)]);
    let (parms, bytes) = if let Some(how) = images::estimate_choice(&px, row, bpp, g.bpc, &bands) {
        (Some(png_parms(&g)), images::png_filter(&px, row, bpp, how))
    } else if carried.is_some() {
        (None, px)
    } else {
        return None;
    };
    let mut d = s.dict.clone();
    set_chain(&mut d, &[Stage { name: b"FlateDecode".to_vec(), parms }]);
    let z = flate::deflate_zlib(&bytes, &opt.flate);
    (z.len() + to_bytes(&Obj::Dict(d.clone())).len() < plain).then_some((d, z))
}

fn repredict(
    s: &Stream,
    data: &[u8],
    carried: Option<&Dict>,
    opt: &Options,
    resolve: &dyn Fn(&Obj) -> Option<Obj>,
) -> Option<(Dict, Vec<u8>)> {
    let (g, px) = samples(s, data, carried, resolve)?;
    let (row, bpp) = (g.row(), g.bpp());
    // The candidates are ranked by their size with the quick settings plus the size of their stream
    // dictionary (the predictor parameters take room too), and only the best one gets the full encode. Below
    // effort 100 the data is a sample of rows across the image, so the dictionary counts in proportion. The
    // first of equal sizes wins. Only their sizes count, so their outputs are not remembered.
    // On the fast path the candidates are ranked with the level effort 0 gives images, else with the quick
    // settings.
    let rank = flate::Options { fast: if opt.flate.fast > 0 { image_level(0) } else { 0 }, ..flate::Options::quick() };
    let quick = flate::Options {
        shared: opt.flate.shared.as_ref().map(|sh| flate::deflate::Shared::with_pool(sh.pool.clone(), false)),
        ..rank
    };
    let nrows = px.len() / row;
    let rows = predictor_sample(nrows, row, opt.effort);
    let sampled = rows.as_ref().map_or(nrows, |r| r.iter().map(|&(a, b)| b - a).sum());
    let dict_with = |parms: &Option<Dict>| {
        let mut d = s.dict.clone();
        set_chain(&mut d, &[Stage { name: b"FlateDecode".to_vec(), parms: parms.clone() }]);
        d
    };
    let dict_len = |parms: &Option<Dict>| to_bytes(&Obj::Dict(dict_with(parms))).len();
    let mut best: Option<(u64, Option<Dict>, Vec<u8>)> = None;
    for how in [
        None,
        Some(images::Choice::MinSum),
        Some(images::Choice::Fixed(2)),
        Some(images::Choice::Fixed(1)),
        Some(images::Choice::Fixed(4)),
    ] {
        let (parms, bytes, stride) = match how {
            None => (None, px.clone(), row),
            Some(how) => (Some(png_parms(&g)), images::png_filter(&px, row, bpp, how), row + 1),
        };
        let data_len = match &rows {
            None => flate::deflate_zlib(&bytes, &quick).len(),
            Some(r) => flate::deflate_zlib(
                &r.iter().flat_map(|&(a, b)| &bytes[a * stride..b * stride]).copied().collect::<Vec<u8>>(),
                &quick,
            )
            .len(),
        };
        // (data for all rows + dictionary) x sampled rows, in whole numbers.
        let score = data_len as u64 * nrows as u64 + dict_len(&parms) as u64 * sampled as u64;
        if best.as_ref().is_none_or(|b| score < b.0) {
            best = Some((score, parms, bytes));
        }
    }
    let (_, parms, bytes) = best?;
    let z = flate::deflate_zlib(&bytes, &opt.flate);
    // Without a predictor before, the plain samples are what reencode() has already encoded (remembered, so
    // this costs nothing): a predictor that saves fewer bytes than its parameters take is not kept.
    if parms.is_some()
        && carried.is_none()
        && z.len() + dict_len(&parms) >= flate::deflate_zlib(&px, &opt.flate).len() + dict_len(&None)
    {
        return None;
    }
    Some((dict_with(&parms), z))
}

/// Below this effort the fast encoder is used (`flate::fast`): effort 0 is the fast path, 1 to 19 bridge to
/// the strong encoder with slower fast levels; from 20 on everything is as it was before the fast path.
pub const FAST_BELOW: u8 = 20;

/// From this effort on, image predictors are chosen by encoding candidates (on sampled rows below 100); below
/// it, on the fast path, from an estimate (`repredict_fast`).
const TRIAL_PREDICTORS: u8 = 10;

/// Bytes of an image the predictor candidates are ranked on below effort 100: 64 KiB at effort 0, twice as
/// much every 20 points.
fn predictor_budget(effort: u8) -> usize {
    (64usize << 10) << (effort.min(99) as usize / 20)
}

/// Rows for ranking the predictor candidates of an image below effort 100: bands of whole rows spread over
/// the image (neighbouring rows keep what predictors and matches see), at most a budget of bytes that grows
/// with the effort. None: rank on the whole image (effort 100, or a small image).
fn predictor_sample(nrows: usize, row: usize, effort: u8) -> Option<Vec<(usize, usize)>> {
    if effort >= 100 || row == 0 {
        return None;
    }
    let budget = predictor_budget(effort);
    if nrows.saturating_mul(row) <= budget {
        return None;
    }
    const BANDS: usize = 8;
    let per = (budget / BANDS / row).max(1);
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(BANDS);
    for k in 0..BANDS {
        let start = k * nrows / BANDS;
        if out.last().is_some_and(|&(_, end)| start < end) {
            continue;
        }
        out.push((start, (start + per).min(nrows)));
    }
    Some(out)
}

/// An image with fewer samples (general filters decoded, predictors undone) than its width, height and
/// colours need. Readers differ on such images (one pads them, another leaves them out), and other filters
/// can change what a reader does, so they are kept exactly as they are. Unknown: not known to be short.
fn short_image(s: &Stream, resolve: &dyn Fn(&Obj) -> Option<Obj>) -> bool {
    let Some(g) = images::geometry(&s.dict, resolve) else { return false };
    let Some(h) = s.dict.get(b"Height").and_then(|x| x.as_int()) else { return false };
    match content_of(s) {
        Some((px, rest)) => rest.is_empty() && (px.len() as u128) < g.row() as u128 * h.max(0) as u128,
        None => false,
    }
}

/// For each stream, the index of the first stream with the same dictionary and data (itself when there is
/// none), and the list of those first ones. Identical streams are re-encoded once.
fn identical_streams(streams: &[(u32, &Stream)]) -> (Vec<usize>, Vec<usize>) {
    let mut by_hash: HashMap<(u64, usize), Vec<usize>> = HashMap::new();
    let mut same = Vec::with_capacity(streams.len());
    let mut unique = Vec::new();
    for (i, (_, s)) in streams.iter().enumerate() {
        let firsts = by_hash.entry((par::hash64(&s.data), s.data.len())).or_default();
        match firsts.iter().copied().find(|&j| streams[j].1 == *s) {
            Some(j) => same.push(j),
            None => {
                firsts.push(i);
                same.push(i);
                unique.push(i);
            }
        }
    }
    (same, unique)
}

/// Bytes the general filters of a stream decode to; None when they don't decode within `limit`, or the last of
/// them not within `last` (where a caller can stop early: an earlier stage, hex text say, can be the larger).
fn decoded_size(s: &Stream, limit: usize, last: usize) -> Option<usize> {
    let Some(stages) = chain(&s.dict) else { return Some(s.data.len()) };
    let general = stages.iter().take_while(|st| is_general(&st.name)).count();
    let mut data: std::borrow::Cow<[u8]> = std::borrow::Cow::Borrowed(&s.data);
    for (i, st) in stages[..general].iter().enumerate() {
        let limit = if i + 1 == general { limit.min(last) } else { limit };
        data = std::borrow::Cow::Owned(decode_stage(st, &data, limit).ok()?);
    }
    Some(data.len())
}

/// The fast encoder's level for image streams below `FAST_BELOW` (other streams get `flate::Options::effort`).
/// Images are most of the decoded bytes and gain least from a closer parse, so they get a lazy level.
fn image_level(effort: u8) -> u8 {
    IMAGE_LEVEL[effort.min(FAST_BELOW - 1) as usize]
}

/// From this effort, the bridge to 20 gives some streams the strong encoder (see `effort_plan`).
const STRONG_FROM: u8 = 10;

/// Decoded bytes that get the strong encoder's quick settings at an effort below 20: none below `STRONG_FROM`,
/// a sixteenth of the input's size at it, twice as much every point (32 times the input at 19).
fn bridge_budget(effort: u8, input_len: usize) -> f64 {
    if !(STRONG_FROM..FAST_BELOW).contains(&effort) {
        return 0.0;
    }
    input_len as f64 * (1u64 << (effort - STRONG_FROM)) as f64 / 16.0
}

/// Effort 0 gives images the lazy level with two steps of lookahead (on the sample: 86.93% of the input against
/// 87.08% with one step and 86.96% with the near-optimal level 8, at 17.4 s of CPU against 16.8 and 22.1).
const IMAGE_LEVEL: [u8; FAST_BELOW as usize] =
    [7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12];

/// A stream other than an image that decodes to more than this gets at most `big_level` below `FAST_BELOW`.
/// The top level parses about 6 MB a second on one thread, so this much takes about 0.65 s: past it, one
/// stream would hold the whole file up (a 10 MB CJK font took 1.5 s of a 5.5 MB file's 1.8 s).
const BIG_STREAM: usize = 4 << 20;

/// The fast encoder's level for a stream over `BIG_STREAM` below `FAST_BELOW`: level 9 (one pass of the
/// near-optimal parse, 40% faster than the top level, 0.2% larger on fonts and 1.4% on page content) up to
/// effort 5, then as images, so the top level again from 10.
fn big_level(effort: u8) -> u8 {
    image_level(effort).max(9)
}

/// Bytes the general filters of a stream decode to (the stored size when they don't decode).
fn decoded_len(s: &Stream, limit: usize) -> usize {
    let Some(stages) = chain(&s.dict) else { return s.data.len() };
    let mut data: std::borrow::Cow<[u8]> = std::borrow::Cow::Borrowed(&s.data);
    for st in stages.iter().take_while(|st| is_general(&st.name)) {
        match decode_stage(st, &data, limit) {
            Ok(d) => data = std::borrow::Cow::Owned(d),
            Err(_) => return s.data.len(),
        }
    }
    data.len()
}

/// The extra work allowed below effort 100, per byte of input, in units of quick-encoding one decoded byte:
/// 1 at effort 1, twice as much every 10 points (about 900 at 99).
fn work_budget(effort: u8) -> f64 {
    2f64.powf((effort.max(1) - 1) as f64 / 10.0)
}

/// Decoded bytes the quick settings may work on below effort 100, per byte of input: 16 at effort 0, twice
/// as much every 10 points, and at least 4 MiB in all (so small files are never cut short). A stream that
/// doesn't fit in what is left is kept exactly as it is, which costs nothing and loses nothing: the time
/// follows the file's size even when its streams decode to a hundred times their size.
fn quick_budget(effort: u8, input_len: usize) -> f64 {
    (input_len as f64 * 16.0 * 2f64.powf(effort as f64 / 10.0)).max((4u64 << 20) as f64)
}

/// CPU time of a decoded byte at an effort's Flate settings, against the quick settings (measured, see
/// `flate::Options::effort`; large images).
fn work_per_byte(effort: u8) -> f64 {
    match effort {
        0..=19 => 1.0,
        20..=39 => 1.7,
        40..=59 => 2.0,
        60..=79 => 3.1,
        _ => 3.9,
    }
}

/// The settings for each distinct stream (see `Options::effort`), or None to keep it exactly as it is. At
/// effort 100, `opt` for all of them. Below, the streams that take the most room in the file come first:
/// each gets the quick settings while its decoded bytes fit in what is left of `quick_budget` (and is kept
/// otherwise), then the strongest settings up to the effort (`opt` itself, or a lower step of 20) whose
/// extra work fits in what is left of `work_budget`. Sizes decide, never time.
fn effort_plan(
    streams: &[(u32, &Stream)],
    unique: &[usize],
    opt: &Options,
    input_len: usize,
    pool: Option<&par::Pool>,
) -> Vec<Option<Options>> {
    if opt.effort >= 100 {
        return vec![Some(opt.clone()); unique.len()];
    }
    let mut order: Vec<usize> = (0..unique.len()).collect();
    order.sort_by_key(|&u| (std::cmp::Reverse(streams[unique[u]].1.data.len()), u));
    let mut floor = quick_budget(opt.effort, input_len);
    let mut out = vec![None; unique.len()];
    // The fast path: every stream gets the effort's own settings while its decoded bytes fit, images a faster
    // level of the fast encoder, and so does a stream over `BIG_STREAM` up to effort 9. From `STRONG_FROM` on, the streams that take the most room get the strong
    // encoder's quick settings instead (what effort 0 was before the fast path) while their decoded bytes fit
    // in `bridge_budget`, so the time and the size go on smoothly to 20. A stream whose filters don't decode
    // within `max_stream` is kept as it is (`reencode` would find the same).
    if opt.effort < FAST_BELOW {
        // Out of time: nothing more is decoded (the whole rewrite is given up after the plan). A stream that
        // decodes to more than the whole budget is kept whatever comes before it, so its decoding stops there
        // (a 450 KB image of 243 MB in a 1.1 MB file took 0.2 s to decode as far as `max_stream`, only to be
        // kept).
        let whole = floor as usize;
        let sizes = par::map(pool, unique.len(), None, |u| {
            if opt.deadline.passed() {
                return None;
            }
            decoded_size(streams[unique[u]].1, opt.max_stream, whole)
        });
        let strong = Options {
            flate: flate::Options {
                shared: opt.flate.shared.clone(),
                deadline: opt.flate.deadline,
                ..flate::Options::quick()
            },
            ..opt.clone()
        };
        let mut budget = bridge_budget(opt.effort, input_len);
        for u in order {
            let s = streams[unique[u]].1;
            let Some(n) = sizes[u] else { continue };
            if n as f64 > floor {
                continue;
            }
            floor -= n as f64;
            if n as f64 <= budget {
                budget -= n as f64;
                out[u] = Some(strong.clone());
                continue;
            }
            let mut o = opt.clone();
            if s.dict.name(b"Subtype") == Some(b"Image") {
                o.flate.fast = image_level(opt.effort);
            } else if n > BIG_STREAM && o.flate.fast > big_level(opt.effort) {
                o.flate.fast = big_level(opt.effort);
            }
            out[u] = Some(o);
        }
        return out;
    }
    let at = |e: u8| Options {
        flate: flate::Options {
            shared: opt.flate.shared.clone(),
            deadline: opt.flate.deadline,
            ..flate::Options::effort(e)
        },
        effort: e,
        ..opt.clone()
    };
    let sizes = par::map(pool, unique.len(), None, |u| decoded_len(streams[unique[u]].1, opt.max_stream));
    // From 20 on, every stream gets at least the strong encoder's quick settings.
    let quick = Options {
        flate: flate::Options {
            shared: opt.flate.shared.clone(),
            deadline: opt.flate.deadline,
            ..flate::Options::quick()
        },
        effort: 0,
        ..opt.clone()
    };
    // The settings that may be given above quick, strongest first, with their extra work per decoded byte.
    let mut steps = Vec::new();
    if !(opt.flate.same_output(&quick.flate) && predictor_budget(opt.effort) == predictor_budget(0)) {
        steps.push((opt.clone(), work_per_byte(opt.effort) - 1.0));
        for e in (20..opt.effort / 20 * 20).step_by(20).rev() {
            steps.push((at(e), work_per_byte(e) - 1.0));
        }
    }
    let mut budget = input_len as f64 * work_budget(opt.effort);
    for u in order {
        if sizes[u] as f64 > floor {
            continue;
        }
        floor -= sizes[u] as f64;
        out[u] = Some(quick.clone());
        for (o, extra) in &steps {
            let w = sizes[u] as f64 * extra;
            if w <= budget {
                budget -= w;
                out[u] = Some(o.clone());
                break;
            }
        }
    }
    out
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
    // An encrypted file the empty password doesn't open stays as it is. One it opens is read decrypted and
    // written with the same encryption: same dictionary, /ID, file key and permissions.
    if let Some(Err(why)) = &doc.crypt {
        return Err(format!("encrypted: {why}"));
    }
    if doc.crypt.is_some() && !opt.rewrite_encrypted {
        return Err("encrypted: left as it is (rewrite_encrypted is off)".into());
    }
    let crypt = doc.crypt.as_ref().and_then(|c| c.as_ref().ok());
    let objs = load_reachable(&doc)?;
    gate(&doc, &objs, opt.respect_conformance)?;
    let mut stats = Stats {
        objects: objs.len(),
        dropped_unreachable: doc
            .entries
            .values()
            .filter(|e| !matches!(e, Entry::Free))
            .count()
            .saturating_sub(objs.len()),
        ..Default::default()
    };
    stats.encryption = crypt.map(|c| c.describe()).unwrap_or_default();

    // One pool of threads for everything this file needs. Each stream gets its own memory of encoded inputs
    // (its plain samples are often encoded twice), gone with the stream, so memory doesn't grow with the file.
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

    // Streams are re-encoded first, in parallel (the biggest first, identical streams once); the results are
    // used in object order below, so the output doesn't depend on the number of threads.
    let streams: Vec<(u32, &Stream)> =
        objs.iter().filter_map(|(&n, (_, o))| if let Obj::Stream(s) = o { Some((n, &**s)) } else { None }).collect();
    let (same, unique) = identical_streams(&streams);
    let settings = effort_plan(&streams, &unique, opt, input.len(), pool);
    let mut order: Vec<usize> = (0..unique.len()).collect();
    order.sort_by_key(|&u| (std::cmp::Reverse(streams[unique[u]].1.data.len()), u));
    let dbg = std::env::var_os("PDFE_DEBUG").is_some();
    let mut encoded = par::map(pool, unique.len(), Some(&order), |u| {
        // Out of time: nothing more is encoded (the whole rewrite is given up below).
        if opt.deadline.passed() {
            return None;
        }
        let (n, s) = streams[unique[u]];
        let resolve = |o: &Obj| -> Option<Obj> {
            match o {
                Obj::Ref(n, _) => objs.get(n).map(|x| x.1.clone()),
                x => Some(x.clone()),
            }
        };
        let t = clock::now();
        // XMP stays as it is for tools that read it straight from the file, unless it is encrypted. (Whether
        // it is depends on the dictionary alone, so identical streams agree.)
        let xmp = s.dict.name(b"Type") == Some(b"Metadata")
            && crypt.is_some_and(|c| c.stream_method(n, &s.dict).is_ok_and(|m| m != Method::Identity));
        let r = settings[u].as_ref().and_then(|o| {
            let o = Options {
                flate: flate::Options {
                    shared: Some(flate::deflate::Shared::with_pool(workers.clone(), true)),
                    ..o.flate.clone()
                },
                ..o.clone()
            };
            if xmp {
                reencode_xmp(s, &o)
            } else {
                reencode(s, &o, &resolve)
            }
        });
        let short = r.is_some() && short_image(s, &resolve);
        let r = if short { None } else { r };
        if dbg {
            let new = r.as_ref().map_or(s.data.len(), |x| x.1.len());
            let how = settings[u]
                .as_ref()
                .map_or("kept, over the quick budget".to_string(), |o| format!("effort {}", o.effort));
            eprintln!(
                "stream {n}: {} -> {new} bytes, {how}, {:?}{}",
                s.data.len(),
                clock::since(t),
                if short { " (short image, kept)" } else { "" }
            );
        }
        r
    });
    if opt.deadline.passed() {
        return Err(TIMED_OUT.into());
    }
    if dbg && unique.len() < streams.len() {
        eprintln!("{} streams identical to an earlier one, encoded once", streams.len() - unique.len());
    }
    let slot: HashMap<usize, usize> = unique.iter().enumerate().map(|(u, &i)| (i, u)).collect();
    // Each result is moved out at the last stream that uses it (identical streams share one), so it is not
    // held twice.
    let mut uses = vec![0usize; unique.len()];
    for i in &same {
        uses[slot[i]] += 1;
    }

    // The JBIG2 data written for fax images, which their converter has decoded back already (see
    // `verified_jbig2`).
    let mut jbig2: HashMap<u32, Vec<u8>> = HashMap::new();
    // Every stream gets a direct /Length.
    let mut direct: BTreeMap<u32, (u16, Vec<u8>)> = BTreeMap::new(); // serialised indirect objects
    let mut packable: Vec<(u32, &Obj)> = Vec::new();
    let mut si = 0;
    for (&n, (g, o)) in &objs {
        match o {
            Obj::Stream(s) => {
                stats.streams += 1;
                debug_assert_eq!(streams[si].0, n);
                let u = slot[&same[si]];
                uses[u] -= 1;
                let result = if uses[u] == 0 { encoded[u].take() } else { encoded[u].clone() };
                let (dict, data) = match result {
                    Some((d, z, change)) => {
                        stats.streams_reencoded += 1;
                        stats.jpegs_retabled += change.jpeg_retabled as usize;
                        stats.fax_to_jbig2 += change.fax_to_jbig2 as usize;
                        if change.fax_to_jbig2 {
                            jbig2.insert(n, z.clone());
                        }
                        (d, z)
                    }
                    None => {
                        stats.streams_kept += 1;
                        (s.dict.clone(), s.data.clone())
                    }
                };
                si += 1;
                // Encrypted again with this object's key.
                let (mut dict, data) = match crypt {
                    Some(c) => c.seal_stream(n, *g, &dict, &data).map_err(|e| format!("object {n}: {e}"))?,
                    None => (dict, data),
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
                let body = match crypt {
                    Some(c) => to_bytes(&c.seal_object(n, *g, o).map_err(|e| format!("object {n}: {e}"))?),
                    None => to_bytes(o),
                };
                direct.insert(n, (*g, body));
            }
            _ => packable.push((n, o)),
        }
    }

    // Object streams, grouped by kind so similar objects sit together.
    packable.sort_by(|a, b| kind_key(a.1).cmp(&kind_key(b.1)).then(a.0.cmp(&b.0)));
    // New objects are numbered after the highest number the document uses (the encryption dictionary's
    // objects keep their numbers; a dangling reference keeps its number free), never after the old /Size:
    // that is a claim of the file, not something it holds, and the gate has already checked that no object
    // is numbered at or beyond it.
    let mut next_num = highest_number(&doc, &objs, crypt) + 1;
    let mut in_stm: HashMap<u32, (u32, u32)> = HashMap::new();
    let chunks: Vec<&[(u32, &Obj)]> = packable.chunks(opt.objstm_size.max(1)).collect();
    let mut raws = Vec::with_capacity(chunks.len());
    for (k, chunk) in chunks.iter().enumerate() {
        let sn = next_num + k as u32;
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
        raws.push((raw, first));
    }
    let zs = par::map(pool, raws.len(), None, |k| flate::deflate_zlib(&raws[k].0, &opt.flate));
    for (chunk, ((_, first), z)) in chunks.iter().zip(raws.iter().zip(zs)) {
        let sn = next_num;
        next_num += 1;
        let first = *first;
        let mut d = Dict::default();
        d.set(b"Type", Obj::Name(b"ObjStm".to_vec()));
        d.set(b"N", Obj::Int(chunk.len() as i64));
        d.set(b"First", Obj::Int(first as i64));
        d.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
        // Encrypted as a stream; the objects inside are not encrypted on their own.
        let (mut d, z) = match crypt {
            Some(c) => c.seal_stream(sn, 0, &d, &z).map_err(|e| format!("object stream: {e}"))?,
            None => (d, z),
        };
        d.set(b"Length", Obj::Int(z.len() as i64));
        let mut out = Vec::new();
        Writer { out: &mut out }.dict(&d);
        out.extend_from_slice(b"stream\n");
        out.extend_from_slice(&z);
        out.extend_from_slice(b"\nendstream");
        direct.insert(sn, (0, out));
        stats.objstms += 1;
    }
    // The encryption dictionary and its objects as they were: never encrypted, never in an object stream.
    if let Some(c) = crypt {
        if let Some((n, g)) = c.obj {
            direct.insert(n, (g, to_bytes(&Obj::Dict(c.dict.clone()))));
        }
        for (n, g, o) in &c.parts {
            direct.insert(*n, (*g, to_bytes(o)));
        }
    }

    // The file.
    let minor = if doc.version.0 == 1 { doc.version.1.max(5) } else { doc.version.1 };
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(format!("%PDF-{}.{}\n", doc.version.0, minor).as_bytes());
    // A comment of high bytes tells transfer tools the file is binary.
    out.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
    let mut offsets: HashMap<u32, (usize, u16)> = HashMap::new();
    // Each body is freed once it is in the file, so the objects are not held twice from here on (the
    // self-check reads the output back while the input's objects are still held).
    for (n, (g, body)) in direct {
        offsets.insert(n, (out.len(), g));
        out.extend_from_slice(format!("{n} {g} obj\n").as_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(b"\nendobj\n");
    }
    // Cross-reference stream.
    let xn = next_num;
    let size = xn + 1;
    let xref_off = out.len();
    offsets.insert(xn, (xref_off, 0));
    let max_off = offsets.values().map(|v| v.0).max().unwrap_or(0).max(xref_off);
    let max_f3 = in_stm
        .values()
        .map(|v| v.1 as usize)
        .max()
        .unwrap_or(0)
        .max(offsets.values().map(|v| v.1 as usize).max().unwrap_or(0))
        .max(65535);
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
    // PNG "Up" prediction usually helps the column-structured rows; keep whichever is smaller, counting the
    // bytes its parameters take in the dictionary.
    let cols = 1 + w1 + w2;
    let mut up = Vec::with_capacity(rows.len() + rows.len() / cols + 1);
    for (r, row) in rows.chunks(cols).enumerate() {
        up.push(2u8);
        for (i, &b) in row.iter().enumerate() {
            let above = if r == 0 { 0 } else { rows[(r - 1) * cols + i] };
            up.push(b.wrapping_sub(above));
        }
    }
    let mut both = par::map(pool, 2, None, |k| flate::deflate_zlib(if k == 0 { &rows } else { &up }, &opt.flate));
    let (z_up, z_plain) = (both.pop().unwrap(), both.pop().unwrap());
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

    // Read our own output back and compare everything reachable.
    self_check(&objs, &out, &doc.trailer, crypt, pool, &jbig2)?;
    // Anything written after the deadline passed (object streams, say) went out stored and is not an
    // output of these settings: a rewrite either finishes in time or gives nothing.
    if opt.deadline.passed() {
        return Err(TIMED_OUT.into());
    }
    Ok((out, stats))
}

/// Trailer keys that describe the cross-reference structure itself (rewritten, not copied).
const STRUCTURAL: [&[u8]; 9] =
    [b"Size", b"Prev", b"XRefStm", b"Index", b"W", b"Type", b"Filter", b"DecodeParms", b"Length"];

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
    content_within(s, 1 << 30)
}

/// `content_of` with each stage decoding to at most `limit` bytes (None beyond it, as for damaged data).
fn content_within(s: &Stream, limit: usize) -> Option<(Vec<u8>, Vec<Stage>)> {
    let stages = chain(&s.dict)?;
    let mut data = s.data.clone();
    let mut rest = Vec::new();
    for st in stages {
        if is_general(&st.name) && rest.is_empty() {
            data = decode_stage(&st, &data, limit).ok()?;
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

/// The same JPEG with other Huffman tables: the same DCT stage, every other byte the same, and the same coded
/// symbols in every scan (so the same coefficients).
fn same_jpeg(a: &(Vec<u8>, Vec<Stage>), b: &(Vec<u8>, Vec<Stage>)) -> bool {
    a.1 == b.1 && matches!(a.1.as_slice(), [st] if st.name == b"DCTDecode") && crate::jpeg::equivalent(&a.0, &b.0)
}

fn same_except_encoding(a: &Dict, b: &Dict) -> bool {
    let strip = |d: &Dict| -> Vec<(Vec<u8>, Obj)> {
        d.0.iter().filter(|(k, _)| !matches!(k.as_slice(), b"Length" | b"Filter" | b"DecodeParms")).cloned().collect()
    };
    strip(a) == strip(b)
}

/// A fax image the rewrite wrote as JBIG2 (`jbig2`: the data written for each object number): its converter
/// decoded these very bytes back to the image's pixels (`fax::pdf::jbig2_candidate`), so the output holding the
/// same bytes under that one filter needs no second decode (the bytes and the dictionary are still compared).
fn verified_jbig2(jbig2: &HashMap<u32, Vec<u8>>, n: u32, s: &Stream) -> bool {
    jbig2.get(&n).is_some_and(|z| *z == s.data)
        && s.dict.get(b"Filter") == Some(&Obj::Name(b"JBIG2Decode".to_vec()))
        && !s.dict.has(b"DecodeParms")
}

fn self_check(
    objs: &BTreeMap<u32, (u16, Obj)>,
    out: &[u8],
    trailer: &Dict,
    crypt: Option<&Crypt>,
    pool: Option<&par::Pool>,
    jbig2: &HashMap<u32, Vec<u8>>,
) -> Result<(), String> {
    let doc2 = xref::open(out).map_err(|e| format!("self-check: output unreadable: {e:?}"))?;
    // It opens the same way: the same encryption dictionary and object, file key and methods. Objects are then
    // compared decrypted.
    let same_crypt = match (crypt, &doc2.crypt) {
        (None, None) => true,
        (Some(a), Some(Ok(b))) => a == b,
        _ => false,
    };
    if !same_crypt {
        return Err("self-check: encryption differs".into());
    }
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
    // Objects are compared on the pool (the biggest streams first), each stream decoded to at most
    // `CHECK_ON_POOL` bytes there; a stream that needs more is compared afterwards, one at a time, so the check
    // holds at most two such copies per thread beside two of the largest stream. The first difference in object
    // order is the one reported, whatever the number of threads.
    let all: Vec<(&u32, &(u16, Obj))> = objs.iter().collect();
    let mut order: Vec<usize> = (0..all.len()).collect();
    order.sort_by_key(|&k| (std::cmp::Reverse(if let Obj::Stream(s) = &all[k].1 .1 { s.data.len() } else { 0 }), k));
    // None: the stream needs more than `limit` decoded bytes (or doesn't decode within it).
    let check = |k: usize, limit: usize| -> Option<Result<(), String>> {
        let (n, (g, o)) = all[k];
        let (g2, o2) = &objs2[n];
        if g != g2 {
            return Some(Err(format!("self-check: generation of {n} differs")));
        }
        match (o, o2) {
            (Obj::Stream(s1), Obj::Stream(s2)) => {
                if !same_except_encoding(&s1.dict, &s2.dict) {
                    return Some(Err(format!("self-check: dictionary of stream {n} differs")));
                }
                if s1.data == s2.data
                    && s1.dict.get(b"Filter") == s2.dict.get(b"Filter")
                    && s1.dict.get(b"DecodeParms") == s2.dict.get(b"DecodeParms")
                {
                    return Some(Ok(()));
                }
                if verified_jbig2(jbig2, *n, s2) {
                    return Some(Ok(()));
                }
                let full = limit == 1 << 30;
                let Some(c1) = content_within(s1, limit) else {
                    return full.then(|| Err(format!("self-check: stream {n} undecodable")));
                };
                let Some(c2) = content_within(s2, limit) else {
                    return full.then(|| Err(format!("self-check: stream {n} undecodable in output")));
                };
                // Different bytes are fine for a JPEG with other Huffman tables (the same coded symbols) and
                // for a CCITT image written as JBIG2 (the same pixels).
                let resolve = |o: &Obj| match o {
                    Obj::Ref(r, _) => objs.get(r).map(|x| x.1.clone()),
                    x => Some(x.clone()),
                };
                if c1 != c2 && !same_jpeg(&c1, &c2) && !crate::fax::pdf::same_image(s1, s2, &resolve) {
                    return Some(Err(format!("self-check: content of stream {n} differs")));
                }
                Some(Ok(()))
            }
            (x, y) => Some(if x != y { Err(format!("self-check: object {n} differs")) } else { Ok(()) }),
        }
    };
    let on_pool = par::map(pool, all.len(), Some(&order), |k| check(k, CHECK_ON_POOL));
    let mut checked = Vec::with_capacity(all.len());
    for (k, r) in on_pool.into_iter().enumerate() {
        checked.push(r.or_else(|| check(k, 1 << 30)).unwrap_or_else(|| Err("self-check: stream undecodable".into())));
    }
    checked.into_iter().collect()
}

/// Decoded bytes a stream may take, per copy, while it is compared on the thread pool (see `self_check`).
const CHECK_ON_POOL: usize = 16 << 20;

#[cfg(test)]
mod effort_tests {
    use super::*;

    fn flate_stream(decoded: &[u8], extra: &[(&[u8], Obj)]) -> Stream {
        let mut d = Dict::default();
        d.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
        for (k, v) in extra {
            d.set(k, v.clone());
        }
        Stream { dict: d, data: flate::deflate_zlib(decoded, &flate::Options::quick()) }
    }

    #[test]
    fn predictor_samples_are_whole_rows_inside_the_image() {
        assert_eq!(predictor_sample(1000, 300, 100), None);
        assert_eq!(predictor_sample(100, 300, 0), None, "small images are ranked whole");
        for (nrows, row, effort) in
            [(3000usize, 1000usize, 0u8), (5000, 4000, 50), (7, 200_000, 10), (1_000_000, 3, 99)]
        {
            let bands = predictor_sample(nrows, row, effort).unwrap();
            let mut last_end = 0;
            let mut rows = 0;
            for &(a, b) in &bands {
                assert!(a >= last_end && a < b && b <= nrows, "{bands:?}");
                last_end = b;
                rows += b - a;
            }
            // About the budget (at least one row per band).
            assert!(rows * row <= predictor_budget(effort).max(8 * row), "{nrows} x {row}: {rows} rows");
        }
        assert!(predictor_budget(0) < predictor_budget(40) && predictor_budget(40) < predictor_budget(99));
    }

    #[test]
    fn the_budget_goes_to_the_biggest_streams_first_and_grows_with_the_effort() {
        // A very compressible big stream (decodes to 1000 times its size in the file) and small text.
        let big = flate_stream(&vec![7u8; 2_000_000], &[]);
        let text: Vec<u8> = (0..20_000u32).map(|i| b"BT 12 Tf (abc) Tj ET\n"[(i % 21) as usize]).collect();
        let small = flate_stream(&text, &[]);
        let streams: Vec<(u32, &Stream)> = vec![(1, &small), (2, &big)];
        let unique = vec![0, 1];
        let input_len = 50_000;
        let plan = |e: u8| -> Vec<Option<u8>> {
            effort_plan(&streams, &unique, &Options::effort(e), input_len, None)
                .iter()
                .map(|o| o.as_ref().map(|o| o.effort))
                .collect()
        };
        assert_eq!(plan(100), vec![Some(100), Some(100)]);
        // Both fit in the quick budget (at least 4 MiB of decoded bytes).
        assert_eq!(plan(0), vec![Some(0), Some(0)]);
        // At 30 the budget (7.5 x 50 KB) is too small for the big one's 0.7 x 2 MB; the text fits.
        assert_eq!(plan(30), vec![Some(30), Some(0)]);
        // At 50 (30 x 50 KB) the big one, taken first, gets the cheaper 20 step; the text still fits.
        assert_eq!(plan(50), vec![Some(50), Some(20)]);
        // At 99 the budget (about 900 x 50 KB) takes both at full strength.
        assert_eq!(plan(99), vec![Some(99), Some(99)]);
        // Settings follow the effort given to each stream.
        let o = effort_plan(&streams, &unique, &Options::effort(50), input_len, None);
        assert!(o[1].as_ref().unwrap().flate.same_output(&flate::Options::effort(20)));
        assert!(o[0].as_ref().unwrap().flate.same_output(&flate::Options::effort(50)));
    }

    #[test]
    fn a_stream_too_big_for_its_step_gets_a_cheaper_one() {
        // 1 MB decoded. At effort 60 the budget is about 60 times the file: the 60 step needs 2.1 MB of extra
        // work, the 40 step 1.0 MB, the 20 step 0.7 MB.
        let data: Vec<u8> = (0..1_000_000u32).map(|i| if i % 1000 < 3 { 0 } else { 255 }).collect();
        let s = flate_stream(&data, &[]);
        let streams: Vec<(u32, &Stream)> = vec![(1, &s)];
        let step = |input_len: usize| {
            effort_plan(&streams, &[0], &Options::effort(60), input_len, None)[0].as_ref().unwrap().effort
        };
        assert_eq!(step(40_000), 60);
        assert_eq!(step(20_000), 40);
        assert_eq!(step(15_000), 20);
        assert_eq!(step(5_000), 0);
        let o = effort_plan(&streams, &[0], &Options::effort(60), 15_000, None);
        assert!(o[0].as_ref().unwrap().flate.same_output(&flate::Options::effort(20)));
    }

    #[test]
    fn the_fast_path_gives_images_a_lazy_level_and_the_bridge_the_strong_encoder() {
        let text: Vec<u8> = (0..60_000u32).map(|i| b"BT 12 Tf (abc) Tj ET\n"[(i % 21) as usize]).collect();
        let px: Vec<u8> = (0..200 * 150u32).map(|i| ((i % 200) + (i / 200)) as u8).collect();
        let page = flate_stream(&text, &[]);
        let image = flate_stream(&px, &[(b"Subtype", Obj::Name(b"Image".to_vec()))]);
        let streams: Vec<(u32, &Stream)> = vec![(1, &page), (2, &image)];
        let levels = |e: u8, input_len: usize| -> Vec<u8> {
            effort_plan(&streams, &[0, 1], &Options::effort(e), input_len, None)
                .iter()
                .map(|o| o.as_ref().unwrap().flate.fast)
                .collect()
        };
        // Effort 0: the page at the fast encoder's top level, the image at a lazy one.
        assert_eq!(levels(0, 10_000), vec![flate::fast::LEVELS, image_level(0)]);
        assert!(image_level(0) < flate::fast::LEVELS);
        // Images get closer parses as the effort grows; nothing gets the strong encoder below STRONG_FROM.
        for e in 1..STRONG_FROM {
            let l = levels(e, 10_000_000);
            assert!(l[0] == flate::fast::LEVELS && l[1] >= image_level(e - 1), "effort {e}: {l:?}");
        }
        // From STRONG_FROM the streams that take the most room get the strong encoder (fast 0) while their
        // decoded bytes fit in the bridge budget: none of these at 10 in a 10 KB file, both at 19.
        assert_eq!(levels(STRONG_FROM, 10_000), vec![flate::fast::LEVELS, image_level(STRONG_FROM)]);
        assert_eq!(levels(19, 10_000), vec![0, 0]);
        assert!(bridge_budget(9, 1000) == 0.0 && bridge_budget(20, 1000) == 0.0);
        assert!((STRONG_FROM..19).all(|e| bridge_budget(e + 1, 1000) == 2.0 * bridge_budget(e, 1000)));
        // A page that decodes to more than BIG_STREAM gets one pass of the near-optimal parse at effort 0, then
        // closer parses as images do, and the top level from STRONG_FROM until the bridge takes it (at 17 in a
        // 1 MB file).
        let mut d = Dict::default();
        d.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
        let big_text: Vec<u8> = text.iter().copied().cycle().take(BIG_STREAM + 1).collect();
        let fast1 = flate::Options { fast: 1, ..flate::Options::quick() };
        let big = Stream { dict: d, data: flate::deflate_zlib(&big_text, &fast1) };
        let streams: Vec<(u32, &Stream)> = vec![(1, &big)];
        let big_levels: Vec<u8> = (0..FAST_BELOW)
            .map(|e| effort_plan(&streams, &[0], &Options::effort(e), 1_000_000, None)[0].as_ref().unwrap().flate.fast)
            .collect();
        assert_eq!(big_levels[0], 9);
        assert!((0..STRONG_FROM).all(|e| big_levels[e as usize] == big_level(e)), "{big_levels:?}");
        assert!(big_levels.windows(2).take(16).all(|w| w[0] <= w[1]), "{big_levels:?}");
        assert_eq!(big_levels[STRONG_FROM as usize], flate::fast::LEVELS);
        assert_eq!(big_levels[16..], [flate::fast::LEVELS, 0, 0, 0]);
    }

    #[test]
    fn the_size_pass_stops_at_the_budget_but_not_inside_a_chain() {
        let text: Vec<u8> = (0..50_000u32).map(|i| b"0 0 m 10 10 l S\n"[(i % 16) as usize]).collect();
        let plain = flate_stream(&text, &[]);
        assert_eq!(decoded_size(&plain, 64 << 20, 50_000), Some(50_000));
        assert_eq!(decoded_size(&plain, 64 << 20, 49_999), None);
        assert_eq!(decoded_size(&plain, 49_999, usize::MAX), None);
        // Flate, then hex: the hex text is twice what the last stage gives, and only the last stage stops at
        // `last`, so the size is the same as with no stop at all.
        let mut hex: Vec<u8> = text.iter().flat_map(|b| format!("{b:02X}").into_bytes()).collect();
        hex.push(b'>');
        let mut d = Dict::default();
        d.set(b"Filter", Obj::Array(vec![Obj::Name(b"FlateDecode".to_vec()), Obj::Name(b"ASCIIHexDecode".to_vec())]));
        let chained = Stream { dict: d, data: flate::deflate_zlib(&hex, &flate::Options::quick()) };
        assert_eq!(decoded_size(&chained, 64 << 20, 50_000), Some(50_000));
        assert_eq!(decoded_size(&chained, 64 << 20, 50_000), decoded_size(&chained, 64 << 20, usize::MAX));
        // No general filter: the stored bytes.
        let raw = Stream { dict: Dict::default(), data: text };
        assert_eq!(decoded_size(&raw, 10, 10), Some(50_000));
    }

    #[test]
    fn below_20_a_stream_that_will_not_shrink_is_kept_as_it_is() {
        let resolve = |o: &Obj| Some(o.clone());
        let text: Vec<u8> = (0..40_000u32).map(|i| format!("{} {} l ", i % 97, i % 13).into_bytes()[0]).collect();
        let mut d = Dict::default();
        d.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
        // Stored with the strongest settings already: effort 0's re-encode is larger, so a bridge effort spends
        // nothing more on it, while effort 20 still tries the strong encoder.
        let s = Stream { dict: d.clone(), data: flate::deflate_zlib(&text, &flate::Options::max()) };
        assert!(flate::deflate_zlib(&text, &flate::Options::effort(0)).len() >= s.data.len());
        let strong = |e: u8| Options { flate: flate::Options::quick(), ..Options::effort(e) };
        assert!(reencode(&s, &strong(15), &resolve).is_none());
        assert!(reencode(&s, &Options::effort(0), &resolve).is_none());
        // Stored with effort 0's settings: the strong encoder's quick settings still shrink it. The bridge
        // leaves it alone, but the quick settings the plans of 20 and above give (effort 0, the strong
        // encoder) must try it exactly as before the fast path.
        let page: Vec<u8> = (0..1500u32)
            .flat_map(|i| {
                format!("{} {} {} {} re f q {} 0 0 1 {} cm Q\n", i % 97, i * 7 % 513, i % 41, i % 19, i % 5, i * 3 % 89)
                    .into_bytes()
            })
            .collect();
        let s = Stream { dict: d, data: flate::deflate_zlib(&page, &flate::Options::effort(0)) };
        assert!(flate::deflate_zlib(&page, &flate::Options::quick()).len() < s.data.len());
        assert!(reencode(&s, &strong(15), &resolve).is_none());
        let legacy_quick = Options { flate: flate::Options::quick(), effort: 0, ..Options::effort(50) };
        assert!(reencode(&s, &legacy_quick, &resolve).is_some());
        // Noise behind Flate takes more room than the noise itself: the bridge goes on and, like effort 0,
        // stores it without a filter.
        let mut r = 7u64;
        let noise: Vec<u8> = (0..5000)
            .map(|_| {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                r as u8
            })
            .collect();
        let mut d = Dict::default();
        d.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
        let s = Stream { dict: d, data: flate::deflate_zlib(&noise, &flate::Options::effort(0)) };
        assert!(s.data.len() > noise.len());
        for opt in [Options::effort(0), strong(15)] {
            let (d, z, _) = reencode(&s, &opt, &resolve).expect("stored without a filter");
            assert!(!d.has(b"Filter") && z == noise, "effort {}", opt.effort);
        }
    }

    #[test]
    fn a_stream_over_the_quick_budget_is_kept_as_it_is() {
        // 8 MB of blank image in a 20 KB file: the quick budget is 16 times the file at effort 0, twice as
        // much every 10 points, and at least 4 MiB. Another stream that takes more room in the file comes
        // first and fits.
        let blank = flate_stream(&vec![255u8; 8_000_000], &[]);
        let mut r = 1u64;
        let text: Vec<u8> = (0..200_000)
            .map(|_| {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                (r % 64) as u8 + 32
            })
            .collect();
        let other = flate_stream(&text, &[]);
        assert!(other.data.len() > blank.data.len());
        let streams: Vec<(u32, &Stream)> = vec![(1, &blank), (2, &other)];
        let plan = |e: u8, input_len: usize| -> Vec<Option<u8>> {
            effort_plan(&streams, &[0, 1], &Options::effort(e), input_len, None)
                .iter()
                .map(|o| o.as_ref().map(|o| o.effort))
                .collect()
        };
        for e in [0u8, 20, 40] {
            let p = plan(e, 20_000);
            assert!(p[0].is_none() && p[1].is_some(), "effort {e}: {p:?}");
        }
        // 20 KB x 16 x 2^5 = 10.2 MB at effort 50; effort 100 has no budget.
        assert!(plan(50, 20_000).iter().all(|o| o.is_some()));
        assert_eq!(plan(100, 20_000), vec![Some(100), Some(100)]);
        // A bigger file pays for it at effort 0 (16 x 600 KB = 9.6 MB).
        assert_eq!(plan(0, 600_000), vec![Some(0), Some(0)]);
        assert!(
            quick_budget(0, 1000) >= (4 << 20) as f64 && quick_budget(10, 1 << 20) == 2.0 * quick_budget(0, 1 << 20)
        );
    }

    #[test]
    fn a_predictor_is_kept_only_when_it_saves_more_than_its_parameters() {
        let resolve = |o: &Obj| Some(o.clone());
        let image = |w: usize, h: usize, cs: &[u8], px: Vec<u8>| {
            let mut d = Dict::default();
            d.set(b"Subtype", Obj::Name(b"Image".to_vec()));
            d.set(b"Width", Obj::Int(w as i64));
            d.set(b"Height", Obj::Int(h as i64));
            d.set(b"BitsPerComponent", Obj::Int(8));
            d.set(b"ColorSpace", Obj::Name(cs.to_vec()));
            Stream { dict: d, data: px }
        };
        let total = |d: &Dict, z: &[u8]| to_bytes(&Obj::Dict(d.clone())).len() + z.len();
        let cases = [
            // Blank images: a predictor saves a byte or two of data, and its parameters take about 70.
            (image(322, 333, b"DeviceGray", vec![0; 322 * 333]), false),
            (image(200, 700, b"DeviceGray", vec![255; 200 * 700]), false),
            (image(100, 100, b"DeviceRGB", vec![77; 100 * 100 * 3]), false),
            // A smooth image: the predictor saves far more than that.
            (
                image(256, 256, b"DeviceGray", (0..256 * 256).map(|i| ((i % 256) * 3 + (i / 256) * 5) as u8).collect()),
                true,
            ),
        ];
        for effort in [100u8, 0] {
            let opt = Options::effort(effort);
            for (k, (s, predicted)) in cases.iter().enumerate() {
                let (d, z, _) = reencode(s, &opt, &resolve).expect("Flate makes it smaller");
                let mut plain = s.dict.clone();
                set_chain(&mut plain, &[Stage { name: b"FlateDecode".to_vec(), parms: None }]);
                let plain_total = total(&plain, &flate::deflate_zlib(&s.data, &opt.flate));
                assert!(total(&d, &z) <= plain_total, "image {k}, effort {effort}: {} > {plain_total}", total(&d, &z));
                assert_eq!(d.has(b"DecodeParms"), *predicted, "image {k}, effort {effort}");
            }
        }
    }

    #[test]
    fn a_refused_thread_is_no_loss() {
        // A page with text and four different images, stored with quick settings (so there is work to do).
        let mut objs: Vec<Vec<u8>> =
            vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()];
        objs.push(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /A 5 0 R /B 6 0 R /C 7 0 R /D 8 0 R >> >> >>".to_vec());
        let stream = |dict: &str, decoded: &[u8]| {
            let data = flate::deflate_zlib(decoded, &flate::Options::quick());
            let mut o = format!("<< {dict} /Filter /FlateDecode /Length {} >>\nstream\n", data.len()).into_bytes();
            o.extend_from_slice(&data);
            o.extend_from_slice(b"\nendstream");
            o
        };
        let text: Vec<u8> = (0..400)
            .flat_map(|i| format!("q {} 0 0 50 10 {} cm /A Do Q BT ({i}) Tj ET\n", 40 + i % 9, i % 700).into_bytes())
            .collect();
        objs.push(stream("", &text));
        for k in 0..4usize {
            let px: Vec<u8> =
                (0..160 * 120).map(|i| ((i % 160) * (k + 1) / 2 + (i / 160) * (3 - k % 2)) as u8).collect();
            objs.push(stream(
                "/Type /XObject /Subtype /Image /Width 160 /Height 120 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &px,
            ));
        }
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(o);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for off in offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes(),
        );
        for effort in [0u8, 10, 100] {
            let one = optimize(&pdf, &Options { threads: 1, ..Options::effort(effort) });
            assert!(matches!(one.outcome, Outcome::Smaller(_)), "{:?}", one.outcome);
            // The system gives no thread, one, or two of the three asked for.
            for allowed in [0, 1, 2] {
                let _refuse = par::tests::RefuseSpawns::after(allowed);
                let r = optimize(&pdf, &Options { threads: 4, ..Options::effort(effort) });
                assert!(r.data == one.data, "effort {effort}, {allowed} threads allowed");
            }
        }
    }

    /// A one-page PDF whose content stream is `content`, stored with the fast encoder's quickest level.
    fn page_with(content: &[u8]) -> Vec<u8> {
        let data = flate::deflate_zlib(content, &flate::Options { fast: 1, ..flate::Options::quick() });
        let mut objs: Vec<Vec<u8>> =
            vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()];
        objs.push(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec());
        let mut o = format!("<< /Filter /FlateDecode /Length {} >>\nstream\n", data.len()).into_bytes();
        o.extend_from_slice(&data);
        o.extend_from_slice(b"\nendstream");
        objs.push(o);
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(o);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for off in offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes(),
        );
        pdf
    }

    #[test]
    fn only_the_jbig2_bytes_written_are_taken_as_decoded_already() {
        let z = vec![1u8, 2, 3, 4];
        let jbig2: HashMap<u32, Vec<u8>> = [(4, z.clone())].into_iter().collect();
        let mut d = Dict::default();
        d.set(b"Filter", Obj::Name(b"JBIG2Decode".to_vec()));
        assert!(verified_jbig2(&jbig2, 4, &Stream { dict: d.clone(), data: z.clone() }));
        // Another object, other bytes, the filter in an array, decode parameters or another filter: compared
        // in full.
        assert!(!verified_jbig2(&jbig2, 5, &Stream { dict: d.clone(), data: z.clone() }));
        assert!(!verified_jbig2(&jbig2, 4, &Stream { dict: d.clone(), data: vec![1, 2, 3] }));
        let mut e = d.clone();
        e.set(b"Filter", Obj::Array(vec![Obj::Name(b"JBIG2Decode".to_vec())]));
        assert!(!verified_jbig2(&jbig2, 4, &Stream { dict: e, data: z.clone() }));
        let mut f = d.clone();
        f.set(b"DecodeParms", Obj::Dict(Dict::default()));
        assert!(!verified_jbig2(&jbig2, 4, &Stream { dict: f, data: z.clone() }));
        let mut g = d;
        g.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
        assert!(!verified_jbig2(&jbig2, 4, &Stream { dict: g, data: z }));
    }

    #[test]
    fn the_self_check_finds_a_wrong_stream_on_the_pool_and_after_it() {
        // One content stream below the bytes a comparison may take on the pool, one above (compared after the
        // pool, one at a time). Each is checked against the output of a file that differs in one byte of it.
        for size in [100_000usize, CHECK_ON_POOL + 100_000] {
            // Drawing instructions with varied numbers, so that a closer parse finds something to gain.
            let mut content = Vec::with_capacity(size + 32);
            let mut r = 1u64;
            while content.len() < size {
                r = r.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                content.extend_from_slice(format!("{} {} l\n", (r >> 40) % 600, (r >> 20) % 800).as_bytes());
            }
            content.truncate(size);
            let mut other = content.clone();
            other[size / 2] = b'9';
            let (a, b) = (page_with(&content), page_with(&other));
            let doc = xref::open(&a).unwrap();
            let objs = load_reachable(&doc).unwrap();
            for threads in [1usize, 3] {
                let opt = Options { threads, ..Options::effort(0) };
                let (out_a, out_b) = (optimize(&a, &opt), optimize(&b, &opt));
                assert!(matches!(out_a.outcome, Outcome::Smaller(_)), "{size}: {:?}", out_a.outcome);
                let pool = par::Pool::new(threads);
                assert_eq!(self_check(&objs, &out_a.data, &doc.trailer, None, Some(&pool), &HashMap::new()), Ok(()));
                let wrong = self_check(&objs, &out_b.data, &doc.trailer, None, Some(&pool), &HashMap::new());
                assert_eq!(
                    wrong,
                    Err("self-check: content of stream 4 differs".to_string()),
                    "{size}, {threads} threads"
                );
            }
        }
    }

    #[test]
    fn identical_streams_are_found_by_value() {
        let a = flate_stream(b"same bytes", &[]);
        let b = flate_stream(b"same bytes", &[]);
        let c = flate_stream(b"same bytes", &[(b"Length1", Obj::Int(3))]);
        let d = flate_stream(b"other bytes", &[]);
        let streams: Vec<(u32, &Stream)> = vec![(1, &a), (2, &c), (3, &b), (4, &d), (5, &a)];
        let (same, unique) = identical_streams(&streams);
        assert_eq!(same, vec![0, 1, 0, 3, 0]);
        assert_eq!(unique, vec![0, 1, 3]);
    }
}
