//! Files that keep earlier saves (incremental updates) are left untouched, whatever the later saves changed; one
//! save is still rewritten, a linearised hybrid file's two sections included.

use gpress::pdf::object::Obj;
use gpress::pdf::rewrite::{optimize, Options, Outcome};
use gpress::pdf::xref;
use std::collections::BTreeMap;

const KEEPS: &str = "keeps earlier revisions (incremental updates)";

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
        let x = self.d.len();
        self.d.extend_from_slice(format!("xref\n{first} {count}\n").as_bytes());
        for n in first..first + count {
            let row = match self.at.get(&n) {
                Some(off) if n > 0 => format!("{off:010} 00000 n \n"),
                _ => format!("0000000000 {:05} f \n", if n == 0 { 65535 } else { 0 }),
            };
            self.d.extend_from_slice(row.as_bytes());
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
    let r = optimize(input, &Options::effort(0));
    assert!(matches!(&r.outcome, Outcome::Untouched(w) if w == KEEPS), "{:?}", r.outcome);
    assert_eq!(r.data, input);
}

#[test]
fn a_save_that_only_adds_objects_is_a_revision() {
    // The update writes a new catalog, pages, page and content and names the new catalog: no number is defined
    // twice, so only the second section shows that the first save is still in the file.
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Type /Catalog /Pages 7 0 R >>");
    p.obj(7, b"<< /Type /Pages /Kids [8 0 R] /Count 1 >>");
    p.obj(8, b"<< /Type /Page /Parent 7 0 R /MediaBox [0 0 200 200] /Contents 9 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    p.obj(9, &text("NEW VERSION"));
    p.table(6, 4, &format!("/Size 10 /Root 6 0 R /Prev {x}"), None);
    untouched(&p.d);
}

#[test]
fn a_save_that_adds_only_document_information_is_a_revision() {
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) >>");
    p.table(6, 1, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    untouched(&p.d);
}

#[test]
fn a_save_that_lists_every_row_again_unchanged_is_a_revision() {
    let (mut p, x) = first_save();
    p.obj(6, b"<< /Title (Second save) >>");
    p.table(0, 7, &format!("/Size 7 /Root 1 0 R /Info 6 0 R /Prev {x}"), None);
    untouched(&p.d);
}

#[test]
fn a_save_that_changes_only_the_trailer_is_a_revision() {
    let (mut p, x) = first_save();
    p.table(0, 0, &format!("/Size 6 /Root 1 0 R /ID [<01> <02>] /Prev {x}"), None);
    untouched(&p.d);
}

#[test]
fn a_save_no_section_links_is_a_revision() {
    // A whole second file appended after the first, with no /Prev: the newest chain is one section, but the
    // first save is still in the bytes.
    let (mut p, _) = first_save();
    p.d.extend_from_slice(b"%PDF-1.5\n");
    page(&mut p, "NEW VERSION");
    p.table(0, 6, "/Size 6 /Root 1 0 R", None);
    untouched(&p.d);
}

/// One save, linearised and hybrid, as Acrobat writes it: the linearisation dictionary (10); the first-page
/// section, whose /XRefStm (11) gives the objects in object stream 12 (font 5 here) and whose /Prev names the main
/// section at the end; and the main section, a table that lists font 5 as free for readers without object streams.
/// Returns where the first-page section is.
fn linearised_hybrid() -> (Pdf, usize) {
    let mut p = Pdf::new();
    p.obj(10, b"<< /Linearized 1 /L 0 /O 3 /E 0 /N 1 /T 0 /H [0 0] >>");
    // Room for the first-page section, written once the offsets are known.
    let first = p.d.len();
    p.d.extend_from_slice(&[b' '; 200]);
    p.d.push(b'\n');
    p.obj(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    p.obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    p.obj(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    p.obj(4, &text("ONE SAVE"));
    p.obj(12, &stream("/Type /ObjStm /N 1 /First 4", b"5 0 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"));
    // Its one row: object 5 is entry 0 of object stream 12.
    p.obj(11, &stream("/Type /XRef /Size 13 /Index [5 1] /W [1 1 1]", &[2, 12, 0]));
    let main = p.table(0, 10, "/Size 13", Some(first));
    let mut head = b"xref\n10 3\n".to_vec();
    for n in 10..13 {
        head.extend_from_slice(format!("{:010} 00000 n \n", p.at[&n]).as_bytes());
    }
    let xs = p.at[&11];
    head.extend_from_slice(
        format!("trailer\n<< /Size 13 /Root 1 0 R /Prev {main} /XRefStm {xs} >>\nstartxref\n0\n%%EOF").as_bytes(),
    );
    assert!(head.len() <= 200, "{}", head.len());
    p.d[first..first + head.len()].copy_from_slice(&head);
    (p, first)
}

#[test]
fn one_linearised_hybrid_save_is_rewritten_with_every_object() {
    let (p, _) = linearised_hybrid();
    let doc = xref::open(&p.d).unwrap();
    assert_eq!(doc.sections, 2);
    let r = optimize(&p.d, &Options::effort(0));
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
fn an_update_to_a_linearised_file_is_a_revision() {
    let (mut p, first) = linearised_hybrid();
    p.obj(20, b"<< /Title (Second save) >>");
    p.table(20, 1, &format!("/Size 21 /Root 1 0 R /Info 20 0 R /Prev {first}"), None);
    untouched(&p.d);
}

#[test]
fn one_save_is_rewritten() {
    let (p, _) = first_save();
    assert!(matches!(optimize(&p.d, &Options::effort(0)).outcome, Outcome::Smaller(_)));
}
