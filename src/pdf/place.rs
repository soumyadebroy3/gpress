//! Where each image of a PDF is shown, for the lossy image step: the largest size an image is drawn at decides
//! how far it may be resampled, so an image is reported only when every use of it was measured.
//!
//! The walk goes down the page tree, reads each page's content for its `Do` operators and the matrix in effect
//! at each, and follows the forms they draw. A content it cannot read for certain (an inline image, a `Q`
//! with nothing saved, a filter it does not decode) is not measured, and then no entry of the /XObject
//! dictionary that served it counts (for a form, nor of the dictionaries of the contents around it).
//!
//! An image may also be used where the walk does not look: as a soft mask, in a pattern, a Type 3 font, an
//! annotation's appearance, a thumbnail, a page outside the tree; or through a resources dictionary or a form
//! that something unwalked uses too. So the references to each object are counted over the whole file, and an
//! image is reported only when every one of them is an entry of an /XObject dictionary the walk read cleanly,
//! reached only through objects for which the same holds.
//!
//! Any doubt about the walk as a whole gives no images at all: a limit or the deadline passed, a malformed
//! page tree, a form that draws itself, a size that is not finite. The limits: 20,000,000 operators and
//! 1,000,000 `Do` operators drawn, a page tree 64 deep, forms 16 deep.
//!
//! The sizes are the same on every machine: f64 with + - * / and square roots only, numbers read by hand.

use super::filters;
use super::object::{Dict, Obj, Stream};
use super::parse::Lexer;
use crate::clock::Deadline;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

/// The largest size an image is shown at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    /// The largest displayed width and height over all its placements, in inches.
    pub width: f64,
    pub height: f64,
    /// A placement covers at least 70% of its page: the image is treated as a scan.
    pub scan: bool,
}

/// The largest size each image XObject is shown at, by object number. `objs` is every reachable object of the
/// file by number (generation, value), decrypted, streams with their stored data; a content decodes to at most
/// `max_stream` bytes. An image has an entry only when the walk measured every use of it and it is drawn at
/// least once. When anything about the whole walk is in doubt the map is empty.
pub fn shown(
    objs: &BTreeMap<u32, (u16, Obj)>,
    trailer: &Dict,
    max_stream: usize,
    deadline: Deadline,
) -> BTreeMap<u32, Shown> {
    let walk = Walk {
        objs,
        max_stream,
        deadline,
        left: OPERATORS,
        dos: DOS,
        nodes: BTreeSet::new(),
        followed: BTreeMap::new(),
        xdicts: BTreeMap::new(),
        forms: BTreeMap::new(),
        stack: Vec::new(),
        page: (0.0, 1.0),
        placed: BTreeMap::new(),
    };
    walk.run(trailer).unwrap_or_default()
}

/// Operators the whole walk may read. Decoding is taken from it too (see `Walk::decoded`).
const OPERATORS: u64 = 20_000_000;

/// `Do` operators the whole walk may draw.
const DOS: usize = 1_000_000;

/// What a page-tree node passes down, a node's own value in place of the one from above.
const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"UserUnit"];

/// A matrix [a b c d e f], for row vectors.
type M = [f64; 6];

const ID: M = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// The `Do` operators of a content: the matrix in effect at each, relative to the start of the content, and
/// the name.
type Dos = Vec<(M, Vec<u8>)>;

/// An /XObject dictionary that was in effect for a content.
struct XDict<'a> {
    /// Its entries by name: a page may draw thousands of them.
    names: BTreeMap<&'a [u8], &'a Obj>,
    /// A content it served was not measured: none of its entries count.
    poisoned: bool,
    /// The objects it was reached through.
    via: BTreeSet<u32>,
}

/// m x n: m first, then n.
fn mul(m: &M, n: &M) -> M {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

fn abs(x: f64) -> f64 {
    if x < 0.0 {
        -x
    } else {
        x
    }
}

/// The value of a number. None for anything else, and for a real `real` does not read.
fn num(o: &Obj) -> Option<f64> {
    match o {
        Obj::Int(i) => Some(*i as f64),
        Obj::Real(t) => real(t),
        _ => None,
    }
}

/// A real as written: sign, digits, point, digits. Read by hand, as the library's reader is a large routine:
/// the first 18 digits make an integer, which is divided by the power of ten of the fraction digits among
/// them (exact up to there). Later fraction digits are dropped; more than 18 integer digits is not a number.
fn real(t: &[u8]) -> Option<f64> {
    let (negative, digits) = match t {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, t),
    };
    let (mut mantissa, mut kept, mut scale, mut point) = (0u64, 0, 1.0, false);
    for &c in digits {
        if c == b'.' && !point {
            point = true;
        } else if !c.is_ascii_digit() || (kept == 18 && !point) {
            return None;
        } else if kept < 18 {
            mantissa = mantissa * 10 + u64::from(c - b'0');
            kept += 1;
            if point {
                scale *= 10.0;
            }
        }
    }
    let v = mantissa as f64 / scale;
    (kept > 0).then_some(if negative { -v } else { v })
}

/// A matrix from exactly six numbers.
fn matrix(a: &[Obj]) -> Option<M> {
    let mut m = ID;
    if a.len() != 6 {
        return None;
    }
    for (v, x) in m.iter_mut().zip(a) {
        *v = num(x)?;
    }
    Some(m)
}

/// Takes `n` from the walk's budget. False when nothing is left: the walk gives up.
fn spend(left: &mut u64, n: u64) -> bool {
    *left = left.saturating_sub(n);
    *left > 0
}

