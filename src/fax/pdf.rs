//! The PDF side of fax images: CCITTFaxDecode parameters, JBIG2 instead of CCITT in normal PDFs, and the
//! pixel comparison the self-check uses for such streams.
//!
//! A CCITT image is replaced only when nothing about it is uncertain: one CCITTFaxDecode filter with
//! direct parameters, a DeviceGray image or a stencil mask, the width and height the data describes, a size
//! every JBIG2 decoder can hold, no byte alignment (readers disagree on it), EOL codes where every reader
//! expects them, every row decoded and written back bit for bit, the data ending with end codes or only 0
//! bits, coding work in proportion to the data, and the JBIG2 stream at least 5% smaller (the rewrite's rule
//! `jbig2_min_saving`) and decoding (with our decoder) to the same pixels. JBIG2Decode gives 1 for white, CCITTFaxDecode gives 0 for black unless
//! BlackIs1: the JBIG2 bitmap is chosen so the filter's output is the same bits, and every other entry of the
//! image dictionary (Decode, ImageMask, masks) keeps its meaning.
//!
//! A 1-bit image stored without an image codec (general filters only, or none) is held to the same rules where
//! they apply: a DeviceGray image or a stencil mask, direct width and height that the data fills exactly (whole
//! rows, each padded to a byte), the same size and work bounds, and the JBIG2 stream at least 5% smaller than
//! the best other encoding of the stream and decoding to the same pixels. The padding bits of a row are not
//! pixels and are not compared.

use super::ccitt::{self, Image, Limits, Parsed, Scheme, Stop, MAX_COLUMNS};

/// Most work (`Image::work`) for one image: keeps the time bounded for huge images.
const MAX_WORK: u64 = 1 << 30;
/// Most work for one image per byte of its CCITT data, plus a small allowance: a few bytes of CCITT data can
/// describe a huge image, and a file can hold many, so this bounds the time a file takes by its size. Scans
/// in the test files need at most 270 per byte.
const WORK_PER_BYTE: u64 = 2048;
const WORK_BASE: u64 = 1 << 16;
/// Largest image made JBIG2, in pixels with the width rounded up to 32. A JBIG2 decoder holds the whole page
/// in memory (PDFium twice), where CCITT is decoded row by row; PDFium (and pdf.js, which uses PDFium's
/// decoder) shows nothing for pages above 2^31 - 32 such pixels. Half of that leaves a margin.
const MAX_JBIG2_PIXELS: u64 = 1 << 30;
/// Most changing elements of one image (memory for the decoded rows: 4 bytes each).
const MAX_CHANGES: usize = 1 << 24;
use super::jbig2;
use crate::pdf::filters::chain;
use crate::pdf::object::{Dict, Obj, Stream};

/// The parameters of a CCITTFaxDecode filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaxParams {
    pub k: i64,
    pub columns: usize,
    /// 0 when not given.
    pub rows: usize,
    pub byte_align: bool,
    pub end_of_line: bool,
    pub end_of_block: bool,
    pub black_is_1: bool,
}

impl FaxParams {
    /// From a filter's DecodeParms (None: all defaults). None if a value is not a direct one of the right type.
    pub fn from(parms: Option<&Dict>) -> Option<FaxParams> {
        let get = |k: &[u8]| parms.and_then(|p| p.get(k)).filter(|v| **v != Obj::Null);
        let int = |k: &[u8], d: i64| -> Option<i64> {
            match get(k) {
                None => Some(d),
                Some(Obj::Int(i)) => Some(*i),
                _ => None,
            }
        };
        let flag = |k: &[u8], d: bool| -> Option<bool> {
            match get(k) {
                None => Some(d),
                Some(Obj::Bool(b)) => Some(*b),
                _ => None,
            }
        };
        let columns = int(b"Columns", 1728)?;
        let rows = int(b"Rows", 0)?;
        int(b"DamagedRowsBeforeError", 0)?;
        if !(1..=MAX_COLUMNS as i64).contains(&columns) || !(0..=1 << 20).contains(&rows) {
            return None;
        }
        Some(FaxParams {
            k: int(b"K", 0)?,
            columns: columns as usize,
            rows: rows as usize,
            byte_align: flag(b"EncodedByteAlign", false)?,
            end_of_line: flag(b"EndOfLine", false)?,
            end_of_block: flag(b"EndOfBlock", true)?,
            black_is_1: flag(b"BlackIs1", false)?,
        })
    }
}

