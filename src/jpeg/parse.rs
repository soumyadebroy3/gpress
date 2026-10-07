//! The marker structure of a JPEG file, read strictly. Only what the Huffman re-encoding needs is
//! interpreted; every other byte is kept as a range of the input.

use super::huffman::{DecodeTable, Spec};
use std::ops::Range;

pub type Fail = &'static str;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Component {
    pub id: u8,
    pub h: u8,
    pub v: u8,
    /// Quantisation table number.
    pub tq: u8,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub comps: Vec<Component>,
    pub hmax: u32,
    pub vmax: u32,
}

/// One scan: its header, its entropy-coded data and what decoding it needs.
#[derive(Clone, Debug)]
pub struct Scan {
    /// The SOS segment (with any fill bytes before its marker).
    pub header: Range<usize>,
    /// The entropy-coded data with its restart markers, up to the marker that follows.
    pub data: Range<usize>,
    /// For each component of the scan: (index in the frame, DC table, AC table).
    pub comps: Vec<(usize, u8, u8)>,
    /// Restart interval in MCUs (0: none).
    pub restart: u32,
    /// The Huffman tables this scan uses, by slot (DC 0-3, AC 4-7), as indexes into `Jpeg::tables`.
    pub tables: [Option<usize>; 8],
}

/// The file in order: kept bytes, Huffman table segments (replaced when re-encoding) and scans.
#[derive(Clone, Debug)]
pub enum Piece {
    Keep(Range<usize>),
    Tables,
    Scan(usize),
}

pub struct Jpeg<'a> {
    pub data: &'a [u8],
    pub frame: Frame,
    pub pieces: Vec<Piece>,
    pub scans: Vec<Scan>,
    pub tables: Vec<DecodeTable>,
}

/// Most Huffman table definitions in one file (a sequential file needs at most 8 per scan, 4 scans).
const MAX_TABLES: usize = 64;

/// Most segments in one file. Real files have a few dozen (an ICC profile takes at most 255); the limit keeps
/// the memory for a file made of millions of tiny segments small.
const MAX_SEGMENTS: usize = 4096;

