//! The PDF side of fax images: CCITTFaxDecode parameters, JBIG2 instead of CCITT in normal PDFs, and the
//! pixel comparison the self-check uses for such streams.
//!
//! A CCITT image is replaced only when nothing about it is uncertain: one CCITTFaxDecode filter with
//! direct parameters, a DeviceGray image or a stencil mask, the width and height the data describes, no byte
//! alignment (readers disagree on it), every row decoded and written back bit for bit, the data ending
//! with end codes or only 0 bits, and the JBIG2 stream at least 5% smaller and decoding (with our decoder) to
//! the same pixels. JBIG2Decode gives 1 for white, CCITTFaxDecode gives 0 for black unless BlackIs1: the
//! JBIG2 bitmap is chosen so the filter's output is the same bits, and every other entry of the image
//! dictionary (Decode, ImageMask, masks) keeps its meaning.

use super::ccitt::{self, Limits, Parsed, Scheme, Stop, MAX_COLUMNS};
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

/// The parameters of a stream whose only filter is CCITTFaxDecode, and its /Height (Width and Height may be
/// references to numbers).
fn lone_fax(s: &Stream, resolve: Resolve) -> Result<(FaxParams, usize), &'static str> {
    let stages = chain(&s.dict).ok_or("filter chain unreadable")?;
    if stages.len() != 1 || stages[0].name != b"CCITTFaxDecode" {
        return Err("not a lone CCITTFaxDecode filter");
    }
    let d = &s.dict;
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
    let p = FaxParams::from(stages[0].parms.as_ref()).ok_or("parameters not direct values")?;
    let int = |k: &[u8]| d.get(k).and_then(|x| resolve(x)).and_then(|x| x.as_int());
    let w = int(b"Width").ok_or("no Width")?;
    let h = int(b"Height").ok_or("no Height")?;
    if w != p.columns as i64 {
        return Err("Width differs from Columns");
    }
    if !(1..=1 << 20).contains(&h) {
        return Err("Height out of range");
    }
    Ok((p, h as usize))
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
    let parsed = ccitt::parse(data, scheme, p.columns, false, Limits { max_rows: h, ..Limits::default() }).ok_or("bad parameters")?;
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
    match scheme {
        // Group 4 rows are never preceded by EOL codes or fill bits.
        Scheme::G4 => {
            if l.gaps.iter().any(|g| g.eol || g.fill != 0) {
                return Err("EOL codes in Group 4 data");
            }
        }
        // With EndOfLine, readers look for an EOL before every row after the first.
        Scheme::G31D | Scheme::G32D => {
            if p.end_of_line && l.gaps.iter().skip(1).any(|g| !g.eol) {
                return Err("a row without EOL although EndOfLine is set");
            }
        }
    }
    Ok(parsed)
}

fn decide(s: &Stream, resolve: Resolve) -> Result<(Dict, Vec<u8>), &'static str> {
    let (p, h) = lone_fax(s, resolve)?;
    let parsed = clean_parse(&s.data, &p, h)?;
    let out = jbig2::pdf_stream(&parsed.image, p.black_is_1);
    if out.len() * 100 > s.data.len() * 95 {
        return Err("JBIG2 not at least 5% smaller");
    }
    // Our JBIG2 decoder must give the very same pixels back.
    if jbig2::read_pdf_stream(&out, p.black_is_1).as_ref() != Some(&parsed.image) {
        return Err("JBIG2 does not decode to the same pixels");
    }
    let mut d = s.dict.clone();
    d.set(b"Filter", Obj::Name(b"JBIG2Decode".to_vec()));
    d.remove(b"DecodeParms");
    Ok((d, out))
}

/// A lone CCITTFaxDecode image as JBIG2 when that is at least 5% smaller and every check passes: (the new
/// dictionary, the new data). None leaves the stream to the other candidates.
pub fn jbig2_candidate(s: &Stream, resolve: Resolve) -> Option<(Dict, Vec<u8>)> {
    decide(s, resolve).ok()
}

/// What `jbig2_candidate` does with a stream, in words (for measuring).
pub fn explain(s: &Stream, resolve: Resolve) -> String {
    match decide(s, resolve) {
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
    match chain(&b.dict) {
        Some(st) if st.len() == 1 && st[0].name == b"JBIG2Decode" && st[0].parms.is_none() => {}
        _ => return false,
    }
    let parsed = match clean_parse(&a.data, &p, h) {
        Ok(x) => x,
        Err(_) => return false,
    };
    jbig2::read_pdf_stream(&b.data, p.black_is_1).as_ref() == Some(&parsed.image)
}