/// Follows a reference with the document's objects.
pub type Resolve<'a> = &'a dyn Fn(&Obj) -> Option<Obj>;

/// An image dictionary of the two kinds written as JBIG2: a 1-bit DeviceGray image or a stencil mask.
pub fn one_bit(d: &Dict) -> Result<(), &'static str> {
    if d.name(b"Subtype") != Some(b"Image") {
        return Err("not an image");
    }
    let mask = d.get(b"ImageMask") == Some(&Obj::Bool(true));
    let one_bit = d.get(b"BitsPerComponent") == Some(&Obj::Int(1));
    if mask {
        if !(one_bit || d.get(b"BitsPerComponent").is_none()) {
            return Err("mask with odd BitsPerComponent");
        }
    } else if !(one_bit && d.name(b"ColorSpace") == Some(b"DeviceGray")) {
        return Err("not a 1-bit DeviceGray image or a mask");
    }
    Ok(())
}

/// The parameters of a stream whose only filter is CCITTFaxDecode, and its /Height (Width and Height may be
/// references to numbers).
fn lone_fax(s: &Stream, resolve: Resolve) -> Result<(FaxParams, usize), &'static str> {
    let stages = chain(&s.dict).ok_or("filter chain unreadable")?;
    if stages.len() != 1 || stages[0].name != b"CCITTFaxDecode" {
        return Err("not a lone CCITTFaxDecode filter");
    }
    let d = &s.dict;
    one_bit(d)?;
    let p = FaxParams::from(stages[0].parms.as_ref()).ok_or("parameters not direct values")?;
    let int = |k: &[u8]| d.get(k).and_then(resolve).and_then(|x| x.as_int());
    let w = int(b"Width").ok_or("no Width")?;
    let h = int(b"Height").ok_or("no Height")?;
    if w != p.columns as i64 {
        return Err("Width differs from Columns");
    }
    fits(w, h)?;
    Ok((p, h as usize))
}

/// A height in range, and an image of `w` columns (in range) and `h` rows that every JBIG2 decoder can hold.
fn fits(w: i64, h: i64) -> Result<(), &'static str> {
    if !(1..=1 << 20).contains(&h) {
        return Err("Height out of range");
    }
    if (w as u64).next_multiple_of(32) * h as u64 > MAX_JBIG2_PIXELS {
        return Err("too large for some JBIG2 decoders");
    }
    Ok(())
}

/// Is coding this image (`data_len` bytes of CCITT data) within the work bounds?
fn affordable(img: &Image, data_len: usize) -> bool {
    let work = img.work();
    work <= MAX_WORK && work <= WORK_BASE + WORK_PER_BYTE * data_len as u64
}

