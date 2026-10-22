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
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

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
    /// Threads to use (0: every core the machine offers, at most 8; at most 64). Never changes the output.
    pub threads: usize,
    /// Bytes of encoded outputs one file keeps, so that an input another stream or step also encodes is
    /// encoded once. The oldest go first; 0 keeps none. Never changes the output.
    pub store_budget: usize,
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
            store_budget: 2 << 20,
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
    // Everything reachable is read: the object streams decoded for it go.
    doc.drop_cache();
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
    let mut dangling = HashSet::new();
    for (_, o) in objs.values() {
        let mut r = Vec::new();
        match o {
            // A stream's /Length is replaced by a direct number, so its target doesn't matter.
            Obj::Stream(st) => st.dict.0.iter().filter(|(k, _)| k != b"Length").for_each(|(_, v)| v.refs(&mut r)),
            _ => o.refs(&mut r),
        }
        dangling.extend(r.into_iter().filter(|(n, _)| !objs.contains_key(n)));
    }
    if holds_header(doc.data, &dangling) {
        return Err("an object in the file is missing from its cross-reference data".into());
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
                match metadata_text(s) {
                    Some(data) if contains(&data, b"pdfaid:part") || contains(&data, b"pdfaid:conformance") => {
                        why.get_or_insert("PDF/A".into());
                    }
                    Some(_) => {}
                    None => {
                        why.get_or_insert("metadata too large to check for PDF/A".into());
                    }
                }
            }
        }
    }
    match why {
        Some(w) => Err(w),
        None => Ok(()),
    }
}