/// The `Do` operators of a content, or None when it is not measured. Each operator read is taken from `left`.
fn read(data: &[u8], left: &mut u64) -> Option<Dos> {
    let mut lx = Lexer::new(data, 0);
    let (mut ctm, mut saved, mut args, mut dos) = (ID, Vec::new(), Vec::new(), Vec::new());
    loop {
        lx.skip_ws();
        let Some(c) = lx.peek() else { return Some(dos) };
        let start = lx.pos;
        let word = lx.regular_token();
        if c.is_ascii_digit() || b"+-./(<[".contains(&c) || matches!(word, b"true" | b"false" | b"null") {
            lx.pos = start;
            if args.len() == 64 {
                return None;
            }
            args.push(lx.parse_obj().ok()?);
            continue;
        }
        // An empty word is a delimiter that starts no operand.
        if word.is_empty() || !spend(left, 1) {
            return None;
        }
        match word {
            b"q" => {
                if saved.len() == 256 {
                    return None;
                }
                saved.push(ctm);
            }
            b"Q" => ctm = saved.pop()?,
            b"cm" => ctm = mul(&matrix(&args)?, &ctm),
            b"Do" => match args.as_slice() {
                [Obj::Name(name)] => {
                    // More than the whole walk may draw: nothing is left.
                    if dos.len() == DOS {
                        *left = 0;
                        return None;
                    }
                    dos.push((ctm, name.clone()));
                }
                _ => return None,
            },
            b"BI" => return None,
            _ => {}
        }
        args.clear();
    }
}

/// An address as a key: an object inside `objs` stays where it is while `objs` is borrowed.
fn addr<T>(x: &T) -> usize {
    x as *const T as usize
}

fn poison(xdicts: &mut BTreeMap<usize, XDict<'_>>, x: Option<usize>) {
    if let Some(d) = x.and_then(|x| xdicts.get_mut(&x)) {
        d.poisoned = true;
    }
}

struct Walk<'a> {
    objs: &'a BTreeMap<u32, (u16, Obj)>,
    max_stream: usize,
    deadline: Deadline,
    /// What is left of `OPERATORS`.
    left: u64,
    /// What is left of `DOS`.
    dos: usize,
    /// The page-tree nodes met.
    nodes: BTreeSet<u32>,
    /// The references followed to resources and to /XObject dictionaries, by address: the object each names,
    /// and the objects it was reached through.
    followed: BTreeMap<usize, (u32, BTreeSet<u32>)>,
    /// The /XObject dictionaries that were in effect for a content, by address. The address stands for the
    /// dictionary wherever one is passed on.
    xdicts: BTreeMap<usize, XDict<'a>>,
    /// The forms read so far: /Matrix and `Do` operators, or None for one that is not measured.
    forms: BTreeMap<u32, Option<(M, Dos)>>,
    /// The forms being drawn, outermost first, each with the /XObject dictionary in effect around it.
    stack: Vec<(u32, Option<usize>)>,
    /// The page being drawn: its area in user units, and its user unit.
    page: (f64, f64),
    /// The placements so far.
    placed: BTreeMap<u32, Shown>,
}

