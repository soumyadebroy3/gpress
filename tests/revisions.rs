//! Files that keep earlier saves (incremental updates) are written with every save kept, each the same as the
//! input's when the file is cut at its end, or with only the newest when the caller asks; a file whose saves can't
//! be told apart for certain is left untouched. One save is rewritten as before, the two sections of a linearised
//! file or of a Prince hybrid file included.

use gpress::clock::Deadline;
use gpress::pdf::object::{Dict, Obj};
use gpress::pdf::rewrite::{optimize, Dropped, Options, Outcome};
use gpress::pdf::xref;
use std::collections::BTreeMap;
use std::time::Duration;

const KEEPS: &str = "keeps earlier revisions (incremental updates)";
const UNLINKED: &str = "keeps earlier revisions: a save the newest doesn't link";

/// A file written piece by piece, remembering where each object starts.
struct Pdf {
    d: Vec<u8>,
    at: BTreeMap<u32, usize>,
}

impl Pdf {
    fn new() -> Pdf {
        Pdf { d: b"%PDF-1.5\n".to_vec(), at: BTreeMap::new() }
    }

    fn obj(&mut self, n: u32, body: &[u8]) {
        self.at.insert(n, self.d.len());
        self.d.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        self.d.extend_from_slice(body);
        self.d.extend_from_slice(b"\nendobj\n");
    }

    /// A cross-reference table with a row for each number from `first` (in use if written, free if not), its
    /// trailer, and `startxref` naming `start` (the table itself when None). Returns where the table is.
    fn table(&mut self, first: u32, count: u32, trailer: &str, start: Option<usize>) -> usize {
        self.table_of(&[(first, count)], trailer, start)
    }

    /// The same with a subsection for each (first, count).
    fn table_of(&mut self, subsections: &[(u32, u32)], trailer: &str, start: Option<usize>) -> usize {
        let x = self.d.len();
        self.d.extend_from_slice(b"xref\n");
        for &(first, count) in subsections {
            self.d.extend_from_slice(format!("{first} {count}\n").as_bytes());
            for n in first..first + count {
                let row = match self.at.get(&n) {
                    Some(off) if n > 0 => format!("{off:010} 00000 n \n"),
                    _ => format!("0000000000 {:05} f \n", if n == 0 { 65535 } else { 0 }),
                };
                self.d.extend_from_slice(row.as_bytes());
            }
        }
        let s = start.unwrap_or(x);
        self.d.extend_from_slice(format!("trailer\n<< {trailer} >>\nstartxref\n{s}\n%%EOF\n").as_bytes());
        x
    }
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut b = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    b.extend_from_slice(data);
    b.extend_from_slice(b"\nendstream");
    b
}

fn text(t: &str) -> Vec<u8> {
    stream("", &format!("BT /F1 12 Tf 10 10 Td ({t}) Tj ET\n").repeat(50).into_bytes())
}

