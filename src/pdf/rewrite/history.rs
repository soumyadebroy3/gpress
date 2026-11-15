//! Files that keep earlier saves (incremental updates): only the newest save is written, and the earlier ones are
//! listed as dropped.
//!
//! Where each save ends is found first (`xref::saves`). Every save is then read on its own, the file cut at its end,
//! the way a reader opens that cut file: it must open, load and pass the gate. A signature, a certification, usage
//! rights, PDF/A or PDF/X in any save leaves the file untouched whatever the options say. `dropped` lists the earlier
//! saves; `rewrite` then goes on with the newest as a file of one save.

use super::*;
use crate::pdf::xref::{Limits, Save};

const OPENS: &str = "keeps earlier revisions: a save that doesn't open on its own";
const GENERATION: &str = "keeps earlier revisions: a reference to another generation";
const ABSENT: &str = "keeps earlier revisions: a reference to an object that isn't there";
const ELSEWHERE: &str = "keeps earlier revisions: a row that doesn't point at its object";
const LACKS: &str = "keeps earlier revisions: a trailer key that only an earlier save has";
const NOT_CATALOG: &str = "keeps earlier revisions: a /Root that isn't a catalog";

/// What a save's trailer reaches: each object's generation and value, by number.
type Reached = BTreeMap<u32, (u16, Obj)>;

/// An earlier save that was dropped.
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

/// The file's saves, oldest first, when it has more than one and they may be read. An earlier save the newest doesn't
/// link is allowed: it is dropped.
fn find(input: &[u8], rules: &RewriteRules) -> Result<Vec<Save>, String> {
    if let Some(why) = marker(input) {
        return Err(why.into());
    }
    let saves = xref::saves_at_most(input, rules.size(R::most_saves))?;
    // One save in a layout `open_with` doesn't take for one (a hybrid file, say): left as it is, as before.
    if saves.len() < 2 {
        return Err(xref::KEEPS.into());
    }
    // A trailer that lacks a key an earlier one has: some readers take its value from the earlier trailer (see
    // `Save::lacks_key`).
    if saves.iter().any(|s| s.lacks_key) {
        return Err(LACKS.into());
    }
    // Each save is read as a file of its own: together they are at most 16 times the input (the rule
    // `saves_length_factor`).
    let most = (input.len() as u64).saturating_mul(rules.at(R::saves_length_factor));
    if saves.iter().map(|s| s.end as u64).sum::<u64>() > most {
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
fn read(
    input: &[u8],
    end: usize,
    (k, of): (usize, usize),
    opt: &Options,
    budget: &mut Budget,
) -> Result<Reached, String> {
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
    Ok(objs)
}

/// Every save of a file that keeps earlier ones, read and checked on its own, and the list of the earlier saves, which
/// are dropped. `newest` is what the whole file's trailer reaches.
pub(super) fn dropped(input: &[u8], opt: &Options, newest: &Reached) -> Result<Vec<Dropped>, String> {
    let saves = find(input, &opt.rewrite_rules)?;
    let mut budget = Budget::new(input, opt);
    let mut out = Vec::new();
    let mut start = 0;
    for (k, save) in saves.iter().enumerate() {
        let objs = read(input, save.end, (k, saves.len()), opt, &mut budget)?;
        if k + 1 < saves.len() {
            let versions = objs.iter().filter(|(n, v)| newest.get(n) != Some(v)).count();
            out.push(Dropped { start, end: save.end, versions });
        }
        start = save.end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