impl<'a> Walk<'a> {
    /// The value itself, or the object a reference names (by number, as everywhere in the engine).
    fn value(&self, o: &'a Obj) -> Option<&'a Obj> {
        match o {
            Obj::Ref(n, _) => self.objs.get(n).map(|x| &x.1),
            _ => Some(o),
        }
    }

    /// A dictionary given directly or by reference.
    fn dict(&self, o: &'a Obj) -> Option<&'a Dict> {
        match self.value(o)? {
            Obj::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// Takes the search of a dictionary from the budget: nothing for one of ordinary size, so only a file made
    /// to be slow, with a huge dictionary searched over and over, runs out. None when nothing is left.
    fn searched(&mut self, d: &Dict) -> Option<()> {
        spend(&mut self.left, d.0.len() as u64 / 64).then_some(())
    }

    /// Whether a /Resources value leads to an /XObject dictionary.
    fn has_xobjects(&mut self, resources: &'a Obj) -> bool {
        let Some(r) = self.dict(resources) else { return false };
        self.searched(r).is_some() && r.get(b"XObject").and_then(|x| self.dict(x)).is_some()
    }

    /// `dict`, noting a reference as followed, reached through `via`; its object then joins `via`, for what
    /// is reached through it.
    fn follow(&mut self, o: &'a Obj, via: &mut Vec<u32>) -> Option<&'a Dict> {
        let d = self.dict(o)?;
        if let Obj::Ref(n, _) = o {
            self.followed.entry(addr(o)).or_insert((*n, BTreeSet::new())).1.extend(via.iter());
            via.push(*n);
        }
        Some(d)
    }

    /// The /XObject dictionary of a /Resources value, from now on in effect for a content. `owner` is the form
    /// the resources belong to.
    fn xobjects(&mut self, resources: Option<&'a Obj>, owner: Option<u32>) -> Option<usize> {
        let mut via: Vec<u32> = owner.into_iter().collect();
        let r = self.follow(resources?, &mut via)?;
        self.searched(r)?;
        let x = self.follow(r.get(b"XObject")?, &mut via)?;
        let new = || XDict {
            names: x.0.iter().map(|(k, v)| (k.as_slice(), v)).collect(),
            poisoned: false,
            via: BTreeSet::new(),
        };
        self.xdicts.entry(addr(x)).or_insert_with(new).via.extend(via);
        Some(addr(x))
    }

    /// What a name stands for in /XObject dictionary `x`.
    fn named(&self, x: Option<usize>, name: &[u8]) -> Option<&'a Obj> {
        self.xdicts.get(&x?)?.names.get(name).copied()
    }

    /// A stream's data through its filters, or None when the walk does not decode it. The work is taken from
    /// the operator budget, an operator for 64 bytes, a stage that fails as one that decoded to the limit: or
    /// a small file with one large content on every page would keep a walk without a deadline busy for hours.
    fn decoded(&mut self, s: &'a Stream) -> Option<Cow<'a, [u8]>> {
        // Two lengths of vectors: the sum cannot overflow.
        if !spend(&mut self.left, (s.data.len() + s.dict.0.len()) as u64 / 64 + 1) {
            return None;
        }
        let mut data = Cow::Borrowed(s.data.as_slice());
        for st in filters::chain(&s.dict)? {
            if !filters::is_general(&st.name) {
                return None;
            }
            let out = filters::decode_stage(&st, &data, self.max_stream).ok();
            let cost = out.as_ref().map_or(self.max_stream, Vec::len);
            if !spend(&mut self.left, cost as u64 / 64 + 1) {
                return None;
            }
            let mut out = out?;
            if (st.name == b"FlateDecode" || st.name == b"LZWDecode") && filters::predictor(&st)?.0 > 1 {
                out = filters::unpredict(&st, &out).ok()?;
            }
            data = Cow::Owned(out);
        }
        (data.len() <= self.max_stream).then_some(data)
    }

    /// A page's content: its /Contents streams decoded and joined by newlines. None: not measured.
    fn contents(&mut self, page: &'a Dict) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let Some(c) = page.get(b"Contents") else { return Some(out) };
        let list = match self.value(c)? {
            Obj::Array(a) => a.as_slice(),
            _ => std::slice::from_ref(c),
        };
        for (i, part) in list.iter().enumerate() {
            let Obj::Ref(n, _) = part else { return None };
            let Some((_, Obj::Stream(s))) = self.objs.get(n) else { return None };
            let part = self.decoded(s)?;
            if i > 0 {
                // A comment left open at the end of a stream runs on into the next one for a reader that
                // joins them with a space (PDFium), not for one that joins them with a newline (pdf.js).
                if out.iter().rev().take_while(|c| !matches!(c, b'\n' | b'\r')).any(|&c| c == b'%') {
                    return None;
                }
                out.push(b'\n');
            }
            // Two lengths of vectors: the sum cannot overflow.
            if out.len() + part.len() > self.max_stream {
                return None;
            }
            out.extend_from_slice(&part);
        }
        Some(out)
    }

    /// The area of a box: four numbers.
    fn area(&self, o: Option<&'a Obj>) -> Option<f64> {
        let [x1, y1, x2, y2] = self.value(o?)?.as_array()?.as_slice() else { return None };
        Some(abs(num(x2)? - num(x1)?) * abs(num(y2)? - num(y1)?))
    }

    /// Walks page-tree node `n`, `inh` being what the nodes above pass down (see `INHERITED`).
    fn node(&mut self, n: u32, mut inh: [Option<&'a Obj>; 4], depth: usize) -> Option<()> {
        let Some((_, Obj::Dict(d))) = self.objs.get(&n) else { return None };
        if depth > 64 || self.left == 0 || !self.nodes.insert(n) {
            return None;
        }
        for (slot, key) in inh.iter_mut().zip(INHERITED) {
            let Some(own) = d.get(key).filter(|v| !matches!(v, Obj::Null)) else { continue };
            // Resources without an /XObject dictionary: a reader that fills in what a page's resources lack
            // from the nodes above (pdf.js) draws from the inherited ones, so those stay in effect.
            if key != b"Resources" || self.has_xobjects(own) {
                *slot = Some(own);
            }
        }
        let Some(kids) = d.get(b"Kids") else { return self.page(d, inh) };
        // A reader that goes by /Type may take this node for a page and draw a content the walk does not read.
        if d.has(b"Contents") {
            return None;
        }
        for kid in self.value(kids)?.as_array()? {
            let Obj::Ref(k, _) = kid else { return None };
            self.node(*k, inh, depth + 1)?;
        }
        Some(())
    }

    fn page(&mut self, d: &'a Dict, inh: [Option<&'a Obj>; 4]) -> Option<()> {
        if self.deadline.passed() {
            return None;
        }
        let x = self.xobjects(inh[0], None);
        let dos = self.contents(d).and_then(|c| read(&c, &mut self.left));
        if self.left == 0 {
            return None;
        }
        let Some(dos) = dos else {
            poison(&mut self.xdicts, x);
            return Some(());
        };
        // No valid /MediaBox leaves no area, which makes every placement a scan.
        let media = self.area(inh[1]).unwrap_or(0.0);
        let area = match self.area(inh[2]) {
            Some(crop) if crop < media => crop,
            _ => media,
        };
        let unit = match inh[3].and_then(|u| self.value(u)) {
            Some(u @ (Obj::Int(_) | Obj::Real(_))) => num(u)?,
            _ => 1.0,
        };
        self.page = (area, if unit > 1.0 { unit } else { 1.0 });
        self.draw(&dos, &ID, x)
    }

    /// Draws the `Do` operators of a measured content that starts with matrix `base` and has `x` as its
    /// /XObject dictionary.
    fn draw(&mut self, dos: &[(M, Vec<u8>)], base: &M, x: Option<usize>) -> Option<()> {
        for (m, name) in dos {
            self.dos = self.dos.checked_sub(1)?;
            let at = mul(m, base);
            let stream = match self.named(x, name) {
                Some(Obj::Ref(n, _)) => match self.objs.get(n) {
                    Some((_, Obj::Stream(s))) => Some((*n, s)),
                    _ => None,
                },
                _ => None,
            };
            if let Some((_, s)) = stream {
                self.searched(&s.dict)?;
            }
            let kind = stream.and_then(|(_, s)| self.value(s.dict.get(b"Subtype")?)?.as_name());
            match (stream, kind) {
                // Reported only for what the caller takes as an image: the subtype written in its dictionary.
                (Some((n, s)), Some(b"Image")) if s.dict.name(b"Subtype").is_some() => self.place(n, &at)?,
                (Some((n, s)), Some(b"Form")) => self.form(n, s, &at, x)?,
                // The name draws nothing here. A reader that then looks in the dictionaries of the enclosing
                // contents (xpdf and Poppler in each, PDFium in the page's) draws what one of them holds under
                // it, at a size not measured: such a dictionary is poisoned.
                _ => {
                    for &(_, outer) in &self.stack {
                        if self.named(outer, name).is_some() {
                            poison(&mut self.xdicts, outer);
                        }
                    }
                }
            }
        }
        Some(())
    }

    /// Notes that image `n` is drawn under matrix `m`.
    fn place(&mut self, n: u32, m: &M) -> Option<()> {
        let (area, unit) = self.page;
        let width = (m[0] * m[0] + m[1] * m[1]).sqrt() / 72.0 * unit;
        let height = (m[2] * m[2] + m[3] * m[3]).sqrt() / 72.0 * unit;
        let covers = abs(m[0] * m[3] - m[1] * m[2]);
        if !(width.is_finite() && height.is_finite() && covers.is_finite()) {
            return None;
        }
        let s = self.placed.entry(n).or_insert(Shown { width: 0.0, height: 0.0, scan: false });
        if width > s.width {
            s.width = width;
        }
        if height > s.height {
            s.height = height;
        }
        s.scan |= covers >= 0.7 * area;
        Some(())
    }

    /// Draws form `n` under matrix `at`, where `outer` is the /XObject dictionary in effect.
    fn form(&mut self, n: u32, s: &'a Stream, at: &M, outer: Option<usize>) -> Option<()> {
        if self.stack.len() == 16 || self.stack.iter().any(|f| f.0 == n) || self.deadline.passed() {
            return None;
        }
        let x = match s.dict.get(b"Resources") {
            None => outer,
            own => self.xobjects(own, Some(n)),
        };
        // Taken out while the form is drawn (it cannot be drawn inside itself), and put back after.
        let read = match self.forms.remove(&n) {
            Some(read) => read,
            None => self.read_form(s),
        };
        if self.left == 0 {
            return None;
        }
        match &read {
            Some((matrix, dos)) => {
                self.stack.push((n, outer));
                self.draw(dos, &mul(matrix, at), x)?;
                self.stack.pop();
            }
            // What it draws is not known: from its own dictionary or, for a reader that looks outwards, from
            // the dictionary of a content around it.
            None => {
                poison(&mut self.xdicts, x);
                poison(&mut self.xdicts, outer);
                for i in 0..self.stack.len() {
                    poison(&mut self.xdicts, self.stack[i].1);
                }
            }
        }
        self.forms.insert(n, read);
        Some(())
    }

    /// A form's /Matrix and the `Do` operators of its content. None: not measured.
    fn read_form(&mut self, s: &'a Stream) -> Option<(M, Dos)> {
        let m = match s.dict.get(b"Matrix") {
            None => ID,
            Some(Obj::Array(a)) => matrix(a)?,
            Some(_) => return None,
        };
        let data = self.decoded(s)?;
        Some((m, read(&data, &mut self.left)?))
    }

    /// The whole walk, then the reference rule: which images had every reference to them followed.
    fn run(mut self, trailer: &Dict) -> Option<BTreeMap<u32, Shown>> {
        let Obj::Ref(root, _) = trailer.get(b"Root")? else { return None };
        let Obj::Dict(catalog) = &self.objs.get(root)?.1 else { return None };
        let Obj::Ref(pages, _) = catalog.get(b"Pages")? else { return None };
        self.node(*pages, [None; 4], 0)?;

        // How many references name each object, in the whole file.
        let mut refs = Vec::new();
        self.objs.values().for_each(|(_, o)| o.refs(&mut refs));
        trailer.0.iter().for_each(|(_, v)| v.refs(&mut refs));
        let mut total: BTreeMap<u32, usize> = BTreeMap::new();
        for (n, _) in refs {
            *total.entry(n).or_default() += 1;
        }
        // The followed references, in groups reached the same way: the objects they name, and the objects
        // they were reached through. The entries of a clean /XObject dictionary are one group.
        let mut groups: Vec<(Vec<u32>, BTreeSet<u32>)> =
            self.followed.into_values().map(|(n, via)| (vec![n], via)).collect();
        for x in self.xdicts.into_values() {
            if !x.poisoned {
                let named = x.names.values().filter_map(|v| match v {
                    Obj::Ref(n, _) => Some(*n),
                    _ => None,
                });
                groups.push((named.collect(), x.via));
            }
        }
        // An object is accounted when the followed references to it are all there are. A group reached
        // through an object that is not accounted is dropped, which may leave others unaccounted.
        for _ in 0..64 {
            let mut count: BTreeMap<u32, usize> = BTreeMap::new();
            for n in groups.iter().flat_map(|g| &g.0) {
                *count.entry(*n).or_default() += 1;
            }
            let accounted = |n: &u32| count.get(n).is_some_and(|c| total.get(n) == Some(c));
            let before = groups.len();
            groups.retain(|g| g.1.iter().all(accounted));
            if groups.len() == before {
                return Some(self.placed.into_iter().filter(|(n, _)| accounted(n)).collect());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG: (u32, &str) = (1, "<</Type/Catalog/Pages 2 0 R>>");
    const PAGES: (u32, &str) = (2, "<</Type/Pages/Kids[3 0 R]>>");
    const IMAGE: &str = "<</Subtype/Image/Width 8/Height 8>> stream 12345678";
    /// Entries of a Letter page whose resources name object 10 as Im1.
    const LETTER: &str = "/MediaBox[0 0 612 792]/Resources<</XObject<</Im1 10 0 R>>>>";
    /// The same, with object 11 as Im2.
    const TWO: &str = "/MediaBox[0 0 612 792]/Resources<</XObject<</Im1 10 0 R/Im2 11 0 R>>>>";
    /// An inch square, then Im1.
    const DRAW: &str = "72 0 0 72 0 0 cm /Im1 Do";

    fn obj(text: &str) -> Obj {
        Lexer::new(text.as_bytes(), 0).parse_obj().unwrap()
    }

    fn stream(dict: &str, data: &[u8]) -> Obj {
        let Obj::Dict(dict) = obj(dict) else { panic!("{dict} is no dictionary") };
        Obj::Stream(Box::new(Stream { dict, data: data.to_vec() }))
    }

    /// Objects from text, by number. A body "dictionary stream data" is a stream.
    fn file(objects: &[(u32, &str)]) -> BTreeMap<u32, (u16, Obj)> {
        let object = |body: &str| match body.split_once(" stream ") {
            Some((dict, data)) => stream(dict, data.as_bytes()),
            None => obj(body),
        };
        objects.iter().map(|&(n, body)| (n, (0, object(body)))).collect()
    }

    fn trailer() -> Dict {
        Dict(vec![(b"Root".to_vec(), Obj::Ref(1, 0))])
    }

    fn run(objs: &BTreeMap<u32, (u16, Obj)>) -> BTreeMap<u32, Shown> {
        shown(objs, &trailer(), 1 << 20, Deadline::NONE)
    }

    fn shown_in(objects: &[(u32, &str)]) -> BTreeMap<u32, Shown> {
        run(&file(objects))
    }

    /// One page, object 3, with these entries besides its content (object 4), and more objects.
    fn page(entries: &str, content: &str, more: &[(u32, &str)]) -> BTreeMap<u32, Shown> {
        let page = format!("<</Type/Page/Contents 4 0 R{entries}>>");
        let content = format!("<<>> stream {content}");
        let mut objects = vec![CATALOG, PAGES, (3, page.as_str()), (4, content.as_str())];
        objects.extend_from_slice(more);
        shown_in(&objects)
    }

    /// Two pages: page 3 (content: object 4) names objects 10 and 12 as Im1 and Im3, page 5 draws images 11
    /// and 12 an inch square.
    fn two_pages(object10: &str, content: &str) -> BTreeMap<u32, Shown> {
        shown_in(&[
            CATALOG,
            (2, "<</Type/Pages/Kids[3 0 R 5 0 R]/MediaBox[0 0 612 792]>>"),
            (3, "<</Type/Page/Contents 4 0 R/Resources<</XObject<</Im1 10 0 R/Im3 12 0 R>>>>>>"),
            (4, content),
            (5, "<</Type/Page/Contents 6 0 R/Resources<</XObject<</Im2 11 0 R/Im3 12 0 R>>>>>>"),
            (6, "<<>> stream 72 0 0 72 0 0 cm /Im2 Do /Im3 Do"),
            (10, object10),
            (11, IMAGE),
            (12, IMAGE),
        ])
    }

    fn sh(width: f64, height: f64, scan: bool) -> Shown {
        Shown { width, height, scan }
    }

    /// Image 10 alone, at this size.
    fn one(width: f64, height: f64, scan: bool) -> BTreeMap<u32, Shown> {
        BTreeMap::from([(10, sh(width, height, scan))])
    }

    fn keys(got: &BTreeMap<u32, Shown>) -> Vec<u32> {
        got.keys().copied().collect()
    }

    #[test]
    fn placements_are_measured_in_inches() {
        let im = [(10, IMAGE)];
        assert_eq!(page(LETTER, "q 144 0 0 72 10 10 cm /Im1 Do Q", &im), one(2.0, 1.0, false));
        // The operands of other operators are read and dropped.
        let text = "BT /F1 12 Tf (Hi \\) there) Tj [(a) -20 <62>] TJ ET % note\n144 0 0 72 0 0 cm /Im1 Do";
        assert_eq!(page(LETTER, text, &im), one(2.0, 1.0, false));
        // A quarter turn.
        assert_eq!(page(LETTER, "0 144 -72 0 300 300 cm /Im1 Do", &im), one(2.0, 1.0, false));
        // Twice: each dimension keeps its largest value.
        let twice = "q 144 0 0 36 0 0 cm /Im1 Do Q q 72 0 0 216 0 0 cm /Im1 Do Q";
        assert_eq!(page(LETTER, twice, &im), one(2.0, 3.0, false));
        assert_eq!(page(&format!("{LETTER}/UserUnit 2"), "144 0 0 72 0 0 cm /Im1 Do", &im), one(4.0, 2.0, false));
        // No content at all is a clean content: the image is not drawn.
        assert!(shown_in(&[CATALOG, PAGES, (3, &format!("<</Type/Page{LETTER}>>")), (10, IMAGE)]).is_empty());
    }

    #[test]
    fn the_graphics_state_is_followed() {
        // Two cm multiply, Q restores, and a q left open at the end is fine.
        let content = "q 72 0 0 72 0 0 cm q 2 0 0 3 0 0 cm /Im1 Do Q /Im2 Do Q q";
        let got = page(TWO, content, &[(10, IMAGE), (11, IMAGE)]);
        assert_eq!(got, BTreeMap::from([(10, sh(2.0, 3.0, false)), (11, sh(1.0, 1.0, false))]));
    }

    #[test]
    fn pages_inherit_and_contents_are_joined() {
        // Resources and the MediaBox from the /Pages node.
        let got = shown_in(&[
            CATALOG,
            (2, "<</Type/Pages/Kids[3 0 R]/MediaBox[0 0 612 792]/Resources<</XObject<</Im1 10 0 R>>>>>>"),
            (3, "<</Type/Page/Contents 4 0 R>>"),
            (4, "<<>> stream 144 0 0 72 0 0 cm /Im1 Do"),
            (10, IMAGE),
        ]);
        assert_eq!(got, one(2.0, 1.0, false));
        // A page's own resources without an /XObject dictionary leave the inherited ones in effect.
        let got = shown_in(&[
            CATALOG,
            (2, "<</Type/Pages/Kids[3 0 R]/MediaBox[0 0 612 792]/Resources<</XObject<</Im1 10 0 R>>>>>>"),
            (3, "<</Type/Page/Contents 4 0 R/Resources<</Font<<>>>>>>"),
            (4, "<<>> stream 144 0 0 72 0 0 cm /Im1 Do"),
            (10, IMAGE),
        ]);
        assert_eq!(got, one(2.0, 1.0, false));
        // Two content streams, split between the operands of one operator; the array direct and by reference.
        for contents in ["[4 0 R 5 0 R]", "6 0 R"] {
            let got = shown_in(&[
                CATALOG,
                PAGES,
                (3, &format!("<</Type/Page/Contents {contents}{LETTER}>>")),
                (4, "<<>> stream q 144 0 0 72 10"),
                (5, "<<>> stream 10 cm /Im1 Do Q"),
                (6, "[4 0 R 5 0 R]"),
                (10, IMAGE),
            ]);
            assert_eq!(got, one(2.0, 1.0, false), "{contents}");
        }
        // A comment left open where two streams meet: readers differ on where it ends.
        let got = shown_in(&[
            CATALOG,
            PAGES,
            (3, &format!("<</Type/Page/Contents[4 0 R 5 0 R]{LETTER}>>")),
            (4, "<<>> stream q % 9 0 0 9 0 0 cm"),
            (5, "<<>> stream 144 0 0 72 0 0 cm /Im1 Do Q"),
            (10, IMAGE),
        ]);
        assert!(got.is_empty());
    }

    #[test]
    fn content_streams_are_decoded() {
        let text = b"144 0 0 72 0 0 cm /Im1 Do";
        let quick = crate::flate::Options::quick();
        let page = format!("<</Type/Page/Contents 4 0 R{LETTER}>>");
        let mut objs = file(&[CATALOG, PAGES, (3, &page), (10, IMAGE)]);
        objs.insert(4, (0, stream("<</Filter/FlateDecode>>", &crate::flate::deflate_zlib(text, &quick))));
        assert_eq!(run(&objs), one(2.0, 1.0, false));
        // A PNG predictor: one row, filter type 0 (none).
        let row = [&[0u8][..], &text[..]].concat();
        let dict = format!("<</Filter/FlateDecode/DecodeParms<</Predictor 12/Columns {}>>>>", text.len());
        objs.insert(4, (0, stream(&dict, &crate::flate::deflate_zlib(&row, &quick))));
        assert_eq!(run(&objs), one(2.0, 1.0, false));
        // A content longer than the limit is not measured.
        assert!(shown(&objs, &trailer(), 16, Deadline::NONE).is_empty());
        assert!(shown(&objs, &trailer(), 1 << 20, Deadline::after(std::time::Duration::ZERO)).is_empty());
    }

    #[test]
    fn forms_are_followed() {
        let fm1 = "/MediaBox[0 0 612 792]/Resources<</XObject<</Fm1 20 0 R>>>>";
        // A form with a matrix and resources of its own.
        let form = "<</Subtype/Form/Matrix[2 0 0 2 0 0]/Resources<</XObject<</Im1 10 0 R>>>>>> stream /Im1 Do";
        assert_eq!(page(fm1, "72 0 0 72 0 0 cm /Fm1 Do", &[(20, form), (10, IMAGE)]), one(2.0, 2.0, false));
        // A form in a form, the inner one with resources and an /XObject dictionary by reference.
        let nested = [
            (20, "<</Subtype/Form/Matrix[2 0 0 2 0 0]/Resources<</XObject<</Fm2 21 0 R>>>>>> stream 1 0 0 3 0 0 cm /Fm2 Do"),
            (21, "<</Subtype/Form/Resources 22 0 R>> stream /Im1 Do"),
            (22, "<</XObject 23 0 R>>"),
            (23, "<</Im1 10 0 R>>"),
            (10, IMAGE),
        ];
        assert_eq!(page(fm1, "72 0 0 72 0 0 cm /Fm1 Do", &nested), one(2.0, 6.0, false));
        // A form without resources uses the page's.
        let both = "/MediaBox[0 0 612 792]/Resources<</XObject<</Fm1 20 0 R/Im1 10 0 R>>>>";
        let bare = [(20, "<</Subtype/Form>> stream 72 0 0 36 0 0 cm /Im1 Do"), (10, IMAGE)];
        assert_eq!(page(both, "2 0 0 2 0 0 cm /Fm1 Do", &bare), one(2.0, 1.0, false));
        // A form with resources of its own that names what only the page's hold: readers differ on whether
        // it is drawn, so the page's dictionary does not count.
        let lacking = [(20, "<</Subtype/Form/Resources<<>>>> stream 9 0 0 9 0 0 cm /Im1 Do"), (10, IMAGE)];
        assert!(page(both, "72 0 0 72 0 0 cm /Im1 Do /Fm1 Do", &lacking).is_empty());
        // A form that is not measured (a /Matrix of five numbers) takes the dictionary in effect with it.
        let bad = [(20, "<</Subtype/Form/Matrix[1 0 0 1 0]>> stream /Im1 Do"), (10, IMAGE)];
        assert!(page(both, "72 0 0 72 0 0 cm /Im1 Do /Fm1 Do", &bad).is_empty());
        // The same when it has resources of its own: a reader that looks outwards could draw the page's.
        let own = "<</Subtype/Form/Matrix[1 0 0 1 0]/Resources<</XObject<<>>>>>> stream /Im1 Do";
        assert!(page(both, "72 0 0 72 0 0 cm /Im1 Do /Fm1 Do", &[(20, own), (10, IMAGE)]).is_empty());
    }

    #[test]
    fn a_placement_over_most_of_the_page_is_a_scan() {
        let scan = |entries: &str, content: &str| page(entries, content, &[(10, IMAGE)])[&10].scan;
        assert_eq!(page(LETTER, "612 0 0 792 0 0 cm /Im1 Do", &[(10, IMAGE)]), one(8.5, 11.0, true));
        // 70% of 612 x 792 is 339,292.8.
        assert!(scan(LETTER, "612 0 0 555 0 0 cm /Im1 Do"));
        assert!(!scan(LETTER, "612 0 0 554 0 0 cm /Im1 Do"));
        // The CropBox counts when it is smaller.
        assert!(!scan(LETTER, "144 0 0 200 0 0 cm /Im1 Do"));
        assert!(scan(&format!("{LETTER}/CropBox[100 100 300 300]"), "144 0 0 200 0 0 cm /Im1 Do"));
        assert!(!scan(&format!("{LETTER}/CropBox[0 0 9000 9000]"), "144 0 0 200 0 0 cm /Im1 Do"));
        // Without a usable MediaBox every placement is one.
        assert!(scan("/Resources<</XObject<</Im1 10 0 R>>>>", DRAW));
        assert!(scan("/MediaBox[0 0 612]/Resources<</XObject<</Im1 10 0 R>>>>", DRAW));
    }

    #[test]
    fn a_content_that_is_not_measured_takes_its_images_with_it() {
        // Clean: all three images.
        assert_eq!(keys(&two_pages(IMAGE, "<<>> stream 72 0 0 72 0 0 cm /Im1 Do /Im3 Do")), [10, 11, 12]);
        // Image 10 is drawn only by the bad page and 12 by both: only 11 is left.
        for bad in [
            "<<>> stream BI /W 1 /H 1 /BPC 8 /CS /G ID x EI /Im1 Do /Im3 Do",
            "<<>> stream /Im1 Do Q /Im3 Do",
            "<<>> stream 1 0 0 1 0 cm /Im1 Do /Im3 Do",
            "<<>> stream 1 /Im1 Do /Im3 Do",
            "<<>> stream /Im1 Do /Im3 Do ]",
            "<</Filter/FlateDecode>> stream not zlib data",
            "<</Filter/DCTDecode>> stream /Im1 Do /Im3 Do",
        ] {
            assert_eq!(two_pages(IMAGE, bad), BTreeMap::from([(11, sh(1.0, 1.0, false))]), "{bad}");
        }
        // 256 saved states and 64 operands are read, one more is not.
        let im = [(10, IMAGE)];
        let saved = |n: usize| page(LETTER, &format!("{}{DRAW}", "q ".repeat(n)), &im);
        assert_eq!(saved(256), one(1.0, 1.0, false));
        assert!(saved(257).is_empty());
        let operands = |n: usize| page(LETTER, &format!("{}n {DRAW}", "0 ".repeat(n)), &im);
        assert_eq!(operands(64), one(1.0, 1.0, false));
        assert!(operands(65).is_empty());
    }

    #[test]
    fn an_image_used_where_the_walk_does_not_look_is_left_out() {
        let draw2 = "72 0 0 72 0 0 cm /Im1 Do /Im2 Do";
        assert_eq!(keys(&page(TWO, draw2, &[(10, IMAGE), (11, IMAGE)])), [10, 11]);
        // In the resources, never drawn.
        assert_eq!(keys(&page(TWO, DRAW, &[(10, IMAGE), (11, IMAGE)])), [10]);
        // Image 10 is also the soft mask of image 11.
        let masked = [(10, IMAGE), (11, "<</Subtype/Image/SMask 10 0 R>> stream x")];
        assert_eq!(keys(&page(TWO, draw2, &masked)), [11]);
        // Image 10 is also in the resources of an annotation's appearance, which no page draws.
        let annotated = [
            (10, IMAGE),
            (11, IMAGE),
            (30, "<</Type/Annot/AP<</N 31 0 R>>>>"),
            (31, "<</Subtype/Form/Resources<</XObject<</A 10 0 R>>>>>> stream /A Do"),
        ];
        assert_eq!(keys(&page(&format!("{TWO}/Annots[30 0 R]"), draw2, &annotated)), [11]);
        // Image 10 is also in the resources of a page outside the page tree.
        let stray = [(10, IMAGE), (40, "<</Type/Page/Resources<</XObject<</X 10 0 R>>>>>>")];
        assert!(page(LETTER, DRAW, &stray).is_empty());
    }

    #[test]
    fn what_an_unwalked_object_shares_is_left_out() {
        // A resources object by reference: fine alone, not when an appearance stream uses it too.
        let shared = [
            (7, "<</XObject<</Im1 10 0 R>>>>"),
            (10, IMAGE),
            (30, "<</Type/Annot/AP<</N 31 0 R>>>>"),
            (31, "<</Subtype/Form/Resources 7 0 R>> stream /Im1 Do"),
        ];
        let entries = "/MediaBox[0 0 612 792]/Resources 7 0 R";
        assert_eq!(page(entries, DRAW, &shared[..2]), one(1.0, 1.0, false));
        assert!(page(&format!("{entries}/Annots[30 0 R]"), DRAW, &shared).is_empty());
        // A form an annotation uses as its appearance too: the image in the form's resources is left out,
        // the one the page draws itself is not.
        let form = [
            (10, IMAGE),
            (11, IMAGE),
            (20, "<</Subtype/Form/Resources<</XObject<</Im1 10 0 R>>>>>> stream /Im1 Do"),
            (30, "<</Type/Annot/AP<</N 20 0 R>>>>"),
        ];
        let entries = "/MediaBox[0 0 612 792]/Resources<</XObject<</Fm1 20 0 R/Im2 11 0 R>>>>";
        let draw = "72 0 0 72 0 0 cm /Fm1 Do /Im2 Do";
        assert_eq!(keys(&page(entries, draw, &form[..3])), [10, 11]);
        assert_eq!(keys(&page(&format!("{entries}/Annots[30 0 R]"), draw, &form)), [11]);
    }

    /// A page drawing a chain of `k` forms, objects 20 on, each inside the one before; the last draws image 10.
    fn nested(k: u32) -> BTreeMap<u32, Shown> {
        let page = (3, "<</Type/Page/Contents 4 0 R/Resources<</XObject<</F 20 0 R>>>>>>");
        let mut objs = file(&[CATALOG, PAGES, page, (4, "<<>> stream 72 0 0 72 0 0 cm /F Do"), (10, IMAGE)]);
        for i in 0..k {
            let next = if i + 1 == k { 10 } else { 21 + i };
            let dict = format!("<</Subtype/Form/Resources<</XObject<</F {next} 0 R>>>>>>");
            objs.insert(20 + i, (0, stream(&dict, b"/F Do")));
        }
        run(&objs)
    }

    /// A page tree of `k` nodes, objects 100 on, each the only kid of the one before, above page 3.
    fn deep(k: u32) -> BTreeMap<u32, Shown> {
        let page = format!("<</Type/Page/Contents 4 0 R{LETTER}>>");
        let mut objs =
            file(&[(1, "<</Pages 100 0 R>>"), (3, &page), (4, "<<>> stream 72 0 0 72 0 0 cm /Im1 Do"), (10, IMAGE)]);
        for i in 0..k {
            let kid = if i + 1 == k { 3 } else { 101 + i };
            objs.insert(100 + i, (0, obj(&format!("<</Kids[{kid} 0 R]>>"))));
        }
        run(&objs)
    }

    #[test]
    fn a_walk_in_doubt_gives_nothing() {
        // No /Root, a catalog without /Pages, a /Pages that is no dictionary.
        assert!(shown(&file(&[CATALOG, PAGES]), &Dict::default(), 1 << 20, Deadline::NONE).is_empty());
        assert!(shown_in(&[(1, "<</Type/Catalog>>")]).is_empty());
        assert!(shown_in(&[CATALOG, (2, IMAGE)]).is_empty());
        // A good page beside each bad tree: still nothing.
        let good = format!("<</Type/Page/Contents 4 0 R{LETTER}>>");
        for bad in [
            "<</Kids[3 0 R 1 2]>>",
            "<</Kids[3 0 R 2 0 R]>>",
            "<</Kids[3 0 R 3 0 R]>>",
            "<</Kids[3 0 R 5 0 R]>>",
            "<</Kids[3 0 R 10 0 R]>>",
            "<</Kids[3 0 R 99 0 R]>>",
            "<</Kids 7>>",
            "<</Kids[3 0 R]/Contents 4 0 R>>",
        ] {
            let got = shown_in(&[
                CATALOG,
                (2, bad),
                (3, &good),
                (4, "<<>> stream 72 0 0 72 0 0 cm /Im1 Do"),
                (5, "<</Kids[2 0 R]>>"),
                (10, IMAGE),
            ]);
            assert!(got.is_empty(), "{bad}");
        }
        assert_eq!(deep(64), one(1.0, 1.0, false));
        assert!(deep(65).is_empty());
        // A form that draws itself, alone or through another; forms 17 deep.
        assert!(two_pages("<</Subtype/Form>> stream /Im1 Do", "<<>> stream /Im1 Do").is_empty());
        let other = "<</Subtype/Form/Resources<</XObject<</Back 10 0 R>>>>>> stream /Back Do";
        let there = "<</Subtype/Form/Resources<</XObject<</On 13 0 R>>>>>> stream /On Do";
        assert!(page("/Resources<</XObject<</Im1 10 0 R>>>>", "/Im1 Do", &[(10, there), (13, other)]).is_empty());
        assert_eq!(nested(16), one(1.0, 1.0, true));
        assert!(nested(17).is_empty());
        // A size that is not finite: twenty cm of 10^17 each.
        let huge = format!("<<>> stream {}/Im1 Do", "100000000000000000 0 0 100000000000000000 0 0 cm ".repeat(20));
        assert!(two_pages(IMAGE, &huge).is_empty());
        // More Do operators than the walk may draw: a form of 1001 drawn 1000 times.
        let many = |n: usize| {
            let form =
                format!("<</Subtype/Form/Resources<</XObject<</I 11 0 R>>>>>> stream {}", "/I Do ".repeat(n + 1));
            keys(&two_pages(&form, &format!("<<>> stream {}", "/Im1 Do ".repeat(n))))
        };
        assert_eq!(many(10), [11, 12]);
        assert!(many(1000).is_empty());
    }

    #[test]
    fn numbers_are_read_by_hand() {
        assert_eq!(real(b"1.5"), Some(1.5));
        assert_eq!(real(b"-.5"), Some(-0.5));
        assert_eq!(real(b"+3."), Some(3.0));
        assert_eq!(real(b"0012.250"), Some(12.25));
        assert_eq!(real(b"123456789012345678901234567890"), None);
        // Fraction digits after the 18th digit are dropped.
        assert_eq!(real(b"0.5000000000000000000000001"), Some(0.5));
        assert_eq!(real(b"123456789012345678.75"), Some(123456789012345678u64 as f64));
        for bad in ["", "-", ".", "1.2.3", "1e5", "--1"] {
            assert_eq!(real(bad.as_bytes()), None, "{bad}");
        }
        assert_eq!(num(&obj("-7")), Some(-7.0));
        assert_eq!(num(&obj("123456789012345678901234567890")), None);
        assert_eq!(num(&obj("/Name")), None);
    }
}