/// Catalog 1, pages 2, page 3 showing `t` (content 4) in font 5.
fn page(p: &mut Pdf, t: &str) {
    p.obj(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    p.obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    p.obj(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    p.obj(4, &text(t));
    p.obj(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
}

/// The first save: one page showing "ORIGINAL SECRET TEXT". Returns where its table is.
fn first_save() -> (Pdf, usize) {
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    let x = p.table(0, 6, "/Size 6 /Root 1 0 R", None);
    (p, x)
}

fn untouched(input: &[u8]) {
    untouched_for(input, KEEPS, &Options::effort(0));
}

/// `input` comes back as it is, for the reason `why`.
fn untouched_for(input: &[u8], why: &str, opt: &Options) {
    let r = optimize(input, opt);
    assert!(matches!(&r.outcome, Outcome::Untouched(w) if w == why), "{:?}", r.outcome);
    assert_eq!(r.data, input);
}

/// Everything the trailer of `data` reaches, read here without the writer's help: each object's generation and
/// value, a stream as its dictionary without the entries about its encoding and its decoded bytes.
fn reached(data: &[u8]) -> BTreeMap<u32, (u16, Obj, Option<Vec<u8>>)> {
    let doc = xref::open(data).unwrap();
    let mut todo = Vec::new();
    for (k, v) in &doc.trailer.0 {
        if !xref::STRUCTURAL.contains(&k.as_slice()) {
            v.refs(&mut todo);
        }
    }
    let mut out = BTreeMap::new();
    while let Some((n, _)) = todo.pop() {
        if out.contains_key(&n) {
            continue;
        }
        let Ok((g, o)) = doc.load(n) else { continue };
        o.refs(&mut todo);
        out.insert(
            n,
            match o {
                Obj::Stream(s) => {
                    let about = |k: &[u8]| matches!(k, b"Length" | b"Filter" | b"DecodeParms");
                    let dict = Dict(s.dict.0.iter().filter(|(k, _)| !about(k)).cloned().collect());
                    (g, Obj::Dict(dict), Some(xref::decode_fully(&s.dict, &s.data, 1 << 20).unwrap()))
                }
                o => (g, o, None),
            },
        );
    }
    out
}

/// `input` is written smaller with each of its `revisions` saves kept: the output cut at a save's end reaches what
/// the input cut at that save's end reaches, and its trailer says the same about the document. Returns the output.
fn kept_with(input: &[u8], revisions: usize, opt: &Options) -> Vec<u8> {
    let r = optimize(input, opt);
    let Outcome::Smaller(stats) = &r.outcome else { panic!("{:?}", r.outcome) };
    assert_eq!((stats.history_kept, stats.history_dropped.len()), (revisions - 1, 0));
    let (a, b) = (xref::saves(input).unwrap(), xref::saves(&r.data).unwrap());
    assert_eq!((a.len(), b.len()), (revisions, revisions));
    for (a, b) in a.iter().zip(&b) {
        let (old, new) = (&input[..a.end], &r.data[..b.end]);
        assert_eq!(reached(old), reached(new));
        let about = |d: &[u8]| -> Vec<(Vec<u8>, Obj)> {
            let doc = xref::open(d).unwrap();
            doc.trailer.0.iter().filter(|(k, _)| !xref::STRUCTURAL.contains(&k.as_slice())).cloned().collect()
        };
        assert_eq!(about(old), about(new));
    }
    r.data
}

fn kept(input: &[u8], revisions: usize) -> Vec<u8> {
    kept_with(input, revisions, &Options::effort(0))
}

/// With `drop_history`, `input` is written as its newest save alone: the bytes that save gives as a file of its
/// own would not be the same (the numbers differ), so what it reaches is compared. Returns what was dropped.
fn dropped(input: &[u8]) -> Vec<Dropped> {
    let r = optimize(input, &Options { drop_history: true, ..Options::effort(0) });
    let Outcome::Smaller(stats) = &r.outcome else { panic!("{:?}", r.outcome) };
    assert_eq!(stats.history_kept, 0);
    assert_eq!(xref::saves(&r.data).unwrap().len(), 1);
    assert!(!xref::open(&r.data).unwrap().has_revisions);
    assert_eq!(reached(input), reached(&r.data));
    stats.history_dropped.clone()
}

/// `input` is rewritten smaller, and objects 1 to 5 read back the same (streams by their decoded bytes).
fn rewritten(input: &[u8]) {
    let doc = xref::open(input).unwrap();
    assert_eq!(doc.sections, 2);
    let r = optimize(input, &Options::effort(0));
    assert!(matches!(r.outcome, Outcome::Smaller(_)), "{:?}", r.outcome);
    let out = xref::open(&r.data).unwrap();
    for n in 1..=5 {
        match doc.load(n).unwrap().1 {
            Obj::Stream(s) => match out.load(n).unwrap().1 {
                Obj::Stream(t) => assert_eq!(
                    xref::decode_fully(&s.dict, &s.data, 1 << 20),
                    xref::decode_fully(&t.dict, &t.data, 1 << 20)
                ),
                o => panic!("object {n} is {o:?}"),
            },
            o => assert_eq!(out.load(n).unwrap().1, o),
        }
    }
}

#[test]
fn a_save_that_only_adds_objects_is_kept_with_the_first() {
    // The update writes a new catalog, pages, page and content and names the new catalog: no number is defined
    // twice, so only the second section shows that the first save is still in the file.
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Type /Catalog /Pages 7 0 R >>");
    p.obj(7, b"<< /Type /Pages /Kids [8 0 R] /Count 1 >>");
    p.obj(8, b"<< /Type /Page /Parent 7 0 R /MediaBox [0 0 200 200] /Contents 9 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    p.obj(9, &text("NEW VERSION"));
    p.table(6, 4, &format!("/Size 10 /Root 6 0 R /Prev {x}"), None);
    kept(&p.d, 2);
}

#[test]
fn a_save_that_adds_only_document_information_is_kept_with_the_first() {
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) >>");
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    kept(&p.d, 2);
}

#[test]
fn a_save_that_lists_every_row_again_unchanged_is_kept_with_the_first() {
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) >>");
    p.table(0, 7, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    kept(&p.d, 2);
}

#[test]
fn a_save_that_changes_only_the_trailer_is_kept_with_the_first() {
    let (mut p, x) = first_save();
    p.table(0, 0, &format!("/Size 6 /Root 1 0 R /ID [<01> <02>] /Prev {x}"), None);
    kept(&p.d, 2);
}

#[test]
fn a_save_no_section_links_is_kept_only_to_be_dropped() {
    // A whole second file appended after the first, with no /Prev: the newest chain is one section, but the
    // first save is still in the bytes.
    let (mut p, _) = first_save();
    p.d.extend_from_slice(b"%PDF-1.5\n");
    page(&mut p, "NEW VERSION");
    p.table(0, 6, "/Size 6 /Root 1 0 R", None);
    untouched_for(&p.d, UNLINKED, &Options::effort(0));
    assert_eq!(dropped(&p.d).len(), 1);
}

#[test]
fn a_save_that_a_later_startxref_skips_is_a_revision() {
    // An update with its own table and `startxref`, then a `startxref` that names the first save's table again:
    // the chain is one section, and the update is still in the file.
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) >>");
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    p.d.extend_from_slice(format!("startxref\n{x}\n%%EOF\n").as_bytes());
    untouched(&p.d);
}

#[test]
fn one_startxref_more_than_sections_is_a_revision() {
    // An update whose table has no `startxref` of its own, then one that names the first save's table again: one
    // section, two `startxref`.
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) >>");
    let row = format!("{:010} 00000 n \n", p.at[&6]);
    p.d.extend_from_slice(
        format!("xref\n6 1\n{row}trailer\n<< /Size 7 /Root 1 0 R /Info 6 0 R /Prev {x} >>\n").as_bytes(),
    );
    p.d.extend_from_slice(format!("startxref\n{x}\n%%EOF\n").as_bytes());
    assert_eq!(xref::open(&p.d).unwrap().sections, 1);
    untouched(&p.d);
}

/// One save, linearised in qpdf's layout, after what `p` holds: the linearisation dictionary (10); the first-page
/// section, with its row, the /Size of the whole file and a /Prev naming the main section at the end; and the main
/// section, with the page's rows and a /Size of its own. With `first_startxref` a `startxref` follows the
/// first-page section, as qpdf writes one after a table; without it none does, as qpdf writes none after a
/// cross-reference stream. Returns where the first-page section is.
fn linearised_save(mut p: Pdf, first_startxref: bool) -> (Pdf, usize) {
    p.obj(10, b"<< /Linearized 1 /L 0 /O 3 /E 0 /N 1 /T 0 /H [0 0] >>");
    // Room for the first-page section, written once the offsets are known.
    let first = p.d.len();
    p.d.extend_from_slice(&[b' '; 200]);
    p.d.push(b'\n');
    page(&mut p, "ONE SAVE");
    let main = p.table(0, 10, "/Size 10", Some(first));
    let mut head =
        format!("xref\n10 1\n{:010} 00000 n \ntrailer\n<< /Size 11 /Root 1 0 R /Prev {main} >>\n", p.at[&10]);
    if first_startxref {
        head.push_str("startxref\n0\n%%EOF");
    }
    assert!(head.len() <= 200, "{}", head.len());
    p.d[first..first + head.len()].copy_from_slice(head.as_bytes());
    (p, first)
}

#[test]
fn one_linearised_save_is_rewritten_with_every_object() {
    for first_startxref in [true, false] {
        rewritten(&linearised_save(Pdf::new(), first_startxref).0.d);
    }
}

/// Rows of a cross-reference stream with /W [1 3 1] for the numbers in `ns`, in use if written, free if not.
fn stream_rows(p: &Pdf, ns: std::ops::Range<u32>) -> Vec<u8> {
    ns.flat_map(|n| match p.at.get(&n) {
        Some(&off) if n > 0 => [1, (off >> 16) as u8, (off >> 8) as u8, off as u8, 0],
        _ => [0; 5],
    })
    .collect()
}

/// `rows` of `width` bytes as a cross-reference stream holds them under /Filter /FlateDecode with /Predictor 12 and
/// /Columns `width`: each row after a 0, the tag of PNG's filter "none".
fn predicted(rows: &[u8], width: usize) -> Vec<u8> {
    let tagged: Vec<u8> = rows.chunks(width).flat_map(|r| [&[0], r].concat()).collect();
    gpress::flate::deflate_zlib(&tagged, &gpress::flate::Options::quick())
}

#[test]
fn one_linearised_save_of_cross_reference_streams_is_rewritten_with_every_object() {
    // As qpdf writes it with object streams: the first-page stream (13) after the linearisation dictionary, the main
    // stream (14) at the end. The main stream's dictionary says nothing about the document, and the keys about the
    // cross-reference data differ: /W and /DecodeParms too, as in pdfjs/empty.pdf of the corpus.
    let mut p = Pdf::new();
    p.obj(10, b"<< /Linearized 1 /L 0 /O 3 /E 0 /N 1 /T 0 /H [0 0] >>");
    let first = p.d.len();
    p.d.extend_from_slice(&[b' '; 200]);
    p.d.push(b'\n');
    page(&mut p, "ONE SAVE");
    let rows = predicted(&stream_rows(&p, 0..6), 5);
    let main = p.d.len();
    let flate = "/Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns";
    p.obj(14, &stream(&format!("/Type /XRef /Size 6 /Index [0 6] /W [1 3 1] {flate} 5 >>"), &rows));
    p.d.extend_from_slice(format!("startxref\n{first}\n%%EOF\n").as_bytes());
    let dict = format!("/Type /XRef /Size 15 /Index [10 1] /W [1 2 1] {flate} 4 >> /Root 1 0 R /Prev {main}");
    let at = p.at[&10];
    let mut head = b"13 0 obj\n".to_vec();
    head.extend_from_slice(&stream(&dict, &predicted(&[1, (at >> 8) as u8, at as u8, 0], 4)));
    head.extend_from_slice(b"\nendobj\n");
    assert!(head.len() <= 200, "{}", head.len());
    p.d[first..first + head.len()].copy_from_slice(&head);
    rewritten(&p.d);
}

#[test]
fn a_main_stream_after_a_first_page_table_is_rewritten_with_every_object() {
    // The first-page trailer has none of the keys that only a cross-reference stream has (/Type, /Index, /W, /Filter,
    // /DecodeParms, /Length), so none of them may count as the older trailer saying more.
    let mut p = Pdf::new();
    p.obj(10, b"<< /Linearized 1 /L 0 /O 3 /E 0 /N 1 /T 0 /H [0 0] >>");
    let first = p.d.len();
    p.d.extend_from_slice(&[b' '; 200]);
    p.d.push(b'\n');
    page(&mut p, "ONE SAVE");
    let rows = predicted(&stream_rows(&p, 0..6), 5);
    let main = p.d.len();
    let dict =
        "/Type /XRef /Size 6 /Index [0 6] /W [1 3 1] /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 5 >>";
    p.obj(14, &stream(dict, &rows));
    p.d.extend_from_slice(format!("startxref\n{first}\n%%EOF\n").as_bytes());
    let head = format!("xref\n10 1\n{:010} 00000 n \ntrailer\n<< /Size 15 /Root 1 0 R /Prev {main} >>\n", p.at[&10]);
    assert!(head.len() <= 200, "{}", head.len());
    p.d[first..first + head.len()].copy_from_slice(head.as_bytes());
    rewritten(&p.d);
}

#[test]
fn an_update_to_a_linearised_file_is_kept_with_the_first() {
    let (mut p, first) = linearised_save(Pdf::new(), true);
    p.obj(20, b"<< /Title (Second save) >>");
    p.table(20, 1, &format!("/Size 21 /Root 1 0 R /Info 20 0 R /Prev {first}"), None);
    kept(&p.d, 2);
}

#[test]
fn a_linearised_save_appended_without_prev_is_kept_only_to_be_dropped() {
    // Two linearised saves, the second appended whole with no /Prev: as many `startxref` as the newest save has
    // sections, but the first one comes before them.
    let (mut p, _) = linearised_save(Pdf::new(), false);
    p.d.extend_from_slice(b"%PDF-1.5\n");
    let (p, _) = linearised_save(p, false);
    untouched_for(&p.d, UNLINKED, &Options::effort(0));
    assert_eq!(dropped(&p.d).len(), 1);
}

#[test]
fn an_update_after_one_section_and_a_linearisation_dictionary_is_kept_with_the_first() {
    // Two sections and /Linearized near the start, but the newer section comes after the older one.
    let mut p = Pdf::new();
    p.obj(10, b"<< /Linearized 1 >>");
    page(&mut p, "ORIGINAL SECRET TEXT");
    let x = p.table(0, 11, "/Size 11 /Root 1 0 R", None);
    p.obj(11, b"<< /Title (Second save) >>");
    p.table(11, 1, &format!("/Size 12 /Root 1 0 R /Info 11 0 R /Prev {x}"), None);
    kept(&p.d, 2);
}

#[test]
fn linearised_sections_that_define_an_object_differently_are_a_revision() {
    // The first-page section lists content 4 at a second copy with other text than the main section's.
    let mut p = Pdf::new();
    p.obj(10, b"<< /Linearized 1 /L 0 /O 3 /E 0 /N 1 /T 0 /H [0 0] >>");
    let first = p.d.len();
    p.d.extend_from_slice(&[b' '; 200]);
    p.d.push(b'\n');
    page(&mut p, "OLD TEXT");
    let old4 = p.at[&4];
    p.obj(4, &text("NEW TEXT"));
    let new4 = p.at[&4];
    p.at.insert(4, old4);
    let main = p.table(0, 11, "/Size 11", Some(first));
    let head = format!(
        "xref\n4 1\n{new4:010} 00000 n \ntrailer\n<< /Size 11 /Root 1 0 R /Prev {main} >>\nstartxref\n0\n%%EOF"
    );
    assert!(head.len() <= 200);
    p.d[first..first + head.len()].copy_from_slice(head.as_bytes());
    assert_eq!(xref::open(&p.d).unwrap().sections, 2);
    untouched(&p.d);
}

/// A linearised save whose /Linearized starts at byte `at`, after a comment that pads the header.
fn linearised_at(at: usize) -> Vec<u8> {
    let mut p = Pdf::new();
    p.d.push(b'%');
    p.d.extend(std::iter::repeat_n(b'a', at - 23));
    p.d.push(b'\n');
    let (p, _) = linearised_save(p, true);
    assert_eq!(&p.d[at..at + 11], b"/Linearized");
    p.d
}

#[test]
fn a_linearisation_dictionary_counts_only_within_the_first_1024_bytes() {
    // gpress looks for /Linearized in bytes 0 to 1023 only: ending on byte 1023, it makes the two sections one save;
    // a byte later, they are two saves.
    rewritten(&linearised_at(1013));
    untouched(&linearised_at(1014));
}

/// One save written after what `p` holds as Prince writes a hybrid file: an object stream (12) holding font 5 and
/// its cross-reference stream (11), then the table, which has no row for font 5, then an empty table whose trailer
/// adds /XRefStm (and `more`, which Prince never adds).
fn hybrid_tail(p: Pdf, more: &str) -> Pdf {
    let (mut p, main) = hybrid_main(p, "");
    let xs = p.at[&11];
    p.table(0, 0, &format!("/XRefStm {xs} /Prev {main} /Size 13 /Root 1 0 R{more}"), None);
    p
}

/// That save up to its main table, whose trailer gets `main_more` too. Returns where the table is.
fn hybrid_main(mut p: Pdf, main_more: &str) -> (Pdf, usize) {
    p.obj(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    p.obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    p.obj(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    p.obj(4, &text("ONE SAVE"));
    p.obj(12, &stream("/Type /ObjStm /N 1 /First 4", b"5 0 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"));
    p.obj(11, &stream("/Type /XRef /Size 13 /Index [5 1] /W [1 1 1]", &[2, 12, 0]));
    let main = p.table_of(&[(0, 5), (11, 2)], &format!("/Size 13 /Root 1 0 R{main_more}"), None);
    (p, main)
}

#[test]
fn one_hybrid_save_with_an_empty_last_table_is_rewritten_with_every_object() {
    rewritten(&hybrid_tail(Pdf::new(), "").d);
}

#[test]
fn a_table_with_rows_after_a_hybrid_save_is_a_revision() {
    // The second table has a row of its own (a new font 5) and otherwise looks like the empty one.
    let (mut p, main) = hybrid_main(Pdf::new(), "");
    p.obj(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>");
    let xs = p.at[&11];
    p.table(5, 1, &format!("/XRefStm {xs} /Prev {main} /Size 13 /Root 1 0 R"), None);
    untouched(&p.d);
}

#[test]
fn an_empty_table_that_changes_the_trailer_is_a_revision() {
    untouched(&hybrid_tail(Pdf::new(), " /ID [<01> <02>]").d);
}

#[test]
fn a_main_trailer_that_adds_an_entry_to_the_empty_tables_is_a_revision() {
    // The main table's trailer names document information the empty table's lacks; some readers merge the two.
    let mut p = Pdf::new();
    p.obj(6, b"<< /Title (Main trailer only) >>");
    let (mut p, main) = hybrid_main(p, " /Info 6 0 R");
    let xs = p.at[&11];
    p.table(0, 0, &format!("/XRefStm {xs} /Prev {main} /Size 13 /Root 1 0 R"), None);
    untouched(&p.d);
}

#[test]
fn trailers_that_give_a_key_two_values_are_a_revision() {
    let (mut p, main) = hybrid_main(Pdf::new(), " /ID [<01> <02>]");
    let xs = p.at[&11];
    p.table(0, 0, &format!("/XRefStm {xs} /Prev {main} /Size 13 /Root 1 0 R /ID [<03> <04>]"), None);
    untouched(&p.d);
}

#[test]
fn an_empty_update_that_changes_nothing_is_kept_with_the_first() {
    // An empty table with the first save's trailer and no /XRefStm: an update, though it changes nothing.
    let (mut p, x) = first_save();
    p.table(0, 0, &format!("/Size 6 /Root 1 0 R /Prev {x}"), None);
    kept(&p.d, 2);
}

#[test]
fn an_empty_cross_reference_stream_that_names_an_xrefstm_is_a_revision() {
    // The hybrid shape, but its empty newest section is a cross-reference stream: most readers follow /XRefStm only
    // from a table's trailer, and find no font 5.
    let (mut p, main) = hybrid_main(Pdf::new(), "");
    let (xs, x) = (p.at[&11], p.d.len());
    let dict = format!("/Type /XRef /Size 14 /Index [] /W [1 1 1] /Root 1 0 R /Prev {main} /XRefStm {xs}");
    p.obj(13, &stream(&dict, b""));
    p.d.extend_from_slice(format!("startxref\n{x}\n%%EOF\n").as_bytes());
    untouched(&p.d);
}

#[test]
fn an_empty_table_whose_stream_comes_later_is_a_revision() {
    // The same shape, but the cross-reference stream (and the object it adds) were appended after the first save:
    // the first save, without object 6, is still in the file.
    let (mut p, x) = first_save();
    p.obj(7, &stream("/Type /ObjStm /N 1 /First 4", b"6 0 << /Title (Second save) >>"));
    p.obj(8, &stream("/Type /XRef /Size 9 /Index [6 1] /W [1 1 1]", &[2, 7, 0]));
    let xs = p.at[&8];
    p.table(0, 0, &format!("/XRefStm {xs} /Prev {x} /Size 6 /Root 1 0 R"), None);
    untouched(&p.d);
}

#[test]
fn a_linearised_chain_of_three_sections_is_a_revision() {
    // A first-page section at the start whose /Prev names a main table at the end, whose /Prev names a table in the
    // middle (the first save, with catalog 1): three sections, the first before the second.
    let mut p = Pdf::new();
    p.obj(10, b"<< /Linearized 1 /L 0 /O 8 /E 0 /N 1 /T 0 /H [0 0] >>");
    let first = p.d.len();
    p.d.extend_from_slice(&[b' '; 200]);
    p.d.push(b'\n');
    page(&mut p, "ORIGINAL SECRET TEXT");
    let old = p.table(0, 6, "/Size 11 /Root 1 0 R", None);
    p.obj(6, b"<< /Type /Catalog /Pages 7 0 R >>");
    p.obj(7, b"<< /Type /Pages /Kids [8 0 R] /Count 1 >>");
    p.obj(8, b"<< /Type /Page /Parent 7 0 R /MediaBox [0 0 200 200] /Contents 9 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    p.obj(9, &text("NEW VERSION"));
    let main = p.table(6, 4, &format!("/Size 11 /Prev {old}"), Some(first));
    let head = format!(
        "xref\n10 1\n{:010} 00000 n \ntrailer\n<< /Size 11 /Root 6 0 R /Prev {main} >>\nstartxref\n0\n%%EOF",
        p.at[&10]
    );
    assert!(head.len() <= 200);
    p.d[first..first + head.len()].copy_from_slice(head.as_bytes());
    assert_eq!(xref::open(&p.d).unwrap().sections, 3);
    untouched(&p.d);
}

#[test]
fn an_empty_table_after_an_update_is_a_revision() {
    // The first save (catalog 1), an update that adds catalog 6 and names it, then the Prince-style empty table
    // with an /XRefStm written before the update's table and the update's trailer repeated: three sections.
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Type /Catalog /Pages 7 0 R >>");
    p.obj(7, b"<< /Type /Pages /Kids [8 0 R] /Count 1 >>");
    p.obj(8, b"<< /Type /Page /Parent 7 0 R /MediaBox [0 0 200 200] /Contents 9 0 R /Resources << /Font << /F1 13 0 R >> >> >>");
    p.obj(9, &text("NEW VERSION"));
    p.obj(12, &stream("/Type /ObjStm /N 1 /First 5", b"13 0 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"));
    p.obj(11, &stream("/Type /XRef /Size 14 /Index [13 1] /W [1 1 1]", &[2, 12, 0]));
    let t2 = p.table(6, 7, &format!("/Size 14 /Root 6 0 R /Prev {x}"), None);
    let xs = p.at[&11];
    p.table(0, 0, &format!("/XRefStm {xs} /Prev {t2} /Size 14 /Root 6 0 R"), None);
    assert_eq!(xref::open(&p.d).unwrap().sections, 3);
    untouched(&p.d);
}

#[test]
fn a_linearised_file_whose_main_trailer_adds_an_entry_is_a_revision() {
    // The main trailer names document information the first-page trailer lacks; some readers merge the two.
    let mut p = Pdf::new();
    p.obj(10, b"<< /Linearized 1 /L 0 /O 3 /E 0 /N 1 /T 0 /H [0 0] >>");
    let first = p.d.len();
    p.d.extend_from_slice(&[b' '; 200]);
    p.d.push(b'\n');
    page(&mut p, "ONE SAVE");
    p.obj(6, b"<< /Title (Main trailer only) >>");
    let main = p.table(0, 11, "/Size 11 /Info 6 0 R", Some(first));
    let head = format!(
        "xref\n10 1\n{:010} 00000 n \ntrailer\n<< /Size 11 /Root 1 0 R /Prev {main} >>\nstartxref\n0\n%%EOF",
        p.at[&10]
    );
    assert!(head.len() <= 200);
    p.d[first..first + head.len()].copy_from_slice(head.as_bytes());
    untouched(&p.d);
}

#[test]
fn one_save_is_rewritten() {
    let (p, _) = first_save();
    assert!(matches!(optimize(&p.d, &Options::effort(0)).outcome, Outcome::Smaller(_)));
}

/// The first save, then an update that changes the page's content (4), adds document information (6) and takes
/// the font (5) away, the page (3) written again without it. Returns where the update's table is.
fn changed_save() -> (Pdf, usize) {
    let (mut p, x) = first_save();
    p.obj(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>");
    p.obj(4, &text("NEW VERSION"));
    p.obj(6, b"<< /Title (Second save) >>");
    p.at.remove(&5);
    let x = p.table(3, 4, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    (p, x)
}

fn in_use(data: &[u8], n: u32) -> bool {
    matches!(xref::open(data).unwrap().entries.get(&n), Some(xref::Entry::Offset(..) | xref::Entry::InStream(..)))
}

#[test]
fn a_save_that_changes_and_frees_objects_keeps_both_views() {
    let (p, _) = changed_save();
    let out = kept(&p.d, 2);
    // The font is gone from the newest view only, and the first save's text can still be read.
    let first = &out[..xref::saves(&out).unwrap()[0].end];
    assert!(in_use(first, 5) && !in_use(&out, 5));
    assert!(reached(first)[&4].2.as_ref().unwrap().starts_with(b"BT /F1 12 Tf 10 10 Td (ORIGINAL SECRET TEXT)"));
    assert!(reached(&out)[&4].2.as_ref().unwrap().starts_with(b"BT /F1 12 Tf 10 10 Td (NEW VERSION)"));
}

#[test]
fn a_number_freed_and_used_again_keeps_each_view() {
    // A third save gives the font's number (5), free since the second, to new document information.
    let (mut p, x) = changed_save();
    p.obj(5, b"<< /Title (Third save) >>");
    p.table(5, 1, &format!("/Size 7 /Root 1 0 R /Info 5 0 R /Prev {x}"), None);
    let out = kept(&p.d, 3);
    let ends = xref::saves(&out).unwrap();
    assert!(in_use(&out[..ends[0].end], 5) && !in_use(&out[..ends[1].end], 5) && in_use(&out, 5));
}

#[test]
fn an_object_the_first_save_holds_unused_is_kept_once_a_later_save_uses_it() {
    // The first save lists document information (6) that nothing names; the update's trailer names it.
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    p.obj(6, b"<< /Title (Held from the start) >>");
    let x = p.table(0, 7, "/Size 7 /Root 1 0 R", None);
    p.table(0, 0, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    let out = kept(&p.d, 2);
    assert!(!reached(&out[..xref::saves(&out).unwrap()[0].end]).contains_key(&6) && reached(&out).contains_key(&6));
}

#[test]
fn an_update_written_as_streams_is_kept_with_the_first() {
    // The update holds its document information (6) in an object stream (7) under a cross-reference stream (8).
    let (mut p, x) = first_save();
    p.obj(7, &stream("/Type /ObjStm /N 1 /First 4", b"6 0 << /Title (Second save) >>"));
    let (at7, at8) = (p.at[&7], p.d.len());
    let rows = [[2, 0, 7, 0], [1, (at7 >> 8) as u8, at7 as u8, 0], [1, (at8 >> 8) as u8, at8 as u8, 0]].concat();
    let dict = format!("/Type /XRef /Size 9 /Index [6 3] /W [1 2 1] /Root 1 0 R /Info 6 0 R /Prev {x}");
    p.obj(8, &stream(&dict, &rows));
    p.d.extend_from_slice(format!("startxref\n{at8}\n%%EOF\n").as_bytes());
    kept(&p.d, 2);
}

/// The first save with `more` in its catalog and object 6 as `six`, then an update whose catalog no longer has it.
fn claimed_once(more: &str, six: &[u8]) -> Vec<u8> {
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    p.obj(1, format!("<< /Type /Catalog /Pages 2 0 R {more} >>").as_bytes());
    p.obj(6, six);
    let x = p.table(0, 7, "/Size 7 /Root 1 0 R", None);
    p.obj(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    p.table(1, 1, &format!("/Size 7 /Root 1 0 R /Prev {x}"), None);
    p.d
}

#[test]
fn a_claim_in_any_save_leaves_the_file_untouched_whatever_the_options() {
    // Only the first save is signed, or PDF/A (its metadata compressed, so the claim is not in the file's bytes).
    let xmp = gpress::flate::deflate_zlib(
        b"<x:xmpmeta><pdfaid:part>1</pdfaid:part></x:xmpmeta>",
        &gpress::flate::Options::quick(),
    );
    let cases = [
        (claimed_once("/Sig 6 0 R", b"<< /Type /Sig /ByteRange [0 0 0 0] >>"), "digitally signed"),
        (claimed_once("/Metadata 6 0 R", &stream("/Type /Metadata /Subtype /XML /Filter /FlateDecode", &xmp)), "PDF/A"),
    ];
    for (pdf, why) in &cases {
        for (respect_conformance, drop_history) in [(true, false), (false, false), (true, true), (false, true)] {
            untouched_for(pdf, why, &Options { respect_conformance, drop_history, ..Options::effort(0) });
        }
    }
}

#[test]
fn drop_history_writes_the_newest_save_alone_and_lists_the_rest() {
    let (p, _) = changed_save();
    let first = xref::saves(&p.d).unwrap()[0].end;
    // The first save alone holds the font, and its own page and content.
    assert_eq!(dropped(&p.d), vec![Dropped { start: 0, end: first, versions: 3 }]);
    // A file of one save has nothing to drop.
    let (one, _) = first_save();
    let plain = optimize(&one.d, &Options::effort(0));
    let asked = optimize(&one.d, &Options { drop_history: true, ..Options::effort(0) });
    assert!(matches!(&asked.outcome, Outcome::Smaller(s) if s.history_dropped.is_empty() && s.history_kept == 0));
    assert_eq!(asked.data, plain.data);
}

#[test]
fn the_saves_are_kept_at_every_effort_and_the_same_for_any_thread_count() {
    let (p, _) = changed_save();
    for effort in [0, 20, 100] {
        let one = kept_with(&p.d, 2, &Options { threads: 1, ..Options::effort(effort) });
        for threads in [2, 0] {
            assert_eq!(kept_with(&p.d, 2, &Options { threads, ..Options::effort(effort) }), one, "{effort} {threads}");
        }
    }
}

#[test]
fn a_deadline_that_has_passed_gives_nothing() {
    let (p, _) = changed_save();
    for drop_history in [false, true] {
        let gone = Options { deadline: Deadline::after(Duration::ZERO), drop_history, ..Options::effort(0) };
        let r = optimize(&p.d, &gone);
        assert!(matches!(r.outcome, Outcome::TimedOut), "{:?}", r.outcome);
        assert_eq!(r.data, p.d);
    }
}

#[test]
fn saves_that_come_out_no_smaller_are_left_untouched() {
    let mut p = Pdf::new();
    p.obj(1, b"<< /Type /Catalog >>");
    let x = p.table(0, 2, "/Size 2 /Root 1 0 R", None);
    p.table(0, 0, &format!("/Size 2 /Root 1 0 R /ID [<01> <02>] /Prev {x}"), None);
    untouched_for(&p.d, "no saving possible", &Options::effort(0));
}

#[test]
fn numbers_too_high_for_the_first_save_leave_the_file_untouched() {
    // New objects are numbered above every save's numbers: here above the update's 594 objects, which is more
    // than a file as short as the first save may list (a third of its length, as qpdf reads it).
    let (mut p, x) = first_save();
    for n in 6..600 {
        p.obj(n, b"<< >>");
    }
    p.table(6, 594, &format!("/Size 600 /Root 1 0 R /Prev {x}"), None);
    let why = "keeps earlier revisions: numbers too high for a revision's length";
    untouched_for(&p.d, why, &Options::effort(0));
    // The newest save alone has no early revision to hold.
    assert_eq!(dropped(&p.d).len(), 1);
}

#[test]
fn a_reference_to_another_generation_leaves_the_file_untouched() {
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) /Font 5 1 R >>");
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    untouched_for(&p.d, "keeps earlier revisions: a reference to another generation", &Options::effort(0));
}

#[test]
fn a_startxref_in_a_comment_after_another_leaves_the_file_untouched() {
    // The number after the first `startxref` lies past the second one, which the comment holds.
    let (mut p, x) = first_save();
    p.d.extend_from_slice(format!("startxref %startxref\n{x}\n%%EOF\n").as_bytes());
    untouched(&p.d);
}

#[test]
fn more_saves_than_are_taken_apart_leave_the_file_untouched() {
    let (mut p, mut x) = first_save();
    for _ in 0..xref::MOST_SAVES {
        x = p.table(0, 0, &format!("/Size 6 /Root 1 0 R /Prev {x}"), None);
    }
    untouched_for(&p.d, xref::TOO_MANY, &Options::effort(0));
}

#[test]
fn a_prev_of_zero_leaves_the_file_untouched() {
    // The first save's cross-reference stream (7) is the file's first object, so a chain that goes on at a /Prev of
    // 0 finds it there. qpdf and PDFium end the chain instead, and show nothing that only the first save lists.
    let mut p = Pdf::new();
    p.obj(7, &stream("/Type /XRef /Size 8 /W [1 2 1] /Index [1 5 7 1] /Root 1 0 R", &[0; 24]));
    let rows_at = p.d.len() - 24 - b"\nendstream\nendobj\n".len();
    page(&mut p, "ORIGINAL SECRET TEXT");
    let rows: Vec<u8> = [1, 2, 3, 4, 5, 7].iter().flat_map(|n| [1, (p.at[n] >> 8) as u8, p.at[n] as u8, 0]).collect();
    p.d[rows_at..rows_at + 24].copy_from_slice(&rows);
    p.d.extend_from_slice(b"startxref\n9\n%%EOF\n");
    p.obj(6, b"<< /Title (Second save) >>");
    p.table(6, 1, "/Size 8 /Root 1 0 R /Info 6 0 R /Prev 0", None);
    assert_eq!(xref::open(&p.d).unwrap().sections, 2);
    untouched(&p.d);
    untouched_for(&p.d, KEEPS, &Options { drop_history: true, ..Options::effort(0) });
}

#[test]
fn a_reference_to_an_object_that_is_not_there_leaves_the_file_untouched() {
    // Here and for qpdf the reference is null; readers that keep the earlier save's row still show the object.
    // The update frees the document information (6), which its trailer still names.
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    p.obj(6, b"<< /Title (First save) >>");
    let x = p.table(0, 7, "/Size 7 /Root 1 0 R /Info 6 0 R", None);
    p.at.remove(&6);
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    // The update frees the font (5) and writes the page (3) again, naming the font at another generation.
    let (mut q, x) = first_save();
    q.obj(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 1 R >> >> >>");
    q.at.remove(&5);
    q.table_of(&[(3, 1), (5, 1)], &format!("/Size 6 /Root 1 0 R /Prev {x}"), None);
    // No save lists the annotations (9) the page names, or the document information (9) the update's trailer
    // names: Poppler then looks through the bytes and takes the last object of every number, whatever the rows say.
    let (mut r, x) = first_save();
    r.obj(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Annots 9 0 R >>");
    r.table(3, 1, &format!("/Size 6 /Root 1 0 R /Prev {x}"), None);
    let (mut t, x) = first_save();
    t.table(0, 0, &format!("/Size 6 /Root 1 0 R /Info 9 0 R /Prev {x}"), None);
    let why = "keeps earlier revisions: a reference to an object that isn't there";
    for pdf in [&p.d, &q.d, &r.d, &t.d] {
        for drop_history in [false, true] {
            untouched_for(pdf, why, &Options { drop_history, ..Options::effort(0) });
        }
    }
}

/// An update's table and trailer written by hand: `rows` after `xref`, the trailer's entries, the table's place.
fn table_by_hand(p: &mut Pdf, rows: &str, trailer: &str) {
    let x = p.d.len();
    p.d.extend_from_slice(format!("xref\n{rows}trailer\n<< {trailer} >>\nstartxref\n{x}\n%%EOF\n").as_bytes());
}

#[test]
fn a_length_or_an_object_stream_at_another_generation_leaves_the_file_untouched() {
    let both = |d: &[u8]| {
        for drop_history in [false, true] {
            let why = "keeps earlier revisions: a reference to another generation";
            untouched_for(d, why, &Options { drop_history, ..Options::effort(0) });
        }
    };
    // The content (4) takes its length from object 6, which the update writes again at generation 1: the length is
    // read by number here, and at the generation the stream names by qpdf, Poppler and pypdf.
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    // (The stream ends at either length: the bytes between the two ends are more text.)
    let short = b"BT /F1 12 Tf 10 10 Td (SHORT) Tj ET\n".repeat(50);
    let long = [&short[..], b"\nendstream\nendobj\n", &b"BT 10 30 Td (LONG TAIL) Tj ET\n".repeat(50)[..]].concat();
    p.obj(4, &[b"<< /Length 6 0 R >>\nstream\n", &long[..], b"\nendstream"].concat());
    p.obj(6, short.len().to_string().as_bytes());
    let x = p.table(0, 7, "/Size 7 /Root 1 0 R", None);
    let at = p.d.len();
    p.d.extend_from_slice(format!("6 1 obj\n{}\nendobj\n", long.len()).as_bytes());
    table_by_hand(&mut p, &format!("6 1\n{at:010} 00001 n \n"), &format!("/Size 7 /Root 1 0 R /Prev {x}"));
    both(&p.d);

    // The update of `an_update_written_as_streams_is_kept_with_the_first`, then a save that writes its object
    // stream (7) again at generation 1: the rows name the stream by number alone.
    let (mut p, x) = first_save();
    p.obj(7, &stream("/Type /ObjStm /N 1 /First 4", b"6 0 << /Title (Second save) >>"));
    let (at7, at8) = (p.at[&7], p.d.len());
    let rows = [[2, 0, 7, 0], [1, (at7 >> 8) as u8, at7 as u8, 0], [1, (at8 >> 8) as u8, at8 as u8, 0]].concat();
    let dict = format!("/Type /XRef /Size 9 /Index [6 3] /W [1 2 1] /Root 1 0 R /Info 6 0 R /Prev {x}");
    p.obj(8, &stream(&dict, &rows));
    p.d.extend_from_slice(format!("startxref\n{at8}\n%%EOF\n").as_bytes());
    let at = p.d.len();
    p.d.extend_from_slice(b"7 1 obj\n");
    p.d.extend_from_slice(&stream("/Type /ObjStm /N 1 /First 4", b"6 0 << /Title (Third save) >>"));
    p.d.extend_from_slice(b"\nendobj\n");
    let trailer = format!("/Size 9 /Root 1 0 R /Info 6 0 R /Prev {at8}");
    table_by_hand(&mut p, &format!("7 1\n{at:010} 00001 n \n"), &trailer);
    both(&p.d);
}

#[test]
fn a_row_that_does_not_point_at_its_object_leaves_the_file_untouched() {
    // The update lists a number nothing names (8) at the place of the first save's content (4): pypdf moves such a
    // row to the number that stands there, over the update's own row for it.
    let (mut p, x) = first_save();
    let old = p.at[&4];
    p.obj(4, &text("NEW VERSION"));
    let rows = format!("4 1\n{:010} 00000 n \n8 1\n{old:010} 00000 n \n", p.at[&4]);
    table_by_hand(&mut p, &rows, &format!("/Size 9 /Root 1 0 R /Prev {x}"));
    for drop_history in [false, true] {
        let why = "keeps earlier revisions: a row that doesn't point at its object";
        untouched_for(&p.d, why, &Options { drop_history, ..Options::effort(0) });
    }
}

#[test]
fn a_row_in_use_for_object_0_or_a_free_row_of_generation_65535_for_object_1_leaves_the_file_untouched() {
    let both = |d: &[u8]| {
        untouched(d);
        untouched_for(d, KEEPS, &Options { drop_history: true, ..Options::effort(0) });
    };
    // The update writes the content (4) again and lists it under object 0: qpdf 11 reads it as object 4.
    let (mut p, x) = first_save();
    p.obj(4, &text("NEW VERSION"));
    let (at, trailer) = (p.at[&4], format!("/Size 6 /Root 1 0 R /Prev {x}"));
    table_by_hand(&mut p, &format!("0 1\n{at:010} 00000 n \n"), &trailer);
    both(&p.d);

    // A file without an object 1, and an update whose table starts at 1 with the row that object 0 has: Poppler
    // takes such a table to start at 0, so the content (4) it lists is object 3 there.
    let with = |free: &str| {
        let mut p = Pdf::new();
        p.obj(6, b"<< /Type /Catalog /Pages 2 0 R >>");
        p.obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        p.obj(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>");
        p.obj(4, &text("ORIGINAL SECRET TEXT"));
        p.obj(5, b"<< /Title (First save) >>");
        let x = p.table(0, 7, "/Size 7 /Root 6 0 R /Info 5 0 R", None);
        p.obj(4, &text("NEW VERSION"));
        let rows = format!("1 1\n0000000000 {free} f \n4 1\n{:010} 00000 n \n", p.at[&4]);
        table_by_hand(&mut p, &rows, &format!("/Size 7 /Root 6 0 R /Info 5 0 R /Prev {x}"));
        p.d
    };
    both(&with("65535"));
    kept(&with("00000"), 2);
}

#[test]
fn a_prev_that_names_white_space_before_its_section_leaves_the_file_untouched() {
    // The first save's cross-reference stream (7) comes after a line end, and the update's /Prev names that line
    // end: pypdf may give the chain up there and look through the bytes.
    let with = |own: usize, back: usize| {
        let mut p = Pdf::new();
        page(&mut p, "ORIGINAL SECRET TEXT");
        let at7 = p.d.len();
        let mut rows: Vec<u8> = (1..6).flat_map(|n| [1, (p.at[&n] >> 8) as u8, p.at[&n] as u8, 0]).collect();
        rows.extend([0, 0, 0, 0, 1, (at7 >> 8) as u8, at7 as u8, 0]);
        p.obj(7, &stream("/Type /XRef /Size 8 /W [1 2 1] /Index [1 7] /Root 1 0 R", &rows));
        p.d.extend_from_slice(format!("startxref\n{}\n%%EOF\n", at7 - own).as_bytes());
        p.obj(6, b"<< /Title (Second save) >>");
        p.table(6, 1, &format!("/Size 8 /Root 1 0 R /Info 6 0 R /Prev {}", at7 - back), None);
        p.d
    };
    // (The same when the first save's own `startxref` names the line end too.)
    for own in [0, 1] {
        untouched(&with(own, 1));
        untouched_for(&with(own, 1), KEEPS, &Options { drop_history: true, ..Options::effort(0) });
    }
    kept(&with(0, 0), 2);
}

/// An update that adds document information (6) after the section at `x`. Returns where its table is.
fn info_save(p: &mut Pdf, x: usize) -> usize {
    p.obj(6, b"<< /Title (Second save) >>");
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None)
}

#[test]
fn ends_that_cannot_be_told_apart_leave_the_file_untouched() {
    // A `startxref` without a number between the two saves.
    let (mut p, x) = first_save();
    p.d.extend_from_slice(b"startxref\nnone\n%%EOF\n");
    info_save(&mut p, x);
    untouched(&p.d);

    // A `startxref` with no `%%EOF` before the next one.
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) >>");
    let (at6, t) = (p.at[&6], p.d.len());
    let trailer = format!("trailer\n<< /Size 7 /Root 1 0 R /Info 6 0 R /Prev {x} >>");
    let table = format!("xref\n6 1\n{at6:010} 00000 n \n{trailer}\nstartxref\n{t}\nstartxref\n{t}\n%%EOF\n");
    p.d.extend_from_slice(table.as_bytes());
    untouched(&p.d);

    // More than 2048 bytes from a `startxref` to the end of its save: a reader of the file cut there looks for
    // the word in the last bytes only.
    let (mut p, x) = first_save();
    p.d.truncate(p.d.len() - b"%%EOF\n".len());
    p.d.extend_from_slice(format!("%{}\n%%EOF\n", "a".repeat(3000)).as_bytes());
    info_save(&mut p, x);
    untouched(&p.d);

    // The second of three saves ends with a `startxref` that names the third save's table, which lies after it.
    let (mut p, x) = first_save();
    let second = info_save(&mut p, x);
    p.obj(7, b"<< /Title (Third save) >>");
    let third = p.table(7, 1, &format!("/Size 8 /Root 1 0 R /Info 7 0 R /Prev {second}"), None);
    let (old, new) = (format!("startxref\n{second}\n"), format!("startxref\n{third}\n"));
    assert_eq!(old.len(), new.len());
    let at = p.d.windows(old.len()).position(|w| w == old.as_bytes()).unwrap();
    p.d[at..at + old.len()].copy_from_slice(new.as_bytes());
    untouched(&p.d);

    // The word more often than saves are taken apart, here in a string.
    let (mut p, x) = first_save();
    p.obj(6, format!("<< /Title ({}) >>", "startxref ".repeat(70)).as_bytes());
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    untouched_for(&p.d, xref::TOO_MANY, &Options::effort(0));

    // An update to a file whose first object doesn't read: another reader may still find a linearisation
    // dictionary there.
    let mut p = Pdf::new();
    p.d.extend_from_slice(b"9 0 obj\n<< /A (\nendobj\n");
    page(&mut p, "ORIGINAL SECRET TEXT");
    let x = p.table(0, 6, "/Size 6 /Root 1 0 R", None);
    info_save(&mut p, x);
    untouched(&p.d);
}

#[test]
fn a_number_with_two_rows_or_a_row_of_an_unknown_type_leaves_the_file_untouched() {
    // The update's table lists the document information (6) twice, at two places.
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save A) >>");
    let a = p.at[&6];
    p.obj(6, b"<< /Title (Second save B) >>");
    let rows = format!("6 1\n{a:010} 00000 n \n6 1\n{:010} 00000 n \n", p.at[&6]);
    table_by_hand(&mut p, &rows, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"));
    untouched(&p.d);

    // The update is a cross-reference stream (7) whose last row has the type 3.
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) >>");
    let (at6, at7) = (p.at[&6], p.d.len());
    let rows = [[1, (at6 >> 8) as u8, at6 as u8, 0], [1, (at7 >> 8) as u8, at7 as u8, 0], [3, 0, 0, 0]].concat();
    let dict = format!("/Type /XRef /Size 9 /Index [6 3] /W [1 2 1] /Root 1 0 R /Info 6 0 R /Prev {x}");
    p.obj(7, &stream(&dict, &rows));
    p.d.extend_from_slice(format!("startxref\n{at7}\n%%EOF\n").as_bytes());
    untouched(&p.d);
}

#[test]
fn saves_that_are_too_long_together_or_do_not_read_leave_the_file_untouched() {
    // Each save is read as a file of its own: 40 saves of this size are more than 16 times the file together,
    // 17 are not.
    let many = |updates: usize| {
        let (mut p, mut x) = first_save();
        for _ in 0..updates {
            x = info_save(&mut p, x);
        }
        p.d
    };
    untouched_for(&many(39), xref::TOO_MANY, &Options::effort(0));
    kept(&many(16), 17);

    // A first whole file without a /Root, then a second whole file after it: the first doesn't open on its own.
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    p.table(0, 6, "/Size 6", None);
    p.d.extend_from_slice(b"%PDF-1.5\n");
    page(&mut p, "NEW VERSION");
    p.table(0, 6, "/Size 6 /Root 1 0 R", None);
    let why = "keeps earlier revisions: a save that doesn't open on its own";
    untouched_for(&p.d, why, &Options { drop_history: true, ..Options::effort(0) });

    // The first save's content (4) has no `endstream` where its /Length says; the update writes it again.
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    p.obj(4, b"<< /Length 5000 >>\nstream\nBT ET\nendstream");
    let x = p.table(0, 6, "/Size 6 /Root 1 0 R", None);
    p.obj(4, &text("NEW VERSION"));
    p.table(4, 1, &format!("/Size 6 /Root 1 0 R /Prev {x}"), None);
    for drop_history in [false, true] {
        let r = optimize(&p.d, &Options { drop_history, ..Options::effort(0) });
        let why = "keeps earlier revisions: revision 0 of 2: unreadable object 4";
        assert!(matches!(&r.outcome, Outcome::Untouched(w) if w.starts_with(why)), "{:?}", r.outcome);
        assert_eq!(r.data, p.d);
    }
}

#[test]
fn a_trailer_without_a_key_the_save_before_it_has_leaves_the_file_untouched() {
    // PDFium and others take the document information the first trailer names when the newest names none: the
    // update's new value for it (6) would be left at the first save's.
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    p.obj(6, b"<< /Title (First save) >>");
    let x = p.table(0, 7, "/Size 7 /Root 1 0 R /Info 6 0 R", None);
    p.obj(6, b"<< /Title (Second save) >>");
    let named = p.d.len();
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Prev {x}"), None);
    untouched(&p.d);
    untouched_for(&p.d, KEEPS, &Options { drop_history: true, ..Options::effort(0) });
    // The same when the key comes after a value nested deeper than such a reader reads: it never gets to the key.
    p.d.truncate(named);
    let deep = format!("{}1{}", "<< /A ".repeat(16), " >>".repeat(16));
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Prev {x} /X {deep} /Info 6 0 R"), None);
    untouched(&p.d);
    // Not as deep, the file is kept.
    p.d.truncate(named);
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Prev {x} /X [[[1]]] /Info 6 0 R"), None);
    kept(&p.d, 2);

    // A key that names no object (as LibreOffice writes one, and other editors leave out of their updates) reads
    // the same for such a reader in every revision: the saves are kept, but the newest is not written alone.
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    let x = p.table(0, 6, "/Size 6 /Root 1 0 R /DocChecksum /0123456789ABCDEF", None);
    p.obj(6, b"<< /Title (Second save) >>");
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    kept(&p.d, 2);
    let why = "keeps earlier revisions: a trailer key that only an earlier save has";
    untouched_for(&p.d, why, &Options { drop_history: true, ..Options::effort(0) });
}

#[test]
fn a_generation_that_goes_down_leaves_the_file_untouched() {
    // PDFium keeps the row with the higher generation where the others keep the newer row.
    // The first save's document information (6) has generation 1, the update's has generation 0.
    let mut p = Pdf::new();
    page(&mut p, "ORIGINAL SECRET TEXT");
    let at6 = p.d.len();
    p.d.extend_from_slice(b"6 1 obj\n<< /Title (First save) >>\nendobj\n");
    let x = p.d.len();
    let rows: String = (1..6).map(|n| format!("{:010} 00000 n \n", p.at[&n])).collect();
    let trailer = "trailer\n<< /Size 7 /Root 1 0 R /Info 6 1 R >>";
    let table = format!("xref\n0 7\n0000000000 65535 f \n{rows}{at6:010} 00001 n \n{trailer}\nstartxref\n{x}\n%%EOF\n");
    p.d.extend_from_slice(table.as_bytes());
    p.obj(6, b"<< /Title (Second save) >>");
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    untouched(&p.d);
    untouched_for(&p.d, KEEPS, &Options { drop_history: true, ..Options::effort(0) });

    // The first save's cross-reference stream (7) lists the number as free with the generation `free`.
    let with = |free: u8| {
        let mut p = Pdf::new();
        page(&mut p, "ORIGINAL SECRET TEXT");
        let at7 = p.d.len();
        let mut rows: Vec<u8> = (1..6).flat_map(|n| [1, (p.at[&n] >> 8) as u8, p.at[&n] as u8, 0]).collect();
        rows.extend([0, 0, 0, free, 1, (at7 >> 8) as u8, at7 as u8, 0]);
        p.obj(7, &stream("/Type /XRef /Size 8 /W [1 2 1] /Index [1 7] /Root 1 0 R", &rows));
        p.d.extend_from_slice(format!("startxref\n{at7}\n%%EOF\n").as_bytes());
        p.obj(6, b"<< /Title (Second save) >>");
        p.table(6, 1, &format!("/Size 8 /Root 1 0 R /Info 6 0 R /Prev {at7}"), None);
        p.d
    };
    untouched(&with(1));
    kept(&with(0), 2);
}

#[test]
fn a_generation_beyond_16_bits_leaves_the_file_untouched() {
    // Only the low 16 bits are read here (generation 0), so the rule on generations can't be held for such a row.
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) >>");
    let (at6, t) = (p.at[&6], p.d.len());
    let trailer = format!("trailer\n<< /Size 7 /Root 1 0 R /Info 6 0 R /Prev {x} >>");
    p.d.extend_from_slice(format!("xref\n6 1\n{at6:010} 65536 n \n{trailer}\nstartxref\n{t}\n%%EOF\n").as_bytes());
    untouched(&p.d);
}

#[test]
fn a_linearisation_dictionary_that_gives_the_whole_length_leaves_the_file_untouched() {
    // Poppler, and PDFium loading the way Chrome does, take a linearised file from its first-page section when /L is
    // the file's length: they never see the update. With another /L they start from the end, like the others.
    let (mut p, first) = linearised_save(Pdf::new(), true);
    p.obj(20, b"<< /Title (Second save) >>");
    p.table(20, 1, &format!("/Size 21 /Root 1 0 R /Info 20 0 R /Prev {first}"), None);
    let old = b"/L 0 /O 3 /E 0 /N 1 /T 0 /H [0 0]";
    let at = p.d.windows(old.len()).position(|w| w == old).unwrap();
    // A real number isn't read here, so the file stays as it is whatever it says.
    for l in [p.d.len().to_string(), "1.5".to_string()] {
        let new = format!("{:<1$}", format!("/L {l}"), old.len());
        p.d[at..at + old.len()].copy_from_slice(new.as_bytes());
        untouched(&p.d);
        untouched_for(&p.d, KEEPS, &Options { drop_history: true, ..Options::effort(0) });
    }
}
