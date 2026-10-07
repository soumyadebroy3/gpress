//! JPEG images made smaller without changing a single decoded pixel: the entropy-coded data is written again
//! with optimal Huffman tables (what `jpegtran -optimize -copy all` does), by our own code.
//!
//! Every coded symbol is kept as it is (DC differences, AC run/size symbols with their extra bits, end-of-block
//! and zero-run symbols, restart markers), so every decoder gets the same coefficients. Every other byte of the
//! file stays as it is too (APPn segments such as ICC profiles and Adobe markers, comments, quantisation tables,
//! frame and scan headers, restart intervals, the end marker and anything after it); only the Huffman table
//! segments are replaced, by one segment right before each scan with the tables that scan uses.
//!
//! Only baseline and extended sequential Huffman JPEG with 8-bit samples is handled. Progressive, arithmetic-
//! coded, lossless, hierarchical and 12-bit JPEGs, and anything unusual or damaged, are refused (the caller keeps
//! the original). So are files whose layout readers could take differently once the tables move (fill bytes
//! before a marker, segments whose contents don't fill them exactly), and files with data bound to their exact
//! bytes (multi-picture offsets, APP11 boxes such as content credentials). Before a new file is returned it is
//! read back and compared with the original: the same bytes outside the tables and the coded data, the same
//! symbols in every scan.

mod huffman;
mod parse;
mod scan;

use huffman::{Order, Spec};
use parse::{parse, Jpeg, Piece, Scan};
use scan::{BitWriter, Tokens, RESTART};

/// Why a JPEG was left as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused(pub &'static str);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Largest JPEG we work on.
pub const MAX_LEN: usize = 1 << 30;

/// The same JPEG with optimal Huffman tables, checked, when that is smaller.
pub fn optimize(d: &[u8]) -> Result<Vec<u8>, Refused> {
    if d.len() > MAX_LEN {
        return Err(Refused("too large"));
    }
    let mut out = Vec::with_capacity(d.len());
    {
        let j = parse(d).map_err(Refused)?;
        for p in &j.pieces {
            match p {
                Piece::Keep(r) => out.extend_from_slice(&d[r.clone()]),
                Piece::Tables => {}
                Piece::Scan(i) => encode_scan(&j, &j.scans[*i], &mut out).map_err(Refused)?,
            }
        }
    }
    if out.len() >= d.len() {
        return Err(Refused("no saving"));
    }
    if !equivalent(d, &out) {
        return Err(Refused("the new file did not read back the same"));
    }
    Ok(out)
}

/// One scan with optimal tables: the table segment, the scan header as it was, and the coded data.
fn encode_scan(j: &Jpeg, s: &Scan, out: &mut Vec<u8>) -> Result<(), &'static str> {
    let mut freq = vec![[0u64; 256]; 8];
    let mut t = Tokens::new(j, s)?;
    while let Some(tok) = t.next()? {
        if tok.slot != RESTART {
            freq[tok.slot as usize][tok.sym as usize] += 1;
        }
    }
    let mut lens = vec![[0u8; 256]; 8];
    for slot in (0..8).filter(|&k| s.tables[k].is_some()) {
        if freq[slot].iter().all(|&f| f == 0) {
            return Err("a table without symbols");
        }
        lens[slot] = huffman::optimal_lengths(&freq[slot]);
    }
    // The lengths are optimal; which symbol of a length gets which code word decides how many 0xFF bytes come
    // out (each costs a stuffed 0x00). Try a few orders, keep the shortest.
    let mut best: Option<Vec<u8>> = None;
    for order in [Order::Value, Order::Frequent, Order::FewestOnes] {
        let mut specs: Vec<Option<Spec>> = vec![None; 8];
        for slot in (0..8).filter(|&k| s.tables[k].is_some()) {
            specs[slot] = Some(huffman::table(&lens[slot], &freq[slot], order).ok_or("no valid table")?);
        }
        let mut buf = Vec::with_capacity(best.as_ref().map_or(s.data.len() + 1024, |b| b.len()));
        write_scan(j, s, &specs, &mut buf)?;
        if best.as_ref().map_or(true, |b| buf.len() < b.len()) {
            best = Some(buf);
        }
    }
    out.extend_from_slice(&best.ok_or("no table order")?);
    Ok(())
}

