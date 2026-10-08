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
//! coded, lossless, hierarchical and 12-bit JPEGs, fractional sampling, and anything unusual or damaged, are
//! refused (the caller keeps the original). So are files whose layout readers could take differently once the tables move (fill bytes
//! before a marker, segments whose contents don't fill them exactly), and files with data bound to their exact
//! bytes (multi-picture offsets, APP11 boxes such as content credentials). Before a new file is returned it is
//! read back and compared with the original: the same bytes outside the tables and the coded data, the same
//! symbols in every scan.

mod huffman;
mod parse;
mod scan;

use huffman::{Order, Spec};
use parse::{parse, Jpeg, Piece, Scan};
use scan::{BitWriter, CountFF, Sink, Tokens, RESTART};

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
    let mut freq = [[0u64; 256]; 9]; // the last for restart markers, not counted
    let mut t = Tokens::new(j, s)?;
    let mut buf = Tokens::buffer();
    loop {
        let n = t.next(&mut buf)?;
        if n == 0 {
            break;
        }
        for tok in &buf[..n] {
            freq[tok.slot() as usize][tok.sym() as usize] += 1;
        }
    }
    let used: Vec<usize> = (0..8).filter(|&k| s.tables[k].is_some()).collect();
    let mut lens = vec![[0u8; 256]; 8];
    for &slot in &used {
        if freq[slot].iter().all(|&f| f == 0) {
            return Err("a table without symbols");
        }
        lens[slot] = huffman::optimal_lengths(&freq[slot]);
    }
    // The lengths are optimal; which symbol of a length gets which code word decides how many 0xFF bytes come
    // out (each costs a stuffed 0x00). Every order gives the same bits otherwise, so one pass writes the first
    // and counts the 0xFF bytes of the others; another order is written only when it has fewer.
    let mut specs: Vec<Vec<Option<Spec>>> = Vec::with_capacity(3);
    for order in [Order::Value, Order::Frequent, Order::FewestOnes] {
        let mut sp: Vec<Option<Spec>> = vec![None; 8];
        for &slot in &used {
            sp[slot] = Some(huffman::table(&lens[slot], &freq[slot], order).ok_or("no valid table")?);
        }
        specs.push(sp);
    }
    let start = out.len();
    let ff = write_scan(j, s, &[&specs[0], &specs[1], &specs[2]], out)?;
    let best = (0..ff.len()).min_by_key(|&o| (ff[o], o)).ok_or("no table order")?;
    if best != 0 {
        out.truncate(start);
        write_scan(j, s, &[&specs[best]], out)?;
    }
    Ok(())
}

/// The bytes of a scan go into a vector; its 0xFF bytes are counted.
struct VecSink<'a> {
    v: &'a mut Vec<u8>,
    ff: u64,
}

impl Sink for VecSink<'_> {
    #[inline]
    fn byte(&mut self, b: u8) {
        self.v.push(b);
        self.ff += (b == 0xFF) as u64;
    }
    fn marker(&mut self, m: u8) {
        self.v.extend_from_slice(&[0xFF, m]);
    }
}