/// Reads a baseline or extended sequential Huffman JPEG with 8-bit samples. Anything else, and anything
/// unusual, is refused with the reason.
pub fn parse(d: &[u8]) -> Result<Jpeg<'_>, Fail> {
    if d.len() < 4 || d[0] != 0xFF || d[1] != 0xD8 {
        return Err("no start-of-image marker at the start");
    }
    let mut pieces = vec![Piece::Keep(0..2)];
    let mut frame: Option<Frame> = None;
    let mut scans: Vec<Scan> = Vec::new();
    let mut tables: Vec<DecodeTable> = Vec::new();
    let mut slots: [Option<usize>; 8] = [None; 8];
    // Quantisation tables defined so far, and those a scan has used.
    let (mut qdefined, mut qused) = ([false; 4], [false; 4]);
    let mut restart = 0u32;
    let mut pos = 2;
    loop {
        let start = pos;
        if pos >= d.len() {
            return Err("no end-of-image marker");
        }
        if d[pos] != 0xFF {
            return Err("bytes between segments");
        }
        pos += 1;
        // Fill bytes (more 0xFF before a marker) are allowed, but readers differ on them next to segments
        // that move: pdf.js takes 0xFFFF right after an application segment for a wrong length and searches
        // that segment for a marker. So files with them are left as they are.
        match d.get(pos) {
            None => return Err("no end-of-image marker"),
            Some(0xFF) => return Err("fill bytes before a marker"),
            Some(_) => {}
        }
        if pieces.len() >= MAX_SEGMENTS {
            return Err("too many segments");
        }
        let m = d[pos];
        pos += 1;
        if m == 0xD9 {
            // End of image, and whatever follows it, kept as it is.
            pieces.push(Piece::Keep(start..d.len()));
            break;
        }
        match m {
            0xC0 | 0xC1 | 0xC4 | 0xDA | 0xDB | 0xDD | 0xE0..=0xEF | 0xFE => {}
            0xC2 | 0xC6 | 0xCA | 0xCE => return Err("progressive"),
            0xC3 | 0xC7 | 0xCB | 0xCF => return Err("lossless"),
            0xC5 | 0xDE | 0xDF => return Err("hierarchical"),
            0xC9 | 0xCC | 0xCD => return Err("arithmetic coding"),
            0xDC => return Err("number of lines given by a DNL marker"),
            0xF7 | 0xF8 => return Err("JPEG-LS"),
            0xD0..=0xD7 => return Err("restart marker outside a scan"),
            0xD8 => return Err("second start-of-image marker"),
            _ => return Err("unknown marker"),
        }
        if pos + 2 > d.len() {
            return Err("segment past the end");
        }
        let len = u16::from_be_bytes([d[pos], d[pos + 1]]) as usize;
        if len < 2 || pos + len > d.len() {
            return Err("bad segment length");
        }
        let body = &d[pos + 2..pos + len];
        let end = pos + len;
        pos = end;
        match m {
            0xC0 | 0xC1 => {
                if frame.is_some() {
                    return Err("two frame headers");
                }
                frame = Some(read_frame(body)?);
                pieces.push(Piece::Keep(start..end));
            }
            0xC4 => {
                read_tables(body, &mut tables, &mut slots)?;
                pieces.push(Piece::Tables);
            }
            0xDD => {
                if body.len() != 2 {
                    return Err("bad restart interval segment");
                }
                restart = u16::from_be_bytes([body[0], body[1]]) as u32;
                pieces.push(Piece::Keep(start..end));
            }
            0xDB => {
                read_quant_tables(body, &mut qdefined, &qused)?;
                pieces.push(Piece::Keep(start..end));
            }
            0xDA => {
                let f = frame.as_ref().ok_or("scan before the frame header")?;
                let comps = read_scan_header(body, f)?;
                let mut t = [None; 8];
                for &(ci, td, ta) in &comps {
                    t[td as usize] = Some(slots[td as usize].ok_or("scan uses an undefined Huffman table")?);
                    t[4 + ta as usize] = Some(slots[4 + ta as usize].ok_or("scan uses an undefined Huffman table")?);
                    let tq = f.comps[ci].tq as usize;
                    if !qdefined[tq] {
                        return Err("scan uses an undefined quantisation table");
                    }
                    qused[tq] = true;
                }
                // The entropy-coded data runs to the first marker that is neither a stuffed 0xFF byte nor
                // a restart marker.
                let mut p = end;
                loop {
                    match d[p..].iter().position(|&b| b == 0xFF) {
                        None => {
                            p = d.len();
                            break;
                        }
                        Some(i) => {
                            p += i;
                            match d.get(p + 1) {
                                Some(0) | Some(0xD0..=0xD7) => p += 2,
                                _ => break,
                            }
                        }
                    }
                }
                if scans.len() >= 4 {
                    return Err("too many scans");
                }
                scans.push(Scan { header: start..end, data: end..p, comps, restart, tables: t });
                pieces.push(Piece::Scan(scans.len() - 1));
                pos = p;
            }
            0xE2 if body.starts_with(b"MPF\0") => {
                // Multi-picture data points at images after this one by offsets, which would move.
                return Err("multi-picture data");
            }
            // JUMBF boxes (content credentials, JPEG XT and others) can hold a hash of the file's bytes, which
            // would no longer match.
            0xEB => return Err("APP11 data (JUMBF, such as content credentials)"),
            _ => pieces.push(Piece::Keep(start..end)), // APPn, COM
        }
    }
    let frame = frame.ok_or("no frame header")?;
    if scans.is_empty() {
        return Err("no scan");
    }
    // Sequential: every component in exactly one scan.
    for i in 0..frame.comps.len() {
        if scans.iter().map(|s| s.comps.iter().filter(|c| c.0 == i).count()).sum::<usize>() != 1 {
            return Err("a component not in exactly one scan");
        }
    }
    Ok(Jpeg { data: d, frame, pieces, scans, tables })
}