/// The table segment for `specs` (by slot), the scan header as it was, and the coded data with those tables.
fn write_scan(j: &Jpeg, s: &Scan, specs: &[Option<Spec>], out: &mut Vec<u8>) -> Result<(), &'static str> {
    let mut codes = vec![[(0u16, 0u8); 256]; 8];
    let mut seg = vec![0xFF, 0xC4, 0, 0];
    for (slot, spec) in specs.iter().enumerate() {
        if let Some(spec) = spec {
            seg.push(((slot / 4) << 4 | slot % 4) as u8);
            seg.extend_from_slice(&spec.counts);
            seg.extend_from_slice(&spec.values);
            codes[slot] = huffman::codes(spec);
        }
    }
    let len = u16::try_from(seg.len() - 2).map_err(|_| "table segment too long")?;
    seg[2..4].copy_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&seg);
    out.extend_from_slice(&j.data[s.header.clone()]);
    let mut w = BitWriter::new(out);
    let mut t = Tokens::new(j, s)?;
    while let Some(tok) = t.next()? {
        if tok.slot == RESTART {
            w.pad();
            w.out.extend_from_slice(&[0xFF, 0xD0 + tok.sym]);
            continue;
        }
        let (code, len) = codes[tok.slot as usize][tok.sym as usize];
        if len == 0 {
            return Err("a symbol without a code");
        }
        w.put(code as u32, len as u32);
        if tok.nbits > 0 {
            w.put(tok.bits as u32, tok.nbits as u32);
        }
    }
    w.pad();
    Ok(())
}

/// For measuring: per scan, the components, MCUs' worth of tokens, restart markers and zero-run symbols that
/// only lead to the end of their block.
pub fn info(d: &[u8]) -> Result<String, Refused> {
    let j = parse(d).map_err(Refused)?;
    let mut out = format!("{}x{} comps {:?}", j.frame.width, j.frame.height, j.frame.comps.iter().map(|c| (c.h, c.v)).collect::<Vec<_>>());
    for s in &j.scans {
        let mut t = Tokens::new(&j, s).map_err(Refused)?;
        let (mut toks, mut blocks, mut rst, mut zrl, mut idle_zrl, mut run) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        while let Some(tok) = t.next().map_err(Refused)? {
            match (tok.slot, tok.sym) {
                (RESTART, _) => rst += 1,
                (0..=3, _) => {
                    blocks += 1;
                    idle_zrl += run; // zero runs that ended the previous block
                    run = 0;
                }
                (_, 0x00) => {
                    idle_zrl += run;
                    run = 0;
                }
                (_, 0xF0) => {
                    zrl += 1;
                    run += 1;
                }
                _ => run = 0,
            }
            toks += (tok.slot != RESTART) as u64;
        }
        idle_zrl += run;
        out += &format!(
            " | scan {:?} restart {} blocks {blocks} tokens {toks} restarts {rst} zrl {zrl} zrl-before-end {idle_zrl}",
            s.comps.iter().map(|c| c.0).collect::<Vec<_>>(),
            s.restart
        );
    }
    Ok(out)
}

/// True if `b` is `a` with only its Huffman tables and coded data changed: the same bytes everywhere else (in
/// the same order) and the same symbols in every scan. Both must be JPEGs we can read.
pub fn equivalent(a: &[u8], b: &[u8]) -> bool {
    let (Ok(ja), Ok(jb)) = (parse(a), parse(b)) else { return false };
    fn kept<'j>(j: &'j Jpeg) -> impl Iterator<Item = &'j [u8]> + 'j {
        j.pieces.iter().filter_map(move |p| match p {
            Piece::Keep(r) => Some(&j.data[r.clone()]),
            Piece::Scan(i) => Some(&j.data[j.scans[*i].header.clone()]),
            Piece::Tables => None,
        })
    }
    if !kept(&ja).eq(kept(&jb)) {
        return false;
    }
    same_symbols(&ja, &jb)
}

/// True if both are JPEGs we can read with the same coded symbols in every scan (other bytes may differ).
pub fn same_coded_data(a: &[u8], b: &[u8]) -> bool {
    match (parse(a), parse(b)) {
        (Ok(ja), Ok(jb)) => same_symbols(&ja, &jb),
        _ => false,
    }
}

fn same_symbols(ja: &Jpeg, jb: &Jpeg) -> bool {
    if ja.scans.len() != jb.scans.len() {
        return false;
    }
    for (sa, sb) in ja.scans.iter().zip(&jb.scans) {
        if sa.comps != sb.comps || sa.restart != sb.restart {
            return false;
        }
        let (Ok(mut ta), Ok(mut tb)) = (Tokens::new(ja, sa), Tokens::new(jb, sb)) else { return false };
        loop {
            match (ta.next(), tb.next()) {
                (Ok(Some(x)), Ok(Some(y))) if x == y => {}
                (Ok(None), Ok(None)) => break,
                _ => return false,
            }
        }
    }
    true
}