/// A metadata stream's text for the PDF/A check, each stage decoded to at most 16 MiB: None when a stage would
/// decode further (the check can't be made, so the file is left as it is), empty when it doesn't decode.
fn metadata_text(s: &Stream) -> Option<Vec<u8>> {
    let Some(stages) = chain(&s.dict) else { return Some(Vec::new()) };
    let mut data = s.data.clone();
    for st in &stages {
        data = match decode_stage(st, &data, 16 << 20) {
            Ok(d) => d,
            Err(super::filters::FilterError::TooLarge) => return None,
            Err(_) => return Some(Vec::new()),
        };
        if st.name == b"FlateDecode" || st.name == b"LZWDecode" {
            let Ok(d) = super::filters::unpredict(st, &data) else { return Some(Vec::new()) };
            data = d;
        }
    }
    Some(data)
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

/// Whether `data` holds "n g obj" for some (n, g) of `refs`, anywhere, as `format!("{n} {g} obj")` writes
/// it: the same answer as a search for each, in one pass. Before each " obj" must come g's digits (all of
/// them, as g is written after a space), a space, and n's digits; n is every number the digits before that
/// space end with ("112 0 obj" holds "12 0 obj"). A number is written without leading zeros.
fn holds_header(data: &[u8], refs: &HashSet<(u32, u16)>) -> bool {
    if refs.is_empty() {
        return false;
    }
    // The digits that end at `end`, at most `most` of them.
    let digits = |end: usize, most: usize| {
        data[end.saturating_sub(most)..end].iter().rev().take_while(|c| c.is_ascii_digit()).count()
    };
    let number = |t: &[u8]| -> Option<u64> {
        if t.len() > 1 && t[0] == b'0' {
            return None;
        }
        t.iter().try_fold(0u64, |v, &c| v.checked_mul(10)?.checked_add((c - b'0') as u64))
    };
    for at in 0..data.len().saturating_sub(3) {
        if &data[at..at + 4] != b" obj" {
            continue;
        }
        // g: every digit before " obj" (more than five can't be a u16), after a space.
        let k = digits(at, 6);
        if k == 0 || k > 5 || at < k + 1 || data[at - k - 1] != b' ' {
            continue;
        }
        let Some(g) = number(&data[at - k..at]).and_then(|g| u16::try_from(g).ok()) else { continue };
        let end = at - k - 1;
        // n: each number of up to ten digits (a u32) that ends at that space.
        for j in 1..=digits(end, 10) {
            let n = number(&data[end - j..end]).and_then(|n| u32::try_from(n).ok());
            if n.is_some_and(|n| refs.contains(&(n, g))) {
                return true;
            }
        }
    }
    false
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
/// With a new encoding, `content` gets the stream's content (see `content_of`) when it can be had from the
/// decoding done here, so the stream is not decoded again to check the new data or its size.
fn reencode(
    s: &Stream,
    opt: &Options,
    resolve: &dyn Fn(&Obj) -> Option<Obj>,
    content: &mut Option<Content>,
) -> Option<Reencoded> {
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
    // The length of our Flate over `data` as it is, once made (repredict() compares with it).
    let mut flate_len = None;
    if !(adds_layer && shape_matters) && !(over_jpeg && !opt.flate_over_jpeg) {
        let bare_len = best_len;
        let mut with_flate = vec![Stage { name: b"FlateDecode".to_vec(), parms: carried.clone() }];
        with_flate.extend_from_slice(rest);
        let mut d = s.dict.clone();
        set_chain(&mut d, &with_flate);
        let z = flate::deflate_zlib(jpeg.as_ref().unwrap_or(&data), &opt.flate);
        flate_len = jpeg.is_none().then_some(z.len());
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
    // chosen without encoding, and compares whole sizes (the dictionaries counted). (Without a codec after the
    // general filters, our Flate over `data` was made above.)
    if rest.is_empty() && data.len() <= 48 << 20 && opt.flate.fast > 0 && opt.effort < TRIAL_PREDICTORS {
        let dict = best.as_ref().map_or(&s.dict, |b| &b.0);
        let plain = best_len + to_bytes(&Obj::Dict(dict.clone())).len();
        if let Some((d, z)) = repredict_fast(s, &data, carried.as_ref(), opt, resolve, plain) {
            best = Some((d, z));
            best_retabled = false;
        }
    } else if rest.is_empty() && data.len() <= 48 << 20 {
        if let Some((d, z)) = flate_len.and_then(|n| repredict(s, &data, n, carried.as_ref(), opt, resolve)) {
            if z.len() < best_len {
                best = Some((d, z));
                best_retabled = false;
            }
        }
    }
    let best = best.map(|(d, z)| (d, z, Change { jpeg_retabled: best_retabled, ..Change::default() }));
    // The content as `content_of` decodes it: the general filters gave these same bytes (it decodes with a
    // limit no lower), and only the last of them can have a predictor (checked above). Kept when decoding
    // it again would cost something: when the filters decompress (Flate, LZW).
    let decompress = stages[..k].iter().any(|st| matches!(st.name.as_slice(), b"FlateDecode" | b"LZWDecode"));
    if best.is_some() && decompress && opt.max_stream <= CHECK_LIMIT {
        let samples = match stages[..k].last() {
            Some(st) if predicted(st) => super::filters::unpredict(st, &data).ok(),
            _ => Some(data),
        };
        *content = samples.map(|d| (d, rest.to_vec()));
    }
    best
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

/// The predictor candidates in ranking order (the first of equal scores wins).
const CANDIDATES: [Option<images::Choice>; 5] = [
    None,
    Some(images::Choice::MinSum),
    Some(images::Choice::Fixed(2)),
    Some(images::Choice::Fixed(1)),
    Some(images::Choice::Fixed(4)),
];

/// Candidate inputs up to this size are ranked side by side; bigger ones one after another (their segments
/// still run in parallel), so a big image never has more than two copies alive.
const RANK_TOGETHER: usize = 8 << 20;

/// The smallest score, the first of equals: the pick of a loop where only a strictly smaller score replaces
/// the best so far.
fn best_of(scores: &[u64]) -> Option<usize> {
    (0..scores.len()).min_by_key(|&k| (scores[k], k))
}

/// The image's samples with the PNG predictor (or none) that suits them best, and their encode. `flate_len` is
/// the length of our Flate over `data` as it is, which reencode() has already made.
fn repredict(
    s: &Stream,
    data: &[u8],
    flate_len: usize,
    carried: Option<&Dict>,
    opt: &Options,
    resolve: &dyn Fn(&Obj) -> Option<Obj>,
) -> Option<(Dict, Vec<u8>)> {
    let (g, px) = samples(s, data, carried, resolve)?;
    let (row, bpp) = (g.row(), g.bpp());
    // The candidates are ranked by their size with the quick settings plus the size of their stream
    // dictionary (the predictor parameters take room too), and only the best one gets the full encode. Below
    // effort 100 the data is a sample of rows across the image, so the dictionary counts in proportion. The
    // first of equal sizes wins. Only their sizes count, so their outputs are not remembered, and they are
    // ranked side by side when threads are free.
    // On the fast path the candidates are ranked with the level effort 0 gives images, else with the quick
    // settings.
    let rank = flate::Options { fast: if opt.flate.fast > 0 { image_level(0) } else { 0 }, ..flate::Options::quick() };
    let quick = flate::Options { shared: opt.flate.shared.as_ref().map(|sh| sh.without_store()), ..rank };
    let nrows = px.len() / row;
    let rows = predictor_sample(nrows, row, opt.effort);
    let sampled = rows.as_ref().map_or(nrows, |r| r.iter().map(|&(a, b)| b - a).sum());
    let dict_with = |parms: &Option<Dict>| {
        let mut d = s.dict.clone();
        set_chain(&mut d, &[Stage { name: b"FlateDecode".to_vec(), parms: parms.clone() }]);
        d
    };
    let dict_len = |parms: &Option<Dict>| to_bytes(&Obj::Dict(dict_with(parms))).len();
    // The parameters of every candidate with a predictor, and both dictionary sizes.
    let png = png_parms(&g);
    let (dict_plain, dict_png) = (dict_len(&None) as u64, dict_len(&Some(png.clone())) as u64);
    // A candidate's score: (data for all rows + dictionary) x sampled rows, in whole numbers. Its input (the
    // sampled rows below effort 100) is dropped once encoded; the winner is filtered again below.
    let score = |k: usize| -> u64 {
        let input: Cow<[u8]> = match (&rows, CANDIDATES[k]) {
            (None, None) => Cow::Borrowed(&px[..]),
            (None, Some(how)) => Cow::Owned(images::png_filter(&px, row, bpp, how)),
            (Some(r), None) => Cow::Owned(r.iter().flat_map(|&(a, b)| &px[a * row..b * row]).copied().collect()),
            (Some(r), Some(how)) => Cow::Owned(images::filter_rows(&px, row, bpp, how, r)),
        };
        let data_len = flate::deflate_zlib(&input, &quick).len() as u64;
        let dict = if CANDIDATES[k].is_none() { dict_plain } else { dict_png };
        data_len * nrows as u64 + dict * sampled as u64
    };
    // The size of a filtered candidate's input (the biggest): above RANK_TOGETHER the five go one at a time.
    let input_len = rows.as_ref().map_or(px.len() + nrows, |r| r.iter().map(|&(a, b)| (b - a) * (row + 1)).sum());
    let pool = quick.shared.as_ref().map(|sh| &*sh.pool).filter(|_| input_len <= RANK_TOGETHER);
    let k = best_of(&par::map(pool, CANDIDATES.len(), None, score))?;
    let parms = CANDIDATES[k].map(|_| png);
    let bytes = match CANDIDATES[k] {
        None => px,
        Some(how) => images::png_filter(&px, row, bpp, how),
    };
    // The best is `data` itself (the plain samples without a predictor before, or the same predictor bytes):
    // reencode() has already tried our Flate over it.
    if bytes == data {
        return None;
    }
    let z = flate::deflate_zlib(&bytes, &opt.flate);
    // Without a predictor before, `data` is the plain samples: a predictor that saves fewer bytes than its
    // parameters take is not kept.
    if parms.is_some() && carried.is_none() && z.len() + dict_len(&parms) >= flate_len + dict_len(&None) {
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
/// `content`: the stream's content (see `content_of`) when it is known already.
fn short_image(s: &Stream, resolve: &dyn Fn(&Obj) -> Option<Obj>, content: Option<&Content>) -> bool {
    let Some(g) = images::geometry(&s.dict, resolve) else { return false };
    let Some(h) = s.dict.get(b"Height").and_then(|x| x.as_int()) else { return false };
    let decoded;
    let content = match content {
        Some(c) => Some(c),
        None => {
            decoded = content_of(s);
            decoded.as_ref()
        }
    };
    match content {
        Some((px, rest)) => rest.is_empty() && (px.len() as u128) < g.row() as u128 * h.max(0) as u128,
        None => false,
    }
}

/// What a stream's encoders start from: its data, /Filter and /DecodeParms.
type Input<'a> = (&'a [u8], Option<&'a Obj>, Option<&'a Obj>);

/// For each stream, the index of the first stream with the same dictionary and data (itself when there is
/// none); the list of those first ones; and for each first one, whether another first one has the same
/// encoder input (data, /Filter and /DecodeParms) under another dictionary. Identical streams are re-encoded
/// once.
///
/// Streams are found by value in two maps, so the time is linear whatever the data: an id for each encoder
/// input, then the first stream of each (input, dictionary), which is exactly stream equality. The maps are
/// only looked up, never walked, so their random hash keys can't reach the output.
fn identical_streams(streams: &[(u32, &Stream)]) -> (Vec<usize>, Vec<usize>, Vec<bool>) {
    let mut inputs: HashMap<Input, usize> = HashMap::new();
    let mut firsts: HashMap<(usize, &Dict), usize> = HashMap::new();
    let mut same = Vec::with_capacity(streams.len());
    let mut unique = Vec::new();
    // For each first stream, its input's id; for each input, how many first streams have it.
    let mut input_of = Vec::new();
    let mut count: Vec<usize> = Vec::new();
    for (i, (_, s)) in streams.iter().enumerate() {
        let next = inputs.len();
        let id = *inputs.entry((s.data.as_slice(), s.dict.get(b"Filter"), s.dict.get(b"DecodeParms"))).or_insert(next);
        if id == count.len() {
            count.push(0);
        }
        let first = *firsts.entry((id, &s.dict)).or_insert(i);
        if first == i {
            unique.push(i);
            input_of.push(id);
            count[id] += 1;
        }
        same.push(first);
    }
    let shares = input_of.iter().map(|&id| count[id] > 1).collect();
    (same, unique, shares)
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

/// Effort 0 gives images the lazy level with two steps of lookahead, the fastest level that pays. With images
/// at each level and everything else as at effort 0, the 90 files the sample rewrites come to (output as a
/// share of their input, and the time against level 7's): 7 87.02%; 8 87.04%, 1.3x; 9 86.70%, 1.45x; 10
/// 86.63%, 1.6x; 11 86.37%, 1.95x; 12 86.30%, 2.15x. Level 8 is the one level weaker than 7 on images, so the
/// scale goes from 7 to 9.
const IMAGE_LEVEL: [u8; FAST_BELOW as usize] =
    [7, 7, 7, 9, 9, 9, 10, 10, 11, 11, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12];

/// A stream other than an image that decodes to more than this gets at most `big_level` below `FAST_BELOW`.
/// The top level parses about 6 MB a second on one thread, so this much takes about 0.65 s: past it, one
/// stream would hold the whole file up (a 10 MB CJK font took 1.5 s of a 5.5 MB file's 1.8 s).
const BIG_STREAM: usize = 4 << 20;

/// The fast encoder's level for a stream over `BIG_STREAM` below `FAST_BELOW`: level 9 (one pass of the
/// near-optimal parse, 40% faster than the top level, 0.2% larger on fonts and 1.4% on page content) up to
/// effort 5, 10 from 6 and the top level again from 10. Level 11 is left out: on a 4.9 MB CFF font it was
/// larger than both 10 and 12, so efforts 8 and 9 would have made a larger file than 6 and 7.
fn big_level(effort: u8) -> u8 {
    match image_level(effort) {
        l if l < 10 => 9,
        l if l < 12 => 10,
        _ => flate::fast::LEVELS,
    }
}

/// The Flate settings for the object streams and the cross-reference stream (`raw`: the bytes to encode): the
/// effort's own, except on the bridge from `STRONG_FROM`, where they get the strong encoder's quick settings
/// when those bytes fit in `bridge_budget` (a budget of their own, as large as the streams' one), so that
/// effort 19 ends where 20 begins.
fn structure_flate(opt: &Options, raw: usize, input_len: usize) -> flate::Options {
    if opt.flate.fast > 0
        && (STRONG_FROM..FAST_BELOW).contains(&opt.effort)
        && raw as f64 <= bridge_budget(opt.effort, input_len)
    {
        flate::Options { shared: opt.flate.shared.clone(), deadline: opt.flate.deadline, ..flate::Options::quick() }
    } else {
        opt.flate.clone()
    }
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
    // level of the fast encoder, and so does a stream over `BIG_STREAM` up to effort 9. From `STRONG_FROM` on,
    // the streams that take the most room get the strong encoder's quick settings instead (what effort 0 was
    // before the fast path) while their decoded bytes fit in `bridge_budget`, so the time and the size go on
    // smoothly to 20. A stream whose filters don't decode within `max_stream` is kept as it is (`reencode` would
    // find the same).
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

/// What a stream job is given: its stream (borrowed, never copied), its settings and the few objects its
/// encoders look up. Never the object table, the encryption or another job's stream.
struct Job<'a> {
    n: u32,
    s: &'a Stream,
    /// From `effort_plan`; None: the stream is kept as it is.
    settings: Option<Options>,
    /// An encrypted XMP stream (see `reencode_xmp`), decided while planning (that needs the encryption).
    xmp: bool,
    /// Another job has the same encoder input (see `identical_streams`).
    shares: bool,
    /// The only objects it may look up (see `job_refs`).
    refs: Vec<(u32, Looked<'a>)>,
}

/// An object a job may look up; of a stream, only its dictionary.
enum Looked<'a> {
    Obj(&'a Obj),
    Dict(&'a Dict),
}

impl Job<'_> {
    /// What the object table gives for `o`: a reference followed once, anything else as it is. Only the
    /// objects in `refs` are there, as if every other were missing; a stream comes with its dictionary and no
    /// data.
    fn resolve(&self, o: &Obj) -> Option<Obj> {
        let Obj::Ref(n, _) = o else { return Some(o.clone()) };
        self.refs.iter().find(|r| r.0 == *n).map(|(_, l)| match l {
            Looked::Obj(o) => (*o).clone(),
            Looked::Dict(d) => Obj::Stream(Box::new(Stream { dict: (*d).clone(), data: Vec::new() })),
        })
    }
}

/// The objects a stream's encoders may look up: what Width, Height and ColorSpace refer to (a fax image reads
/// Width and Height through them, `images::components` the colour space), and when the colour space, direct
/// or looked up, is an array, what its element 1 refers to (an ICC profile, whose /N it reads). A reference
/// to a missing object is left out, so the job finds it missing too.
fn job_refs<'a>(d: &'a Dict, objs: &'a BTreeMap<u32, (u16, Obj)>) -> Vec<(u32, Looked<'a>)> {
    let mut refs: Vec<(u32, Looked<'a>)> = Vec::new();
    let mut look = |o: &'a Obj| -> Option<&'a Obj> {
        let Obj::Ref(n, _) = o else { return Some(o) };
        let (_, t) = objs.get(n)?;
        if !refs.iter().any(|r| r.0 == *n) {
            refs.push((*n, if let Obj::Stream(s) = t { Looked::Dict(&s.dict) } else { Looked::Obj(t) }));
        }
        Some(t)
    };
    for k in [&b"Width"[..], b"Height"] {
        if let Some(v) = d.get(k) {
            look(v);
        }
    }
    if let Some(Obj::Array(a)) = d.get(b"ColorSpace").and_then(&mut look) {
        if let Some(e) = a.get(1) {
            look(e);
        }
    }
    refs
}

/// One stream job: the stream's new encoding (None: it is kept as it is) and the job's check of it (see
/// `prove`). It is given its `Job` only, so it can't reach the object table, the encryption or another job.
/// Its encodes run on the pool of `store`, the file's store of outputs (see `Options::store_budget`). With
/// `dbg` it prints a line.
fn run_job(
    job: &Job,
    store: &Arc<flate::deflate::Shared>,
    opt: &Options,
    dbg: bool,
) -> (Option<Reencoded>, Option<Proof>) {
    // Out of time: nothing more is encoded (the whole rewrite is given up below).
    if opt.deadline.passed() {
        return (None, None);
    }
    let (n, s) = (job.n, job.s);
    let resolve = |o: &Obj| job.resolve(o);
    let t = clock::now();
    let mut content = None;
    let r = job.settings.as_ref().and_then(|o| {
        let o = Options { flate: flate::Options { shared: Some(store.clone()), ..o.flate.clone() }, ..o.clone() };
        if job.xmp {
            reencode_xmp(s, &o)
        } else {
            reencode(s, &o, &resolve, &mut content)
        }
    });
    #[cfg(test)]
    let r = check_tests::encoder_fault(n, r);
    let short = r.is_some() && short_image(s, &resolve, content.as_ref());
    let r = if short { None } else { r };
    // The new data checked against the stream's content here, in parallel and without decoding the
    // stream again, so the self-check only has to find the same data in the output.
    let proof = match (&r, content) {
        (Some(r), Some(c)) => prove(n, s, &c, r),
        _ => None,
    };
    if dbg {
        let new = r.as_ref().map_or(s.data.len(), |x| x.1.len());
        let how =
            job.settings.as_ref().map_or("kept, over the quick budget".to_string(), |o| format!("effort {}", o.effort));
        eprintln!(
            "stream {n}: {} -> {new} bytes, {how}, {:?}{}",
            s.data.len(),
            clock::since(t),
            if short { " (short image, kept)" } else { "" }
        );
    }
    (r, proof)
}

fn rewrite(input: &[u8], opt: &Options) -> Result<(Vec<u8>, Stats), String> {
    // Object streams are read once each, within max_stream (see `xref::Limits::once`).
    let doc = xref::open_with(input, xref::Limits::once(opt.max_stream)).map_err(|e| format!("unreadable: {e:?}"))?;
    // An encrypted file the empty password doesn't open stays as it is. One it opens is read decrypted and
    // written with the same encryption: same dictionary, /ID, file key and permissions.
    if let Some(Err(why)) = &doc.crypt {
        return Err(format!("encrypted: {why}"));
    }
    if doc.crypt.is_some() && !opt.rewrite_encrypted {
        return Err("encrypted: left as it is (rewrite_encrypted is off)".into());
    }
    let crypt = doc.crypt.as_ref().and_then(|c| c.as_ref().ok());
    let objs = load_reachable(&doc);
    if std::env::var_os("PDFE_DEBUG").is_some() {
        let s = doc.objstm_stats();
        eprintln!(
            "object streams: {} decoded, largest {} B, {} B decoded and {} B read of a budget of {} B, at most {} B \
             held, {} integers read again, {} header entries out of order, {} sharing an offset",
            s.decoded,
            s.largest,
            s.decoded_bytes,
            s.stored_bytes,
            s.budget,
            s.peak_held,
            s.memo_hits,
            s.unordered,
            s.shared
        );
    }
    let objs = objs?;
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

    // One pool of threads for everything this file needs.
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
    let (same, unique, shares) = identical_streams(&streams);
    let settings = effort_plan(&streams, &unique, opt, input.len(), pool);
    // Each job is given its stream, its settings and the few objects its encoders look up: nothing else of
    // the file.
    let jobs: Vec<Job> = unique
        .iter()
        .zip(settings)
        .zip(shares)
        .map(|((&i, settings), shares)| {
            let (n, s) = streams[i];
            // XMP stays as it is for tools that read it straight from the file, unless it is encrypted.
            // (Whether it is depends on the dictionary alone, so identical streams agree.)
            let xmp = s.dict.name(b"Type") == Some(b"Metadata")
                && crypt.is_some_and(|c| c.stream_method(n, &s.dict).is_ok_and(|m| m != Method::Identity));
            Job { n, s, settings, xmp, shares, refs: job_refs(&s.dict, &objs) }
        })
        .collect();
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    order.sort_by_key(|&u| (std::cmp::Reverse(jobs[u].s.data.len()), u));
    let dbg = std::env::var_os("PDFE_DEBUG").is_some();
    // One store of encoded outputs for the whole file: an input that two jobs encode with the same settings
    // (two streams with the same data under other dictionaries, say) is encoded once. It goes with the file.
    let store = flate::deflate::Shared::with_store(workers.clone(), opt.store_budget);
    #[cfg(test)]
    let running = job_tests::Running::default();
    let done = par::map(pool, jobs.len(), Some(&order), |u| {
        #[cfg(test)]
        let _running = running.enter();
        run_job(&jobs[u], &store, opt, dbg)
    });
    #[cfg(test)]
    job_tests::MOST_JOBS.set(running.most());
    #[cfg(test)]
    job_tests::STORE.set(store.stats());
    let sharing = jobs.iter().filter(|j| j.shares).count();
    drop(jobs);
    let (mut encoded, proofs): (Vec<_>, Vec<_>) = done.into_iter().unzip();
    if opt.deadline.passed() {
        return Err(TIMED_OUT.into());
    }
    if dbg {
        let st = store.stats();
        // The time saved in whole milliseconds: formatting an f64 would put about 8 KB of gzip into the
        // WebAssembly module, where this line is never printed.
        let ms = st.saved.as_millis();
        eprintln!(
            "plan: {} streams, {} jobs ({} identical to an earlier one, {sharing} sharing their input); store: \
             budget {}, peak {} bytes, {} kept, {} hits, {} computed, about {}.{:03} s saved; segments planned once: \
             {}",
            streams.len(),
            unique.len(),
            streams.len() - unique.len(),
            opt.store_budget,
            st.peak,
            st.kept,
            st.hits,
            st.computed,
            ms / 1000,
            ms % 1000,
            st.reused_segments
        );
    }
    // The outputs in the store are not needed any more.
    drop(store);
    let slot: HashMap<usize, usize> = unique.iter().enumerate().map(|(u, &i)| (i, u)).collect();
    // Each result is moved out at the last stream that uses it (identical streams share one), so it is not
    // held twice.
    let mut uses = vec![0usize; unique.len()];
    for i in &same {
        uses[slot[i]] += 1;
    }

    // Every stream gets a direct /Length.
    let mut direct: BTreeMap<u32, (u16, Vec<u8>)> = BTreeMap::new(); // serialised indirect objects
    let mut packable: Vec<(u32, &Obj)> = Vec::new();
    // The stream jobs' checks, with the object each is written to (in object order).
    let mut proven: Vec<(u32, &Proof)> = Vec::new();
    let mut si = 0;
    for (&n, (g, o)) in &objs {
        match o {
            Obj::Stream(s) => {
                stats.streams += 1;
                debug_assert_eq!(streams[si].0, n);
                let u = slot[&same[si]];
                uses[u] -= 1;
                let result = if uses[u] == 0 { encoded[u].take() } else { encoded[u].clone() };
                #[cfg(test)]
                let (result, u) = check_tests::writer_fault(n, result, u, &encoded);
                let (dict, data) = match result {
                    Some((d, z, change)) => {
                        if let Some(p) = &proofs[u] {
                            proven.push((n, p));
                        }
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
    let objstm_bytes: usize = raws.iter().map(|r| r.0.len()).sum();
    let structure = structure_flate(opt, objstm_bytes, input.len());
    let zs = par::map(pool, raws.len(), None, |k| flate::deflate_zlib(&raws[k].0, &structure));
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
    // Its rows count too: a file can claim object numbers far beyond what it holds.
    let structure = structure_flate(opt, objstm_bytes + rows.len() + up.len(), input.len());
    let mut both = par::map(pool, 2, None, |k| flate::deflate_zlib(if k == 0 { &rows } else { &up }, &structure));
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
    let taken = self_check(&objs, &out, &doc.trailer, crypt, &proven, pool)?;
    if dbg {
        eprintln!("self-check: {taken} of {} re-encoded streams checked by their jobs", stats.streams_reencoded);
    }
    #[cfg(test)]
    check_tests::TAKEN.with(|t| t.set(taken));
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
type Content = (Vec<u8>, Vec<Stage>);

/// Most bytes the self-check decodes a stream to.
const CHECK_LIMIT: usize = 1 << 30;

/// The content of a stream (see `Content`).
fn content_of(s: &Stream) -> Option<Content> {
    content_in(&s.dict, &s.data, CHECK_LIMIT)
}

/// The content of stream data under a dictionary's filters, each stage decoding to at most `limit` bytes (None
/// beyond it, as for damaged data).
fn content_in(dict: &Dict, data: &[u8], limit: usize) -> Option<Content> {
    let stages = chain(dict)?;
    let mut data = std::borrow::Cow::Borrowed(data);
    let mut rest = Vec::new();
    for st in stages {
        if is_general(&st.name) && rest.is_empty() {
            data = std::borrow::Cow::Owned(decode_stage(&st, &data, limit).ok()?);
            if predicted(&st) {
                data = std::borrow::Cow::Owned(super::filters::unpredict(&st, &data).ok()?);
            }
        } else {
            rest.push(st);
        }
    }
    Some((data.into_owned(), rest))
}

/// Whether the content is read with this general stage's predictor undone.
fn predicted(st: &Stage) -> bool {
    (st.name == b"FlateDecode" || st.name == b"LZWDecode")
        && st.parms.as_ref().and_then(|p| p.get(b"Predictor")).and_then(|x| x.as_int()).unwrap_or(1) > 1
}

/// A stream job's check of a re-encoded stream, for the self-check: the new data of stream `of`, decoded
/// under these filter entries, gave the stream's content back; `sha` is that data's SHA-256.
struct Proof {
    of: u32,
    sha: [u8; 32],
    filter: Option<Obj>,
    parms: Option<Obj>,
}

impl Proof {
    /// Whether output stream `s2`, written for stream `n` (`s1`), is what the job checked: `s1` is the
    /// stream it checked (or one identical to it), and the output has the same data under the same filter
    /// entries, so it decodes to the content of `s1`.
    fn holds(&self, n: u32, s1: &Stream, s2: &Stream, objs: &BTreeMap<u32, (u16, Obj)>) -> bool {
        (self.of == n || matches!(objs.get(&self.of), Some((_, Obj::Stream(s))) if **s == *s1))
            && s2.dict.get(b"Filter") == self.filter.as_ref()
            && s2.dict.get(b"DecodeParms") == self.parms.as_ref()
            && crate::pack::sha256(&s2.data) == self.sha
    }
}

/// Checks the new encoding of stream `n` (`s`, with `content`) the way the self-check would, where that
/// saves work. The check costs two SHA-256 passes over the new data (here and in the self-check); decoding
/// the stream again costs more once its filters halve the content or better (inflating then takes about 130
/// instructions per input byte, slow ones; two SHA-256 passes 128 per byte of the smaller new data, fast
/// ones). The streams it leaves, and JPEGs and fax images whose bytes change by design, are compared in full
/// by the self-check.
fn prove(n: u32, s: &Stream, content: &Content, (d, z, change): &Reencoded) -> Option<Proof> {
    let pays = content.0.len() / 2 >= s.data.len();
    if !pays || *change != Change::default() || content_in(d, z, CHECK_LIMIT).as_ref() != Some(content) {
        return None;
    }
    Some(Proof {
        of: n,
        sha: crate::pack::sha256(z),
        filter: d.get(b"Filter").cloned(),
        parms: d.get(b"DecodeParms").cloned(),
    })
}

/// The same JPEG with other Huffman tables: the same DCT stage, every other byte the same, and the same coded
/// symbols in every scan (so the same coefficients).
fn same_jpeg(a: &Content, b: &Content) -> bool {
    a.1 == b.1 && matches!(a.1.as_slice(), [st] if st.name == b"DCTDecode") && crate::jpeg::equivalent(&a.0, &b.0)
}

fn same_except_encoding(a: &Dict, b: &Dict) -> bool {
    let strip = |d: &Dict| -> Vec<(Vec<u8>, Obj)> {
        d.0.iter().filter(|(k, _)| !matches!(k.as_slice(), b"Length" | b"Filter" | b"DecodeParms")).cloned().collect()
    };
    strip(a) == strip(b)
}

/// Reads the output back and compares every reachable object with the input's. A re-encoded stream is
/// decoded and compared unless its job's check (in `proven`, by object number) holds for it. Gives the number
/// of streams taken on their job's check.
fn self_check(
    objs: &BTreeMap<u32, (u16, Obj)>,
    out: &[u8],
    trailer: &Dict,
    crypt: Option<&Crypt>,
    proven: &[(u32, &Proof)],
    pool: Option<&par::Pool>,
) -> Result<usize, String> {
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
    // Objects are compared on the pool (the biggest stored streams first), each filter stage decoding to at
    // most `CHECK_ON_POOL` bytes there; a stream that needs more is compared afterwards, one at a time, to 1 GiB
    // a stage. A comparison holds a clone of the stream's stored bytes, whole whatever its filters (and that is
    // its content when its first filter is not one we decode), and up to three decoded copies: the input's
    // content, and the output's decode with and without its predictor, whose two row buffers add up to two more
    // for a one-row image. On the pool each decoded copy is at most `CHECK_ON_POOL`; a fax image written as
    // JBIG2 has both images parsed, within the fax limits but for its trailing end-of-line codes. The first
    // difference in object order is the one reported, whatever the number of threads.
    let all: Vec<(&u32, &(u16, Obj))> = objs.iter().collect();
    let mut order: Vec<usize> = (0..all.len()).collect();
    order.sort_by_key(|&k| (std::cmp::Reverse(if let Obj::Stream(s) = &all[k].1 .1 { s.data.len() } else { 0 }), k));
    // None: the stream needs more than `limit` decoded bytes (or doesn't decode within it). Ok(true): taken on
    // its job's check, without decoding.
    let check = |k: usize, limit: usize| -> Option<Result<bool, String>> {
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
                    return Some(Ok(false));
                }
                let proof = proven.binary_search_by_key(n, |&(m, _)| m).ok().and_then(|i| proven.get(i));
                if proof.is_some_and(|(_, p)| p.holds(*n, s1, s2, objs)) {
                    return Some(Ok(true));
                }
                let full = limit == CHECK_LIMIT;
                let Some(c1) = content_in(&s1.dict, &s1.data, limit) else {
                    return full.then(|| Err(format!("self-check: stream {n} undecodable")));
                };
                let Some(c2) = content_in(&s2.dict, &s2.data, limit) else {
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
                Some(Ok(false))
            }
            (x, y) => Some(if x != y { Err(format!("self-check: object {n} differs")) } else { Ok(false) }),
        }
    };
    let on_pool = par::map(pool, all.len(), Some(&order), |k| check(k, CHECK_ON_POOL));
    let mut checked = Vec::with_capacity(all.len());
    for (k, r) in on_pool.into_iter().enumerate() {
        checked
            .push(r.or_else(|| check(k, CHECK_LIMIT)).unwrap_or_else(|| Err("self-check: stream undecodable".into())));
    }
    checked.into_iter().try_fold(0, |taken, r| r.map(|t| taken + t as usize))
}

/// Decoded bytes a stream may take, per copy, while it is compared on the thread pool (see `self_check`).
const CHECK_ON_POOL: usize = 16 << 20;

#[cfg(test)]
mod effort_tests {
    use super::*;
    use std::sync::Arc;

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
        // Never weaker than effort 0's: level 8 (weaker than 7 on images) is skipped.
        assert!((0..FAST_BELOW).all(|e| image_level(e) >= image_level(0) && image_level(e) != 8));
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
        assert!(!big_levels.contains(&11), "{big_levels:?}");
        assert!((0..STRONG_FROM).all(|e| big_levels[e as usize] == big_level(e)), "{big_levels:?}");
        assert!(big_levels.windows(2).take(16).all(|w| w[0] <= w[1]), "{big_levels:?}");
        assert_eq!(big_levels[STRONG_FROM as usize], flate::fast::LEVELS);
        assert_eq!(big_levels[16..], [flate::fast::LEVELS, 0, 0, 0]);
    }

    #[test]
    fn the_bridge_gives_the_object_streams_the_strong_encoder_when_they_fit() {
        let fast = |e: u8, raw: usize, input_len: usize| structure_flate(&Options::effort(e), raw, input_len).fast;
        // Below STRONG_FROM and from 20 on: the effort's own settings (the fast encoder below 20).
        assert_eq!(fast(0, 0, 1_000_000), Options::effort(0).flate.fast);
        assert_eq!(fast(9, 100, 1_000_000), Options::effort(9).flate.fast);
        assert!(fast(0, 0, 1_000_000) > 0);
        assert!(structure_flate(&Options::effort(20), 100, 1_000_000).same_output(&Options::effort(20).flate));
        // On the bridge: the strong encoder (fast 0) while the bytes fit in its budget, which doubles every point.
        assert_eq!(fast(10, 62_500, 1_000_000), 0);
        assert_eq!(fast(10, 62_501, 1_000_000), Options::effort(10).flate.fast);
        assert_eq!(fast(19, 32_000_000, 1_000_000), 0);
        assert_eq!(fast(19, 32_000_001, 1_000_000), Options::effort(19).flate.fast);
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
        assert!(reencode(&s, &strong(15), &resolve, &mut None).is_none());
        assert!(reencode(&s, &Options::effort(0), &resolve, &mut None).is_none());
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
        assert!(reencode(&s, &strong(15), &resolve, &mut None).is_none());
        let legacy_quick = Options { flate: flate::Options::quick(), effort: 0, ..Options::effort(50) };
        assert!(reencode(&s, &legacy_quick, &resolve, &mut None).is_some());
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
            let (d, z, _) = reencode(&s, &opt, &resolve, &mut None).expect("stored without a filter");
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
                let (d, z, _) = reencode(s, &opt, &resolve, &mut None).expect("Flate makes it smaller");
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
                assert_eq!(self_check(&objs, &out_a.data, &doc.trailer, None, &[], Some(&pool)), Ok(0));
                let wrong = self_check(&objs, &out_b.data, &doc.trailer, None, &[], Some(&pool));
                assert_eq!(
                    wrong,
                    Err("self-check: content of stream 4 differs".to_string()),
                    "{size}, {threads} threads"
                );
            }
        }
    }

    #[test]
    fn load_reachable_lets_the_object_streams_go() {
        // Our output keeps its objects in object streams. Read as the self-check reads it (each stream kept
        // once decoded), nothing is held once everything reachable is read.
        let annots: Vec<String> = (4..54).map(|n| format!("{n} 0 R")).collect();
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [{}] >>", annots.join(" ")),
        ];
        objs.extend((4..54).map(|n| format!("<< /Type /Annot /Subtype /Link /Rect [0 0 {n} {n}] /Border [0 0 0] >>")));
        let mut input = b"%PDF-1.4\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(input.len());
            input.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            input.extend_from_slice(o.as_bytes());
            input.extend_from_slice(b"\nendobj\n");
        }
        let x = input.len();
        input.extend_from_slice(b"xref\n0 54\n0000000000 65535 f \n");
        for off in offs {
            input.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        input.extend_from_slice(format!("trailer\n<< /Size 54 /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes());
        let out = optimize(&input, &Options::effort(0));
        assert!(matches!(&out.outcome, Outcome::Smaller(s) if s.objstms > 0), "{:?}", out.outcome);
        let doc = xref::open(&out.data).unwrap();
        assert_eq!(load_reachable(&doc).unwrap().len(), 53);
        let s = doc.objstm_stats();
        assert!(s.peak_held > 0 && s.held == 0, "{s:?}");
    }

    #[test]
    fn best_of_takes_the_first_of_equals() {
        assert_eq!(best_of(&[7, 3, 3, 9, 3]), Some(1));
        assert_eq!(best_of(&[4]), Some(0));
        assert_eq!(best_of(&[]), None);
        // The same scores from par::map, where later candidates finish first.
        let scores = [7u64, 3, 3, 9, 3];
        for threads in [1, 2, 4, 8] {
            for seed in 1..=4 {
                let pool = par::Pool::new(threads);
                let got = par::jitter::with(seed, || {
                    par::map(Some(&pool), scores.len(), None, |k| {
                        std::thread::sleep(std::time::Duration::from_millis(5 - k as u64));
                        scores[k]
                    })
                });
                assert_eq!(best_of(&got), Some(1), "{threads} threads, seed {seed}");
            }
        }
    }

    /// `repredict` as it was before the candidates were ranked side by side: one after another, keeping the
    /// best one's bytes. The reference for the test below.
    fn repredict_one_by_one(
        s: &Stream,
        data: &[u8],
        flate_len: usize,
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
        let rank =
            flate::Options { fast: if opt.flate.fast > 0 { image_level(0) } else { 0 }, ..flate::Options::quick() };
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
        // The best is `data` itself (the plain samples without a predictor before, or the same predictor bytes):
        // reencode() has already tried our Flate over it.
        if bytes == data {
            return None;
        }
        let z = flate::deflate_zlib(&bytes, &opt.flate);
        // Without a predictor before, `data` is the plain samples: a predictor that saves fewer bytes than its
        // parameters take is not kept.
        if parms.is_some() && carried.is_none() && z.len() + dict_len(&parms) >= flate_len + dict_len(&None) {
            return None;
        }
        Some((dict_with(&parms), z))
    }

    #[test]
    fn repredict_matches_the_one_by_one_loop() {
        let resolve = |o: &Obj| Some(o.clone());
        let mut r = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        let image = |w: usize, bpc: i64, cs: &[u8], px: Vec<u8>| {
            let mut d = Dict::default();
            d.set(b"Subtype", Obj::Name(b"Image".to_vec()));
            d.set(b"Width", Obj::Int(w as i64));
            d.set(b"BitsPerComponent", Obj::Int(bpc));
            d.set(b"ColorSpace", Obj::Name(cs.to_vec()));
            Stream { dict: d, data: px }
        };
        let png_parms = |colors: i64, bpc: i64, w: usize| {
            let mut p = Dict::default();
            p.set(b"Predictor", Obj::Int(15));
            p.set(b"Colors", Obj::Int(colors));
            p.set(b"BitsPerComponent", Obj::Int(bpc));
            p.set(b"Columns", Obj::Int(w as i64));
            p
        };
        // Under 64 KiB, ranked whole at every effort; over it, ranked on a sample of rows at effort 0.
        let noise: Vec<u8> = (0..16 * 24 * 3).map(|_| next() as u8).collect();
        let gradient = |w: usize, h: usize| -> Vec<u8> { (0..w * h).map(|i| (i % w + i / w * 2) as u8).collect() };
        let ramps: Vec<u8> = (0..24)
            .flat_map(|_| {
                let o = next() as usize;
                (0..64 * 3).map(move |i| (o + i / 3 * (1 + i % 3)) as u8)
            })
            .collect();
        let rows: Vec<Vec<u8>> = (0..4).map(|_| (0..256).map(|_| (next() % 8) as u8 * 30).collect()).collect();
        let repeating: Vec<u8> = (0..64).flat_map(|y| rows[[0, 1, 1, 2, 3, 3, 3, 0][y % 8]].clone()).collect();
        let sixteen: Vec<u8> = (0..24 * 24 * 3)
            .flat_map(|i| ((i % 72 * 800 + i / 72 * 180) as u16 ^ (next() % 4) as u16).to_be_bytes())
            .collect();
        let one_bit: Vec<u8> =
            (0..256 * 32).map(|i| if (i / 32 / 7 + i % 32 / 3) % 3 == 0 { 0xF0 } else { 0x0F }).collect();
        // One row: Sub and Paeth filter it alike but for the filter type, so the best scores tie.
        let one_row: Vec<u8> = (0..1200).map(|i| (i * 3) as u8).collect();
        let mut cases: Vec<(&str, Stream, Option<Dict>)> = vec![
            ("noise", image(16, 8, b"DeviceRGB", noise.clone()), None),
            ("one row", image(1200, 8, b"DeviceGray", one_row), None),
            ("gradient", image(128, 8, b"DeviceGray", gradient(128, 128)), None),
            ("big gradient", image(272, 8, b"DeviceGray", gradient(272, 248)), None),
            ("ramps", image(64, 8, b"DeviceRGB", ramps), None),
            ("repeating rows", image(256, 8, b"DeviceGray", repeating), None),
            ("16-bit", image(24, 16, b"DeviceRGB", sixteen), None),
            ("1-bit", image(256, 1, b"DeviceGray", one_bit), None),
        ];
        // Stored with a predictor: Up (the ranking's pick for this image, so nothing changes), and MinSum.
        for how in [images::Choice::Fixed(2), images::Choice::MinSum] {
            let data = images::png_filter(&gradient(128, 128), 128, 1, how);
            cases.push(("gradient, predicted", image(128, 8, b"DeviceGray", data), Some(png_parms(1, 8, 128))));
        }
        // Noise stored with Sub: no predictor wins, by less than the size of the predictor parameters.
        let data = images::png_filter(&noise, 48, 3, images::Choice::Fixed(1));
        cases.push(("noise, predicted", image(16, 8, b"DeviceRGB", data), Some(png_parms(3, 8, 16))));
        let (mut some, mut predicted) = (0, 0);
        for (name, s, carried) in &cases {
            // At 15 as on the bridge: the fast encoder's image level, so the fast encoder ranks the candidates.
            let efforts = [(0u8, 0u8), (15, image_level(15)), (50, 0), (100, 0)];
            for (effort, fast) in efforts {
                // The quick settings for the final encode too: only the ranking depends on the effort here.
                let opt = |pool: &Arc<par::Pool>| Options {
                    flate: flate::Options {
                        shared: Some(flate::deflate::Shared::with_pool(pool.clone(), true)),
                        fast,
                        ..flate::Options::quick()
                    },
                    ..Options::effort(effort)
                };
                let one = opt(&par::Pool::new(1));
                let flate_len = flate::deflate_zlib(&s.data, &one.flate).len();
                let want = repredict_one_by_one(s, &s.data, flate_len, carried.as_ref(), &one, &resolve);
                some += want.is_some() as usize;
                predicted += want.as_ref().is_some_and(|(d, _)| d.has(b"DecodeParms")) as usize;
                for threads in [1, 2, 4, 8] {
                    for seed in if threads == 1 { 0..=0 } else { 1..=4 } {
                        let o = opt(&par::Pool::new(threads));
                        let got = par::jitter::with(seed, || {
                            repredict(s, &s.data, flate_len, carried.as_ref(), &o, &resolve)
                        });
                        assert!(got == want, "{name}, effort {effort}, {threads} threads, seed {seed}");
                    }
                }
            }
        }
        // Both outcomes were tried: a new predictor, and none (or nothing better than what was there).
        assert!(predicted >= 6 && some < cases.len() * 4, "{some} results, {predicted} with a predictor");
    }

    #[test]
    fn ranking_keeps_nothing_in_the_store() {
        // The candidates are ranked on the file's pool without its store: only the winner's encode goes
        // through the store. (Kept, a ranking encode with the same settings would answer the winner's.)
        let resolve = |o: &Obj| Some(o.clone());
        let mut d = Dict::default();
        d.set(b"Subtype", Obj::Name(b"Image".to_vec()));
        d.set(b"Width", Obj::Int(128));
        d.set(b"BitsPerComponent", Obj::Int(8));
        d.set(b"ColorSpace", Obj::Name(b"DeviceGray".to_vec()));
        let s = Stream { dict: d, data: (0..128 * 128).map(|i| (i % 128 + i / 128 * 2) as u8).collect() };
        let flate_len = flate::deflate_zlib(&s.data, &flate::Options::quick()).len();
        for threads in [1, 4] {
            let store = flate::deflate::Shared::with_store(par::Pool::new(threads), 1 << 20);
            let opt = Options {
                flate: flate::Options { shared: Some(store.clone()), ..flate::Options::quick() },
                ..Options::effort(100)
            };
            let got = repredict(&s, &s.data, flate_len, None, &opt, &resolve);
            assert!(got.is_some_and(|(d, _)| d.has(b"DecodeParms")), "{threads} threads");
            let st = store.stats();
            assert_eq!((st.hits, st.computed, st.kept), (0, 1, 1), "{threads} threads");
        }
    }

    #[test]
    fn identical_streams_are_found_by_value() {
        let a = flate_stream(b"same bytes", &[]);
        let b = flate_stream(b"same bytes", &[]);
        let c = flate_stream(b"same bytes", &[(b"Length1", Obj::Int(3))]);
        let d = flate_stream(b"other bytes", &[]);
        let streams: Vec<(u32, &Stream)> = vec![(1, &a), (2, &c), (3, &b), (4, &d), (5, &a)];
        let (same, unique, shares) = identical_streams(&streams);
        assert_eq!(same, vec![0, 1, 0, 3, 0]);
        assert_eq!(unique, vec![0, 1, 3]);
        // a and c have the same data and filters under other dictionaries.
        assert_eq!(shares, vec![true, true, false]);
    }

    #[test]
    fn identical_streams_match_the_pairwise_reference() {
        // Two 16-byte inputs with the same hash64: the second block cancels the difference of the first.
        let m = 0x9E37_79B9_7F4A_7C15u64;
        let step = |h: u64, v: u64| (h ^ v).wrapping_mul(m).rotate_left(29);
        let h0 = 16u64.wrapping_mul(m) ^ 0x2545_F491_4F6C_DD1D;
        let (v1, w1, v2) = (1u64, 2u64, 7u64);
        let w2 = v2 ^ step(h0, v1) ^ step(h0, w1);
        let x = [v1.to_le_bytes(), v2.to_le_bytes()].concat();
        let y = [w1.to_le_bytes(), w2.to_le_bytes()].concat();
        assert!(x != y && par::hash64(&x) == par::hash64(&y), "a planted collision");
        let data = [x, y, b"other data, 16 b".to_vec()];
        // The O(n^2) answer: the first equal stream, the firsts, and whether another first has the same data,
        // /Filter and /DecodeParms.
        let reference = |streams: &[(u32, &Stream)]| {
            let same: Vec<usize> =
                (0..streams.len()).map(|i| (0..=i).find(|&j| streams[j].1 == streams[i].1).unwrap()).collect();
            let unique: Vec<usize> = (0..streams.len()).filter(|&i| same[i] == i).collect();
            let input =
                |s: &Stream| (s.data.clone(), s.dict.get(b"Filter").cloned(), s.dict.get(b"DecodeParms").cloned());
            let shares: Vec<bool> = unique
                .iter()
                .map(|&i| unique.iter().any(|&j| j != i && input(streams[j].1) == input(streams[i].1)))
                .collect();
            (same, unique, shares)
        };
        let mut r = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |k: u64| {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            (r % k) as usize
        };
        let (mut merged, mut shared) = (0, 0);
        for _ in 0..300 {
            let n = 1 + next(40);
            let set: Vec<Stream> = (0..n)
                .map(|_| {
                    let mut d = Dict::default();
                    let length = [Some(Obj::Int(16)), Some(Obj::Ref(9, 0)), None][next(3)].clone();
                    let filter = [None, Some(Obj::Name(b"FlateDecode".to_vec()))][next(2)].clone();
                    let mut p = Dict::default();
                    p.set(b"Predictor", Obj::Int(12));
                    let parms = [None, Some(Obj::Dict(p))][next(2)].clone();
                    // The same entries in another order make another dictionary.
                    let length_first = next(2) == 0;
                    if let (Some(l), true) = (&length, length_first) {
                        d.set(b"Length", l.clone());
                    }
                    if let Some(f) = filter {
                        d.set(b"Filter", f);
                    }
                    if let Some(p) = parms {
                        d.set(b"DecodeParms", p);
                    }
                    if let (Some(l), false) = (length, length_first) {
                        d.set(b"Length", l);
                    }
                    Stream { dict: d, data: data[next(3)].clone() }
                })
                .collect();
            let streams: Vec<(u32, &Stream)> = set.iter().enumerate().map(|(k, s)| (k as u32 + 1, s)).collect();
            let got = identical_streams(&streams);
            assert_eq!(got, reference(&streams), "{streams:?}");
            // Never merged across the planted collision.
            assert!(got.0.iter().enumerate().all(|(i, &j)| streams[j].1.data == streams[i].1.data));
            merged += n - got.1.len();
            shared += got.2.iter().filter(|&&x| x).count();
        }
        assert!(merged > 1000 && shared > 1000, "{merged} merged, {shared} sharing");
    }

    #[test]
    fn headers_are_found_as_a_search_for_each_finds_them() {
        let planted: [&[u8]; 14] = [
            b"112 0 obj",
            b"5 010 obj",
            b"x 0 obj",
            b"0 0 obj",
            b"00 1 obj",
            b"98765432101234 7 obj",
            b"4294967295 65535 obj",
            b"4294967296 65536 obj",
            b"12  3 obj",
            b"7 3obj",
            b"8 123456 obj",
            b"5\n0 obj",
            b"6x10 obj",
            b"1 0 obj",
        ];
        // Soups of these, so that near-headers of every kind come up.
        let tokens: [&[u8]; 12] = [b"0", b"1", b"2", b"5", b"7", b"9", b"0", b" ", b" ", b" obj", b"x", b"\n"];
        let mut r = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |k: usize| {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            (r % k as u64) as usize
        };
        let (mut found, mut asked) = (0, 0);
        for round in 0..600 {
            let mut soup: Vec<u8> = (0..next(60)).flat_map(|_| tokens[next(tokens.len())].iter().copied()).collect();
            for _ in 0..next(4) {
                let at = next(soup.len() + 1);
                soup.splice(at..at, planted[next(planted.len())].iter().copied());
            }
            // Headers at the very start and the very end too.
            if round % 3 == 0 {
                soup.splice(0..0, planted[next(planted.len())].iter().copied());
            }
            if round % 3 == 1 {
                soup.extend_from_slice(planted[next(planted.len())]);
            }
            // Every (n, g) the soup could hold: numbers read from every "digits space digits" in it, leading
            // zeros and all, plus small ones.
            let mut cands: BTreeSet<(u32, u16)> = (0..=12).flat_map(|n| (0..=12).map(move |g| (n, g))).collect();
            for a in 0..soup.len() {
                for b in a + 1..=soup.len().min(a + 18) {
                    let Some(sp) = soup[a..b].iter().position(|&c| c == b' ') else { continue };
                    let (x, y) = (&soup[a..a + sp], &soup[a + sp + 1..b]);
                    let read = |t: &[u8]| -> Option<u64> {
                        if t.is_empty() || !t.iter().all(u8::is_ascii_digit) {
                            return None;
                        }
                        std::str::from_utf8(t).ok()?.parse().ok()
                    };
                    if let (Some(n), Some(g)) = (read(x), read(y)) {
                        if let (Ok(n), Ok(g)) = (u32::try_from(n), u16::try_from(g)) {
                            cands.insert((n, g));
                        }
                    }
                }
            }
            let mut any = false;
            for &(n, g) in &cands {
                let want = contains(&soup, format!("{n} {g} obj").as_bytes());
                assert_eq!(
                    holds_header(&soup, &HashSet::from([(n, g)])),
                    want,
                    "{n} {g} in {:?}",
                    String::from_utf8_lossy(&soup)
                );
                any |= want;
                found += want as usize;
                asked += 1;
            }
            assert_eq!(holds_header(&soup, &cands.iter().copied().collect()), any);
        }
        assert!(found > 1000 && asked > found * 3, "{found} found of {asked}");
    }
}

/// The self-check against wrong streams planted on purpose: new data changed before its stream job checks it
/// (an encoder fault), or after (a writer fault).
#[cfg(test)]
pub(super) mod check_tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(super) enum Fault {
        /// One byte of the new data changed before the job checks it (it no longer decodes).
        Encoder,
        /// One byte changed under the Flate layer before the job checks it: it decodes, to other content.
        EncoderContent,
        /// One byte of the new data changed after the job checked it.
        Writer,
        /// The next stream's new data written in its place, under its own dictionary.
        Swap,
        /// The new dictionary written with other filter parameters.
        Parms,
        /// The new dictionary written without its filter.
        Filter,
    }

    thread_local! {
        /// The fault planted in one object by rewrites on this thread (with one thread, the stream jobs run on
        /// it too).
        static FAULT: Cell<Option<(Fault, u32)>> = const { Cell::new(None) };
        /// Streams the last self-check on this thread took on their jobs' checks.
        pub(super) static TAKEN: Cell<usize> = const { Cell::new(usize::MAX) };
    }

    fn flip(z: &mut [u8]) {
        let i = z.len() / 2;
        z[i] ^= 0x10;
    }

    pub(super) fn encoder_fault(n: u32, r: Option<Reencoded>) -> Option<Reencoded> {
        let (d, mut z, change) = r?;
        match FAULT.get() {
            Some((Fault::Encoder, at)) if at == n => flip(&mut z),
            Some((Fault::EncoderContent, at)) if at == n => {
                let mut raw = flate::zlib::unwrap(&z, usize::MAX).expect("Flate data").data;
                flip(&mut raw);
                z = flate::deflate_zlib(&raw, &flate::Options::quick());
            }
            _ => {}
        }
        Some((d, z, change))
    }

    pub(super) fn writer_fault(
        n: u32,
        r: Option<Reencoded>,
        u: usize,
        encoded: &[Option<Reencoded>],
    ) -> (Option<Reencoded>, usize) {
        let Some((mut d, mut z, change)) = r else { return (None, u) };
        match FAULT.get() {
            Some((Fault::Writer, at)) if at == n => flip(&mut z),
            Some((Fault::Swap, at)) if at == n => {
                let next = encoded[u + 1].as_ref().expect("the next stream is re-encoded");
                return (Some((d, next.1.clone(), change)), u + 1);
            }
            Some((Fault::Parms, at)) if at == n => {
                let mut p = Dict::default();
                p.set(b"Predictor", Obj::Int(2));
                p.set(b"Columns", Obj::Int(4));
                d.set(b"DecodeParms", Obj::Dict(p));
            }
            Some((Fault::Filter, at)) if at == n => {
                d.remove(b"Filter");
            }
            _ => {}
        }
        (Some((d, z, change)), u)
    }

    /// The rewrite of `pdf` at effort 0 with `fault` planted, and the streams its self-check took on their
    /// jobs' checks.
    fn rewrite_with(pdf: &[u8], threads: usize, fault: Option<(Fault, u32)>) -> (Optimized, usize) {
        FAULT.set(fault);
        TAKEN.set(usize::MAX);
        let r = optimize(pdf, &Options { threads, ..Options::effort(0) });
        FAULT.set(None);
        (r, TAKEN.get())
    }

    /// Zlib data in stored blocks (no compression), so every stream is re-encoded.
    fn stored(data: &[u8]) -> Vec<u8> {
        let mut z = vec![0x78, 0x01];
        let mut blocks = data.chunks(65535).peekable();
        if blocks.peek().is_none() {
            z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
        }
        while let Some(b) = blocks.next() {
            z.push(blocks.peek().is_none() as u8);
            z.extend_from_slice(&(b.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
            z.extend_from_slice(b);
        }
        z.extend_from_slice(&flate::zlib::adler32(data).to_be_bytes());
        z
    }

    /// A page drawn by three content streams (4, 5, and 6 identical to 4) and an image (7), each compressed
    /// twice over with the fast encoder's quickest level: the rewrite makes them smaller, and decoding them
    /// costs more than the jobs' checks.
    fn test_pdf() -> Vec<u8> {
        let stream = |dict: &str, decoded: &[u8]| {
            let quickest = flate::Options { fast: 1, ..flate::Options::quick() };
            let data = flate::deflate_zlib(&flate::deflate_zlib(decoded, &quickest), &quickest);
            let mut o = format!("<< {dict} /Filter [/FlateDecode /FlateDecode] /Length {} >>\nstream\n", data.len())
                .into_bytes();
            o.extend_from_slice(&data);
            o.extend_from_slice(b"\nendstream");
            o
        };
        let text = |k: usize| -> Vec<u8> {
            let mut r = k as u64;
            (0..300)
                .flat_map(|i| {
                    r = r.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    let (x, w) = ((r >> 40) % 500, ["fox", "dog", "jumps", "over", "lazy"][(r >> 20) as usize % 5]);
                    format!("BT /F1 {} Tf {x} {} Td (Line {i}: the {w}) Tj ET\n", 9 + k, i % 700).into_bytes()
                })
                .collect()
        };
        let px: Vec<u8> = (0..160 * 120).map(|i| ((i % 160) + (i / 160) * 2) as u8).collect();
        let objs: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 5 0 R 6 0 R] /Resources << /XObject << /A 7 0 R >> >> >>".to_vec(),
            stream("", &text(1)),
            stream("", &text(2)),
            stream("", &text(1)),
            stream("/Type /XObject /Subtype /Image /Width 160 /Height 120 /ColorSpace /DeviceGray /BitsPerComponent 8", &px),
        ];
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
    fn the_content_from_reencode_is_what_the_self_check_decodes() {
        let text: Vec<u8> = (0..400).flat_map(|i| format!("BT ({i} of 400) Tj ET\n").into_bytes()).collect();
        let px: Vec<u8> = (0..160 * 120).map(|i| ((i % 160) * 3 + (i / 160)) as u8).collect();
        let tiff: Vec<u8> =
            px.iter().enumerate().map(|(i, &x)| if i % 160 == 0 { x } else { x.wrapping_sub(px[i - 1]) }).collect();
        // LZW with 9-bit codes only: a clear code every 200 bytes, each byte as its own code.
        let lzw = |d: &[u8]| {
            let codes: Vec<u16> = d
                .chunks(200)
                .flat_map(|c| std::iter::once(256).chain(c.iter().map(|&b| b as u16)))
                .chain(std::iter::once(257))
                .collect();
            let mut out = vec![0u8; (codes.len() * 9).div_ceil(8)];
            for (k, &c) in codes.iter().enumerate() {
                for b in 0..9 {
                    if c >> (8 - b) & 1 == 1 {
                        out[(k * 9 + b) / 8] |= 0x80 >> ((k * 9 + b) % 8);
                    }
                }
            }
            out
        };
        let hex: Vec<u8> = stored(&text).iter().flat_map(|b| format!("{b:02X}").into_bytes()).chain(*b">").collect();
        let image = "/Subtype /Image /Width 160 /Height 120 /ColorSpace /DeviceGray /BitsPerComponent 8";
        let parms = |p: u8, bpc: u8| {
            format!("/DecodeParms << /Predictor {p} /Colors 1 /BitsPerComponent {bpc} /Columns 160 >>")
        };
        // Each with whether its content is kept: only when the filters decompress (Flate, LZW).
        let cases: Vec<(String, Vec<u8>, bool)> = vec![
            ("/Filter /FlateDecode".into(), stored(&text), true),
            (
                format!("{image} /Filter /FlateDecode {}", parms(15, 8)),
                stored(&images::png_filter(&px, 160, 1, images::Choice::Fixed(2))),
                true,
            ),
            (format!("{image} /Filter /FlateDecode {}", parms(2, 8)), stored(&tiff), true),
            // Predictor 2 is undone only for 8-bit samples: the content is unknown, and the check fails.
            (format!("/Filter /FlateDecode {}", parms(2, 4)), stored(&tiff), true),
            ("/Filter [/ASCIIHexDecode /FlateDecode]".into(), hex, true),
            ("/Filter /LZWDecode".into(), lzw(&text), true),
            (format!("{image} /Filter [/FlateDecode /DCTDecode]"), stored(&text), true),
            (
                "/Filter /RunLengthDecode".into(),
                text.chunks(128).flat_map(|c| [&[c.len() as u8 - 1][..], c].concat()).chain([128]).collect(),
                false,
            ),
            (image.into(), px.clone(), false),
        ];
        let resolve = |o: &Obj| Some(o.clone());
        let mut decoded = 0;
        for (dict, data, kept) in cases {
            let dict = match super::super::parse::Lexer::new(format!("<< {dict} >>").as_bytes(), 0).parse_obj() {
                Ok(Obj::Dict(d)) => d,
                x => panic!("{dict}: {x:?}"),
            };
            let s = Stream { dict, data };
            for (max_stream, known) in [(Options::default().max_stream, kept), (CHECK_LIMIT + 1, false)] {
                let mut content = None;
                let r = reencode(&s, &Options { max_stream, ..Options::effort(0) }, &resolve, &mut content);
                assert!(r.is_some(), "{:?}", s.dict);
                assert_eq!(content, if known { content_of(&s) } else { None }, "{:?}", s.dict);
                decoded += content.is_some() as usize;
            }
        }
        assert_eq!(decoded, 6);
    }

    fn caught(r: &Optimized) -> bool {
        matches!(&r.outcome, Outcome::Untouched(why) if why.starts_with("self-check:"))
    }

    #[test]
    fn a_correct_rewrite_is_checked_by_its_stream_jobs() {
        let pdf = test_pdf();
        let (one, taken) = rewrite_with(&pdf, 1, None);
        let Outcome::Smaller(stats) = &one.outcome else { panic!("{:?}", one.outcome) };
        assert_eq!(stats.streams_reencoded, 4);
        // Every stream, the one identical to another too, is taken on its job's check.
        assert_eq!(taken, 4);
        let (four, taken) = rewrite_with(&pdf, 4, None);
        assert!(four.data == one.data && taken == 4);
    }

    #[test]
    fn a_wrong_stream_is_caught() {
        let pdf = test_pdf();
        let (clean, _) = rewrite_with(&pdf, 1, None);
        assert!(matches!(clean.outcome, Outcome::Smaller(_)));
        // Stream 6 is encoded once with 4, by 4's job; the next stream encoded after 6 or 7 has been written.
        for (fault, objects) in [
            (Fault::Encoder, &[4, 5, 7][..]),
            (Fault::EncoderContent, &[4, 5, 7]),
            (Fault::Writer, &[4, 5, 6, 7]),
            (Fault::Swap, &[4, 5]),
            (Fault::Parms, &[4, 5, 6, 7]),
            (Fault::Filter, &[4, 5, 6, 7]),
        ] {
            for &n in objects {
                let (r, _) = rewrite_with(&pdf, 1, Some((fault, n)));
                assert!(caught(&r), "{fault:?} in stream {n}: {:?}", r.outcome);
                assert!(r.data == pdf, "{fault:?} in stream {n}");
            }
        }
    }

    #[test]
    fn an_encrypted_output_is_checked_on_its_decrypted_data() {
        for (name, pdf) in [
            ("r4_aes_128", &include_bytes!("../../tests/data/enc/r4_aes_128.pdf")[..]),
            ("r3_rc4_128", &include_bytes!("../../tests/data/enc/r3_rc4_128.pdf")[..]),
        ] {
            let (clean, taken) = rewrite_with(pdf, 1, None);
            let Outcome::Smaller(stats) = &clean.outcome else { panic!("{name}: {:?}", clean.outcome) };
            assert!(!stats.encryption.is_empty() && taken > 0, "{name}: {taken}");
            // A byte changed in any stream as it is written is caught.
            let doc = xref::open(pdf).unwrap();
            let streams: Vec<u32> = load_reachable(&doc)
                .unwrap()
                .into_iter()
                .filter(|(_, (_, o))| matches!(o, Obj::Stream(_)))
                .map(|(n, _)| n)
                .collect();
            let mut found = 0;
            for n in streams {
                let (r, _) = rewrite_with(pdf, 1, Some((Fault::Writer, n)));
                if caught(&r) {
                    found += 1;
                } else {
                    // A stream kept as it was: nothing was changed.
                    assert!(r.data == clean.data, "{name}: stream {n}: {:?}", r.outcome);
                }
            }
            assert_eq!(found, stats.streams_reencoded, "{name}");
        }
    }
}

/// Stream jobs: what each is given, and how many run at once.
#[cfg(test)]
pub(super) mod job_tests {
    use super::*;
    use crate::fax::ccitt;
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    /// Stream jobs running at once in one rewrite, counted around `run_job`.
    #[derive(Default)]
    pub(super) struct Running {
        now: AtomicUsize,
        most: AtomicUsize,
    }

    /// One job running (until dropped).
    pub(super) struct In<'r>(&'r Running);

    impl Running {
        pub(super) fn enter(&self) -> In<'_> {
            let now = self.now.fetch_add(1, SeqCst) + 1;
            self.most.fetch_max(now, SeqCst);
            In(self)
        }
        pub(super) fn most(&self) -> usize {
            self.most.load(SeqCst)
        }
    }

    impl Drop for In<'_> {
        fn drop(&mut self) {
            self.0.now.fetch_sub(1, SeqCst);
        }
    }

    thread_local! {
        /// The most stream jobs that ran at once in the last rewrite on this thread.
        pub(super) static MOST_JOBS: Cell<usize> = const { Cell::new(0) };
        /// What the store of the last rewrite on this thread did and held.
        pub(super) static STORE: Cell<flate::deflate::StoreStats> = Cell::new(Default::default());
    }

    fn parse(text: &str) -> Obj {
        super::super::parse::Lexer::new(text.as_bytes(), 0).parse_obj().unwrap()
    }

    fn dict(text: &str) -> Dict {
        match parse(text) {
            Obj::Dict(d) => d,
            x => panic!("{text}: {x:?}"),
        }
    }

    /// An image of `w` x `h` samples with `colors` components, its dictionary entries in `entries`.
    fn image(entries: &str, w: usize, h: usize, colors: usize) -> Obj {
        let px: Vec<u8> = (0..w * h * colors).map(|i| (i % 97 + i / (w * colors) * 3) as u8).collect();
        let d = dict(&format!("<< /Subtype /Image /BitsPerComponent 8 /Filter /FlateDecode {entries} >>"));
        Obj::Stream(Box::new(Stream { dict: d, data: flate::deflate_zlib(&px, &flate::Options::quick()) }))
    }

    /// A Group 4 fax image of `w` x `h`: rows of letter-sized blobs on white, like a scan.
    fn fax(entries: &str, w: usize, h: usize) -> Obj {
        let mut r = 0x1234_5678u64;
        let mut next = move |k: u64| {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            (r % k) as usize
        };
        let mut px = vec![vec![0u8; w]; h];
        for y in (10..h.saturating_sub(14)).step_by(18) {
            for x in (8..w.saturating_sub(9)).step_by(9) {
                let (bw, bh) = (3 + next(5), 6 + next(6));
                for row in px.iter_mut().skip(y).take(bh) {
                    for p in row.iter_mut().skip(x).take(bw) {
                        *p = (next(9) != 0) as u8;
                    }
                }
            }
        }
        let mut img = ccitt::Image::new(w);
        let mut ch = Vec::new();
        for row in &px {
            ccitt::row_changes(row, &mut ch);
            img.push_row(&ch);
        }
        let gaps = vec![ccitt::Gap { fill: 0, eol: false, tag: false }; h];
        let end = vec![ccitt::Gap { fill: 0, eol: true, tag: false }; 2];
        let layout = ccitt::Layout {
            scheme: ccitt::Scheme::G4,
            rule: ccitt::Rule::Standard,
            gaps,
            end,
            pad: 0,
            trailing: Vec::new(),
        };
        let d = dict(&format!(
            "<< /Subtype /Image /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode \
             /DecodeParms << /K -1 /Columns {w} >> {entries} >>"
        ));
        Obj::Stream(Box::new(Stream { dict: d, data: ccitt::encode(&img, &layout) }))
    }

    /// Object tables to look things up in: images that look up their Width, Height and colour space in every
    /// way the encoders allow (references to names, arrays, ICC profiles, a non-stream profile, missing
    /// objects, chains of references), and every PDF among the test files.
    fn tables() -> Vec<BTreeMap<u32, (u16, Obj)>> {
        let mut made: Vec<(u32, Obj)> = vec![
            (1, Obj::Int(32)),
            (2, Obj::Int(24)),
            (3, parse("/DeviceRGB")),
            (4, parse("[/ICCBased 5 0 R]")),
            (5, Obj::Stream(Box::new(Stream { dict: dict("<< /N 3 >>"), data: vec![7; 300] }))),
            (6, parse("<< /N 3 >>")),
            (7, Obj::Ref(3, 0)),
            (8, Obj::Ref(2, 0)),
            (9, parse("[/ICCBased 6 0 R]")),
            (10, Obj::Int(200)),
            (11, Obj::Int(120)),
            (12, Obj::Ref(11, 0)),
            (13, parse("[/Indexed 3 0 R 255 <00>]")),
            (14, parse("[/ICCBased 7 0 R]")),
        ];
        for (k, (entries, colors)) in [
            ("/Width 32 /Height 2 0 R /ColorSpace 3 0 R", 3),
            ("/Width 32 /Height 24 /ColorSpace 4 0 R", 3),
            ("/Width 32 /Height 24 /ColorSpace [/ICCBased 5 0 R]", 3),
            ("/Width 32 /Height 24 /ColorSpace [/ICCBased 6 0 R]", 3),
            ("/Width 32 /Height 98 0 R /ColorSpace 99 0 R", 3),
            ("/Width 32 /Height 24 /ColorSpace 7 0 R", 3),
            ("/Width 32 /Height 8 0 R /ColorSpace /DeviceGray", 1),
            ("/Width 1 0 R /Height 24 /ColorSpace /DeviceGray", 1),
            ("/Width 32 /Height 24 /ColorSpace [/ICCBased 99 0 R]", 3),
            ("/Width 32 /Height 24 /ColorSpace 13 0 R", 1),
            ("/Width 32 /Height 24 /ColorSpace 14 0 R", 3),
            ("/Width 32 /Height 24 /ColorSpace [/Indexed 3 0 R 255 <00>]", 1),
        ]
        .into_iter()
        .enumerate()
        {
            made.push((20 + k as u32, image(entries, 32, 24, colors)));
        }
        made.push((40, fax("/Width 10 0 R /Height 11 0 R", 200, 120)));
        made.push((41, fax("/Width 200 /Height 12 0 R", 200, 120)));
        made.push((42, fax("/Width 10 0 R /Height 97 0 R", 200, 120)));
        made.push((43, fax("/Width 200 /Height 120", 200, 120)));
        let mut out = vec![made.into_iter().map(|(n, o)| (n, (0, o))).collect()];
        for dir in ["tests/data", "tests/data/enc"] {
            let mut files: Vec<_> = std::fs::read_dir(format!("{}/{dir}", env!("CARGO_MANIFEST_DIR")))
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
                .collect();
            files.sort();
            for f in files {
                let data = std::fs::read(&f).unwrap();
                if let Some(objs) = xref::open(&data).ok().and_then(|doc| load_reachable(&doc).ok()) {
                    out.push(objs);
                }
            }
        }
        assert!(out.len() > 15, "{} tables", out.len());
        out
    }

    fn table_resolve(objs: &BTreeMap<u32, (u16, Obj)>, o: &Obj) -> Option<Obj> {
        match o {
            Obj::Ref(n, _) => objs.get(n).map(|x| x.1.clone()),
            x => Some(x.clone()),
        }
    }

    #[test]
    fn job_refs_cover_every_lookup() {
        let (mut streams, mut looked_up) = (0, 0);
        for objs in tables() {
            for (&n, (_, o)) in &objs {
                let Obj::Stream(s) = o else { continue };
                let refs: BTreeSet<u32> = job_refs(&s.dict, &objs).iter().map(|r| r.0).collect();
                let asked = RefCell::new(BTreeSet::new());
                let recording = |o: &Obj| {
                    if let Obj::Ref(r, _) = o {
                        asked.borrow_mut().insert(*r);
                    }
                    table_resolve(&objs, o)
                };
                // The quick settings (at 15 as the bridge gives them), and the fast encoder at 0 and 15.
                let quick = |e: u8| Options { flate: flate::Options::quick(), ..Options::effort(e) };
                for opt in [quick(0), quick(15), quick(50), quick(100), Options::effort(0), Options::effort(15)] {
                    let mut content = None;
                    reencode(s, &opt, &recording, &mut content);
                    short_image(s, &recording, content.as_ref());
                    short_image(s, &recording, None);
                }
                let asked = asked.into_inner();
                let left: Vec<&u32> = asked.iter().filter(|r| objs.contains_key(*r) && !refs.contains(*r)).collect();
                assert!(left.is_empty(), "stream {n} looked up {left:?}, which job_refs leaves out");
                streams += 1;
                looked_up += asked.len();
            }
        }
        assert!(streams > 40 && looked_up >= 16, "{streams} streams, {looked_up} lookups");
    }

    #[test]
    fn job_resolve_equals_the_table() {
        let (mut compared, mut faxes) = (0, 0);
        for objs in tables() {
            for (&n, (_, o)) in &objs {
                let Obj::Stream(s) = o else { continue };
                let job = Job { n, s, settings: None, xmp: false, shares: false, refs: job_refs(&s.dict, &objs) };
                let mine = |o: &Obj| job.resolve(o);
                let table = |o: &Obj| table_resolve(&objs, o);
                assert_eq!(images::geometry(&s.dict, &mine), images::geometry(&s.dict, &table), "stream {n}");
                if let Some(cs) = s.dict.get(b"ColorSpace") {
                    assert_eq!(images::components(cs, &mine), images::components(cs, &table), "stream {n}");
                }
                let why = crate::fax::pdf::explain(s, &table);
                assert_eq!(crate::fax::pdf::explain(s, &mine), why, "stream {n}");
                faxes += why.starts_with("JBIG2") as usize;
                for opt in [Options { flate: flate::Options::quick(), ..Options::effort(0) }, Options::effort(0)] {
                    let (a, b) = (reencode(s, &opt, &mine, &mut None), reencode(s, &opt, &table, &mut None));
                    assert!(a == b, "stream {n}, effort {}", opt.flate.fast);
                }
                compared += 1;
            }
        }
        assert!(compared > 40 && faxes >= 2, "{compared} streams, {faxes} fax images made JBIG2");
    }

    /// A PDF of these objects, numbered from 1 (the first is the catalog).
    fn file_of(objs: &[Vec<u8>]) -> Vec<u8> {
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

    /// A stream object: `entries` and `/Length` (direct, unless `entries` has one) around `data`.
    fn stream_of(entries: &str, data: &[u8]) -> Vec<u8> {
        let length = if entries.contains("/Length") { String::new() } else { format!(" /Length {}", data.len()) };
        let mut o = format!("<< {entries}{length} >>\nstream\n").into_bytes();
        o.extend_from_slice(data);
        o.extend_from_slice(b"\nendstream");
        o
    }

    /// A page drawing `n` different small images (each its own job), stored with the fast encoder's quickest
    /// level, so that effort 0 re-encodes every one.
    fn images_pdf(n: usize) -> Vec<u8> {
        let names: String = (0..n).map(|k| format!("/I{k} {} 0 R ", k + 4)).collect();
        let mut objs: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << {names}>> >> >>")
                .into_bytes(),
        ];
        for k in 0..n {
            let px: Vec<u8> = (0..64 * 48).map(|i| ((i % 64) * (k % 5 + 1) + (i / 64) * (k / 5 + 1)) as u8).collect();
            let data = flate::deflate_zlib(&px, &flate::Options { fast: 1, ..flate::Options::quick() });
            let d = "/Type /XObject /Subtype /Image /Width 64 /Height 48 /ColorSpace /DeviceGray /BitsPerComponent 8 \
                     /Filter /FlateDecode";
            objs.push(stream_of(d, &data));
        }
        file_of(&objs)
    }

    #[test]
    fn jobs_in_flight_never_exceed_threads() {
        let pdf = images_pdf(40);
        let one = optimize(&pdf, &Options { threads: 1, ..Options::effort(0) });
        let Outcome::Smaller(stats) = &one.outcome else { panic!("{:?}", one.outcome) };
        assert!(stats.streams_reencoded == 40 && MOST_JOBS.get() == 1);
        for threads in [2, 3, 4] {
            for seed in 1..=4 {
                let r = par::jitter::with(seed, || optimize(&pdf, &Options { threads, ..Options::effort(0) }));
                let most = MOST_JOBS.get();
                assert!(r.data == one.data, "{threads} threads, seed {seed}");
                assert!((1..=threads).contains(&most), "{threads} threads, seed {seed}: {most} jobs at once");
            }
        }
    }

    /// Streams that share their encoder input under other dictionaries: one content stored plain under a
    /// direct and an indirect /Length, and compressed as two forms with other boxes; an image of repeating
    /// rows twice (once with /Interpolate), and its samples stored a third time with the Up predictor.
    fn sharing_pdf() -> Vec<u8> {
        let quick = |d: &[u8]| flate::deflate_zlib(d, &flate::Options::quick());
        let text: Vec<u8> =
            (0..300).flat_map(|k| format!("q 0.5 g {} {} 10 10 re f Q\n", k % 37, k / 3).into_bytes()).collect();
        let rows: Vec<Vec<u8>> = (0..4).map(|r| (0..64).map(|x| (x * (r + 1) + r * 50) as u8).collect()).collect();
        let px: Vec<u8> = (0..96).flat_map(|y| rows[y / 8 % 4].clone()).collect();
        let up = images::png_filter(&px, 64, 1, images::Choice::Fixed(2));
        let image = "/Type /XObject /Subtype /Image /Width 64 /Height 96 /ColorSpace /DeviceGray /BitsPerComponent 8 \
                     /Filter /FlateDecode";
        let form = |b: u32| format!("/Type /XObject /Subtype /Form /BBox [0 0 {b} {b}] /Filter /FlateDecode");
        file_of(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 5 0 R] /Resources << /XObject \
              << /A 6 0 R /B 7 0 R /C 8 0 R /D 9 0 R /E 10 0 R >> >> >>"
                .to_vec(),
            stream_of("", &text),
            stream_of("/Length 11 0 R", &text),
            stream_of(&form(100), &quick(&text)),
            stream_of(&form(200), &quick(&text)),
            stream_of(image, &quick(&px)),
            stream_of(&format!("{image} /Interpolate true"), &quick(&px)),
            stream_of(
                &format!("{image} /DecodeParms << /Predictor 15 /Colors 1 /BitsPerComponent 8 /Columns 64 >>"),
                &quick(&up),
            ),
            text.len().to_string().into_bytes(),
        ])
    }

    /// One content stored twice (under a direct and an indirect /Length), compressed so well that at effort 50
    /// the first takes most of the work budget and the second gets cheaper settings: one input encoded with
    /// two settings in one file.
    fn mixed_pdf() -> Vec<u8> {
        let text: Vec<u8> =
            (0..800).flat_map(|k| format!("q 0.5 g {} {} 10 10 re f Q\n", k % 7, k / 7 % 4).into_bytes()).collect();
        let z = flate::deflate_zlib(&text, &flate::Options::quick());
        file_of(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 5 0 R] >>".to_vec(),
            stream_of("/Filter /FlateDecode", &z),
            stream_of("/Filter /FlateDecode /Length 6 0 R", &z),
            z.len().to_string().into_bytes(),
        ])
    }

    /// The effort `effort_plan` gives each distinct stream of `pdf` (None: kept as it is).
    fn planned(pdf: &[u8], opt: &Options) -> Vec<Option<u8>> {
        let doc = xref::open(pdf).unwrap();
        let objs = load_reachable(&doc).unwrap();
        let streams: Vec<(u32, &Stream)> = objs
            .iter()
            .filter_map(|(&n, (_, o))| if let Obj::Stream(s) = o { Some((n, &**s)) } else { None })
            .collect();
        let (_, unique, _) = identical_streams(&streams);
        effort_plan(&streams, &unique, opt, pdf.len(), None).into_iter().map(|o| o.map(|o| o.effort)).collect()
    }

    #[test]
    fn the_store_never_changes_the_output() {
        let mixed = mixed_pdf();
        let plan = planned(&mixed, &Options::effort(50));
        assert!(plan.len() == 2 && plan[0] == Some(50) && plan[1].is_some_and(|e| e < 50), "{plan:?}");
        for (pdf, efforts) in [(sharing_pdf(), &[0, 50, 100][..]), (mixed, &[50][..])] {
            for &effort in efforts {
                let at = |store_budget, threads| Options { store_budget, threads, ..Options::effort(effort) };
                let none = optimize(&pdf, &at(0, 1));
                let Outcome::Smaller(stats) = &none.outcome else { panic!("{:?}", none.outcome) };
                assert!(stats.streams_reencoded >= 1, "effort {effort}: {stats:?}");
                assert_eq!(STORE.get(), Default::default(), "no store, nothing counted");
                for budget in [0, 1 << 10, 2 << 20, 1 << 30] {
                    for threads in [1, 2, 4] {
                        for seed in if threads == 1 { 0..=0 } else { 1..=4 } {
                            let r = par::jitter::with(seed, || optimize(&pdf, &at(budget, threads)));
                            let what = format!(
                                "{} streams, effort {effort}, budget {budget}, {threads} threads, seed {seed}",
                                stats.streams
                            );
                            assert!(r.data == none.data, "{what}");
                            let st = STORE.get();
                            assert!(st.peak <= budget, "{what}: {st:?}");
                            // The four streams of one content are encoded once (and so are the images' samples).
                            if budget >= 2 << 20 && stats.streams > 2 {
                                assert!(st.hits >= 3, "{what}: {st:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_store_stays_within_its_budget() {
        // 2,000 different small content streams: their outputs and entry costs come to far more than the
        // budget.
        let n = 2000;
        let contents: Vec<String> = (0..n).map(|k| format!("{} 0 R", k + 4)).collect();
        let mut objs = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [{}] >>", contents.join(" "))
                .into_bytes(),
        ];
        for k in 0..n {
            let text = format!("q {k} 0 0 {k} 0 0 cm 0 0 1 1 re f Q\n").repeat(8);
            objs.push(stream_of(
                "/Filter /FlateDecode",
                &flate::deflate_zlib(text.as_bytes(), &flate::Options::quick()),
            ));
        }
        let pdf = file_of(&objs);
        let budget = 64 << 10;
        let none = optimize(&pdf, &Options { store_budget: 0, threads: 1, ..Options::effort(0) });
        assert!(matches!(none.outcome, Outcome::Smaller(_)), "{:?}", none.outcome);
        for threads in [1, 4] {
            let r = optimize(&pdf, &Options { store_budget: budget, threads, ..Options::effort(0) });
            assert!(r.data == none.data, "{threads} threads");
            let st = STORE.get();
            assert!(st.computed >= n && st.hits == 0, "{threads} threads: {st:?}");
            assert!(
                st.peak <= budget && st.kept * par::ENTRY_COST <= budget && st.kept < n,
                "{threads} threads: {st:?}"
            );
            assert!(st.peak > budget / 2, "the budget was filled: {st:?}");
        }
    }
}