fn read_frame(b: &[u8]) -> Result<Frame, Fail> {
    if b.len() < 6 {
        return Err("short frame header");
    }
    if b[0] != 8 {
        return Err("not 8-bit samples");
    }
    let height = u16::from_be_bytes([b[1], b[2]]) as u32;
    let width = u16::from_be_bytes([b[3], b[4]]) as u32;
    let n = b[5] as usize;
    if height == 0 {
        return Err("number of lines given by a DNL marker");
    }
    if width == 0 || n == 0 || n > 4 || b.len() != 6 + 3 * n {
        return Err("bad frame header");
    }
    let mut comps: Vec<Component> = Vec::with_capacity(n);
    for c in b[6..].chunks(3) {
        let (id, h, v, tq) = (c[0], c[1] >> 4, c[1] & 15, c[2]);
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || tq > 3 {
            return Err("bad frame header");
        }
        if comps.iter().any(|x| x.id == id) {
            return Err("two components with one id");
        }
        comps.push(Component { id, h, v, tq });
    }
    let hmax = comps.iter().map(|c| c.h as u32).max().unwrap_or(1);
    let vmax = comps.iter().map(|c| c.v as u32).max().unwrap_or(1);
    // libjpeg scales components up by whole factors only ("fractional sampling not implemented"); readers
    // differ on the others, so they are left as they are.
    if comps.iter().any(|c| hmax % c.h as u32 != 0 || vmax % c.v as u32 != 0) {
        return Err("fractional sampling");
    }
    Ok(Frame { width, height, comps, hmax, vmax })
}

fn read_scan_header(b: &[u8], f: &Frame) -> Result<Vec<(usize, u8, u8)>, Fail> {
    let n = *b.first().ok_or("bad scan header")? as usize;
    if n == 0 || n > 4 || b.len() != 4 + 2 * n {
        return Err("bad scan header");
    }
    let mut comps: Vec<(usize, u8, u8)> = Vec::with_capacity(n);
    for c in b[1..1 + 2 * n].chunks(2) {
        let i = f.comps.iter().position(|x| x.id == c[0]).ok_or("scan names an unknown component")?;
        // Components in frame order, each once (as the standard requires).
        if comps.last().map_or(false, |l| l.0 >= i) {
            return Err("scan components out of order");
        }
        let (td, ta) = (c[1] >> 4, c[1] & 15);
        if td > 3 || ta > 3 {
            return Err("bad Huffman table number");
        }
        comps.push((i, td, ta));
    }
    let (ss, se, a) = (b[1 + 2 * n], b[2 + 2 * n], b[3 + 2 * n]);
    if (ss, se, a) != (0, 63, 0) {
        return Err("not a sequential scan");
    }
    if n > 1 && comps.iter().map(|c| f.comps[c.0].h as u32 * f.comps[c.0].v as u32).sum::<u32>() > 10 {
        return Err("too many blocks in an MCU");
    }
    Ok(comps)
}

/// A DQT segment, checked as libjpeg checks it and more strictly: 8- or 16-bit tables numbered 0 to 3 that fill
/// the segment exactly (pdf.js reads the tables, not the length, so the two must agree). A table a scan has
/// already used is not defined again (libjpeg keeps the first, pdf.js the last).
fn read_quant_tables(mut b: &[u8], defined: &mut [bool; 4], used: &[bool; 4]) -> Result<(), Fail> {
    if b.is_empty() {
        return Err("empty quantisation table segment");
    }
    while let Some(&pt) = b.first() {
        let (pq, tq) = ((pt >> 4) as usize, (pt & 15) as usize);
        if pq > 1 || tq > 3 {
            return Err("bad quantisation table precision or number");
        }
        let n = 1 + 64 * (1 + pq);
        if b.len() < n {
            return Err("bad quantisation table segment");
        }
        if used[tq] {
            return Err("quantisation table defined again after a scan used it");
        }
        defined[tq] = true;
        b = &b[n..];
    }
    Ok(())
}

fn read_tables(mut b: &[u8], tables: &mut Vec<DecodeTable>, slots: &mut [Option<usize>; 8]) -> Result<(), Fail> {
    if b.is_empty() {
        return Err("empty Huffman table segment");
    }
    while !b.is_empty() {
        if b.len() < 17 {
            return Err("bad Huffman table segment");
        }
        let (class, id) = (b[0] >> 4, b[0] & 15);
        if class > 1 || id > 3 {
            return Err("bad Huffman table class or number");
        }
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&b[1..17]);
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if b.len() < 17 + total {
            return Err("bad Huffman table segment");
        }
        let spec = Spec { counts, values: b[17..17 + total].to_vec() };
        if tables.len() >= MAX_TABLES {
            return Err("too many Huffman tables");
        }
        tables.push(DecodeTable::new(&spec, class == 0)?);
        slots[class as usize * 4 + id as usize] = Some(tables.len() - 1);
        b = &b[17 + total..];
    }
    Ok(())
}