/// Decodes a CCITT stream and accepts it only if every reader must see the same image in it: `h` rows,
/// exactly what the data holds, each written back bit for bit (see the module notes).
fn clean_parse(data: &[u8], p: &FaxParams, h: usize) -> Result<Parsed, &'static str> {
    if p.byte_align {
        return Err("byte-aligned rows");
    }
    if p.rows != 0 && p.rows != h {
        return Err("Rows differs from Height");
    }
    if !p.end_of_block && p.rows == 0 {
        return Err("no end of block and no Rows");
    }
    let scheme = Scheme::from_k(p.k);
    let limits = Limits { max_rows: h, max_changes: MAX_CHANGES, ..Limits::default() };
    let parsed = ccitt::parse(data, scheme, p.columns, false, limits).ok_or("bad parameters")?;
    if parsed.image.rows() != h {
        return Err("rows decoded differ from Height");
    }
    let l = &parsed.layout;
    match parsed.stop {
        // After real end codes (EOFB, RTC) readers stop; what follows is not image data.
        Stop::EndCodes if l.end.len() >= 2 => {}
        // One EOL, or nothing, and then only 0 bits.
        Stop::EndCodes | Stop::EndOfData => {
            if l.pad != 0 || l.trailing.iter().any(|&b| b != 0) {
                return Err("data after the last row");
            }
        }
        // More rows than the height, or data that isn't rows.
        Stop::Limit | Stop::Undecodable => return Err("data after the last row"),
    }
    // With EndOfLine, Poppler and xpdf look for an EOL after every row and skip whatever comes before it; they
    // also switch EndOfLine on when the data starts with an EOL. PDFium and libtiff don't.
    match scheme {
        // Group 4 rows are never preceded by EOL codes or fill bits, so with EndOfLine Poppler would show only
        // the first row.
        Scheme::G4 => {
            if p.end_of_line {
                return Err("EndOfLine set for Group 4 data");
            }
            if l.gaps.iter().any(|g| g.eol || g.fill != 0) {
                return Err("EOL codes in Group 4 data");
            }
        }
        // Then every row after the first must have its EOL.
        Scheme::G31D | Scheme::G32D => {
            let eol_mode = p.end_of_line || l.gaps.first().is_some_and(|g| g.eol);
            if eol_mode && l.gaps.iter().skip(1).any(|g| !g.eol) {
                return Err("a row without EOL after EndOfLine or a leading EOL");
            }
        }
    }
    Ok(parsed)
}

/// `min_saving`: the percentage the JBIG2 data must save on the fax data (and it must save a byte).
fn decide(s: &Stream, resolve: Resolve, min_saving: u64) -> Result<(Dict, Vec<u8>), &'static str> {
    let (p, h) = lone_fax(s, resolve)?;
    let parsed = clean_parse(&s.data, &p, h)?;
    if !affordable(&parsed.image, s.data.len()) {
        return Err("too much work for the size of the data");
    }
    as_jbig2(s, &parsed.image, p.black_is_1, s.data.len(), min_saving)
}

/// The image of stream `s` as a JBIG2 stream of the bitmap `img` (its inverse if asked), when that is at least
/// `min_saving` percent (5 by default) and a byte smaller than `best` bytes: (the new dictionary, the new data).
fn as_jbig2(
    s: &Stream,
    img: &Image,
    invert: bool,
    best: usize,
    min_saving: u64,
) -> Result<(Dict, Vec<u8>), &'static str> {
    let out = jbig2::pdf_stream(img, invert);
    if out.len() as u64 * 100 > best as u64 * 100u64.saturating_sub(min_saving) || out.len() >= best {
        return Err("JBIG2 not smaller by the percentage asked");
    }
    // Our JBIG2 decoder must give the very same pixels back.
    if jbig2::read_pdf_stream(&out, invert, img.changes.len()).as_ref() != Some(img) {
        return Err("JBIG2 does not decode to the same pixels");
    }
    let mut d = s.dict.clone();
    d.set(b"Filter", Obj::Name(b"JBIG2Decode".to_vec()));
    d.remove(b"DecodeParms");
    Ok((d, out))
}

/// A stream as `as_jbig2` writes it: JBIG2Decode alone, without parameters.
fn lone_jbig2(d: &Dict) -> bool {
    matches!(chain(d).as_deref(), Some([st]) if st.name == b"JBIG2Decode" && st.parms.is_none())
}

/// A lone CCITTFaxDecode image as JBIG2 when that is at least `min_saving` percent smaller (5 by default) and
/// every check passes: (the new dictionary, the new data). None leaves the stream to the other candidates.
pub fn jbig2_candidate(s: &Stream, resolve: Resolve, min_saving: u64) -> Option<(Dict, Vec<u8>)> {
    decide(s, resolve, min_saving).ok()
}

/// What `jbig2_candidate` does with a stream, in words (for measuring).
pub fn explain(s: &Stream, resolve: Resolve) -> String {
    use crate::pdf::rules::{RewriteRules, Rule};
    match decide(s, resolve, RewriteRules::default_of(Rule::jbig2_min_saving)) {
        Ok((_, out)) => format!("JBIG2 {} -> {} bytes", s.data.len(), out.len()),
        Err(why) => format!("kept: {why}"),
    }
}