/// The table segment for `specs[0]` (by slot), the scan header as it was, and the coded data with those tables.
/// The coded data with each of the other tables (the same code lengths) is only counted. Returns the number of
/// 0xFF bytes in the coded data, for each of `specs`.
fn write_scan(j: &Jpeg, s: &Scan, specs: &[&[Option<Spec>]], out: &mut Vec<u8>) -> Result<Vec<u64>, &'static str> {
    // Per order, slot and symbol: code word << 8 | length (0: no code). Slot 8 (restart markers) stays empty.
    let mut codes = vec![[[0u32; 256]; 9]; specs.len()];
    let mut seg = vec![0xFF, 0xC4, 0, 0];
    for (slot, spec) in specs[0].iter().enumerate() {
        if let Some(spec) = spec {
            seg.push((((slot / 4) << 4) | (slot % 4)) as u8);
            seg.extend_from_slice(&spec.counts);
            seg.extend_from_slice(&spec.values);
        }
    }
    for (o, sp) in specs.iter().enumerate() {
        for (slot, spec) in sp.iter().enumerate() {
            if let Some(spec) = spec {
                for (c, &(code, len)) in codes[o][slot].iter_mut().zip(huffman::codes(spec).iter()) {
                    *c = (code as u32) << 8 | len as u32;
                }
            }
        }
    }
    let len = u16::try_from(seg.len() - 2).map_err(|_| "table segment too long")?;
    seg[2..4].copy_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&seg);
    out.extend_from_slice(&j.data[s.header.clone()]);
    let mut w = BitWriter::new(VecSink { v: out, ff: 0 });
    let mut others: Vec<BitWriter<CountFF>> = (1..specs.len()).map(|_| BitWriter::new(CountFF::default())).collect();
    let mut t = Tokens::new(j, s)?;
    let mut buf = Tokens::buffer();
    loop {
        let n = t.next(&mut buf)?;
        if n == 0 {
            break;
        }
        for tok in &buf[..n] {
            if tok.slot() == RESTART {
                w.restart(tok.sym());
                others.iter_mut().for_each(|c| c.restart(tok.sym()));
                continue;
            }
            let (slot, sym, nbits, bits) =
                (tok.slot() as usize, tok.sym() as usize, tok.nbits() as u32, tok.bits() as u32);
            let e = codes[0][slot][sym];
            let len = e & 0xFF;
            if len == 0 {
                return Err("a symbol without a code");
            }
            // The code word and its extra bits together: at most 16 + 11 bits.
            w.put((e >> 8) << nbits | bits, len + nbits);
            for (c, o) in others.iter_mut().zip(1..) {
                // The same lengths in every order: only the code words differ.
                c.put((codes[o][slot][sym] >> 8) << nbits | bits, len + nbits);
            }
        }
    }
    w.pad();
    others.iter_mut().for_each(|c| c.pad());
    Ok(std::iter::once(w.out.ff).chain(others.iter().map(|c| c.out.0)).collect())
}

/// For measuring: per scan, the components, MCUs' worth of tokens, restart markers and zero-run symbols that
/// only lead to the end of their block.
pub fn info(d: &[u8]) -> Result<String, Refused> {
    let j = parse(d).map_err(Refused)?;
    let mut out = format!(
        "{}x{} comps {:?}",
        j.frame.width,
        j.frame.height,
        j.frame.comps.iter().map(|c| (c.h, c.v)).collect::<Vec<_>>()
    );
    let mut buf = Tokens::buffer();
    for s in &j.scans {
        let mut t = Tokens::new(&j, s).map_err(Refused)?;
        let (mut toks, mut blocks, mut rst, mut zrl, mut idle_zrl, mut run) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        loop {
            let n = t.next(&mut buf).map_err(Refused)?;
            if n == 0 {
                break;
            }
            for tok in &buf[..n] {
                match (tok.slot(), tok.sym()) {
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
                toks += (tok.slot() != RESTART) as u64;
            }
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
    let (mut x, mut y) = (Tokens::buffer(), Tokens::buffer());
    for (sa, sb) in ja.scans.iter().zip(&jb.scans) {
        if sa.comps != sb.comps || sa.restart != sb.restart {
            return false;
        }
        let (Ok(mut ta), Ok(mut tb)) = (Tokens::new(ja, sa), Tokens::new(jb, sb)) else { return false };
        // Batch by batch; a different batching would only make the two compare unequal, never equal.
        loop {
            match (ta.next(&mut x), tb.next(&mut y)) {
                (Ok(0), Ok(0)) => break,
                (Ok(n), Ok(m)) if n == m && x[..n] == y[..m] => {}
                _ => return false,
            }
        }
    }
    true
}