/// For the self-check: `a` is a lone CCITT image and `b` the JBIG2 stream `jbig2_candidate` makes of it, and
/// the two filters give the same data (decoded with our decoders).
pub fn same_image(a: &Stream, b: &Stream, resolve: Resolve) -> bool {
    let (p, h) = match lone_fax(a, resolve) {
        Ok(x) => x,
        Err(_) => return false,
    };
    if !lone_jbig2(&b.dict) {
        return false;
    }
    let parsed = match clean_parse(&a.data, &p, h) {
        Ok(x) => x,
        Err(_) => return false,
    };
    affordable(&parsed.image, a.data.len())
        && jbig2::read_pdf_stream(&b.data, p.black_is_1, parsed.image.changes.len()).as_ref() == Some(&parsed.image)
}

/// The pixels of a 1-bit image (`one_bit`) whose filters are general ones only: `bits` is what those filters
/// give, a predictor undone, and must be exactly the rows its direct /Width and /Height describe, each padded
/// to a byte. A pixel is its bit as it is (the padding bits are left out). The size and work bounds are the fax
/// path's, the work and the number of changing elements counted against the stored bytes. None: not such an
/// image, or out of bounds.
fn bilevel(s: &Stream, bits: &[u8]) -> Option<Image> {
    let d = &s.dict;
    one_bit(d).ok()?;
    let (w, h) = (d.get(b"Width")?.as_int()?, d.get(b"Height")?.as_int()?);
    if !(1..=MAX_COLUMNS as i64).contains(&w) || fits(w, h).is_err() {
        return None;
    }
    let (w, row) = (w as usize, (w as usize).div_ceil(8));
    if bits.len() as u64 != row as u64 * h as u64 {
        return None;
    }
    // General filters can store millions of changing elements in a few bytes, so their list is held to the
    // stored bytes as a fax image's holds itself (its data spends a bit or more on each): 8 for a byte, over
    // the allowance a small stream has.
    let most = MAX_CHANGES.min((WORK_BASE as usize).saturating_add(s.data.len().saturating_mul(8)));
    let mut img = Image::new(w);
    let (mut px, mut ch) = (vec![0u8; w], Vec::new());
    let mut above: &[u8] = &[];
    for r in bits.chunks_exact(row) {
        // A row of the same bytes as the row above has its changes.
        if r != above {
            for (x, p) in px.iter_mut().enumerate() {
                *p = r[x / 8] >> (7 - x % 8) & 1;
            }
            ccitt::row_changes(&px, &mut ch);
            above = r;
        }
        if img.changes.len() + ch.len() > most {
            return None;
        }
        img.push_row(&ch);
    }
    affordable(&img, s.data.len()).then_some(img)
}

/// A 1-bit image whose filters are general ones only, as JBIG2, when that is at least `min_saving` percent
/// smaller (5 by default) than `best` (the bytes of the best other encoding of the stream) and every check passes: (the new dictionary, the new
/// data). `bits`: what the filters give, a predictor undone. JBIG2Decode gives 1 for a 0 of the bitmap, so the
/// bitmap is the inverse of the bits and the filter gives the same bits back. None leaves the stream to the
/// other candidates.
pub fn bilevel_candidate(s: &Stream, bits: &[u8], best: usize, min_saving: u64) -> Option<(Dict, Vec<u8>)> {
    as_jbig2(s, &bilevel(s, bits)?, true, best, min_saving).ok()
}

/// For the self-check: `a` is a 1-bit image whose filters, general ones only, give `bits` (a predictor undone),
/// `b` is the JBIG2 stream `bilevel_candidate` makes of it, and JBIG2Decode gives the same pixels (decoded with
/// our decoder).
pub fn same_bits(a: &Stream, bits: &[u8], b: &Stream) -> bool {
    lone_jbig2(&b.dict)
        && bilevel(a, bits)
            .is_some_and(|img| jbig2::read_pdf_stream(&b.data, true, img.changes.len()).as_ref() == Some(&img))
}
