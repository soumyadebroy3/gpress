//! Block costs and block writing: dynamic Huffman blocks with an optimised header, fixed blocks, stored blocks.

use super::bits::BitWriter;
use super::huffman::{canonical_codes, lengths_into, limited_lengths};
use super::lz::{d_extra, ll_extra, Hist, Sym};
use super::tables::*;

/// A dynamic block's codes and how its header is sent.
#[derive(Clone)]
pub struct DynCode {
    pub ll: Vec<u8>,
    pub d: Vec<u8>,
    pub header: Header,
    /// Header bits (after the 3-bit block type) plus data bits including end-of-block.
    pub bits: u64,
}

#[derive(Clone)]
pub struct Header {
    pub hlit: usize,
    pub hdist: usize,
    pub hclen: usize,
    pub cl: [u8; 19],
    /// (code-length symbol, extra-bit value)
    pub tokens: Vec<(u8, u8)>,
    pub bits: u64,
}

/// Data bits for a histogram under given lengths (end-of-block included via the histogram).
pub fn data_bits(h: &Hist, ll: &[u8], d: &[u8]) -> u64 {
    let mut bits = 0u64;
    for s in 0..LITLEN_SYMS {
        if h.ll[s] != 0 {
            bits += h.ll[s] as u64 * (ll[s] as u64 + ll_extra(s) as u64);
        }
    }
    for s in 0..DIST_SYMS {
        if h.d[s] != 0 {
            bits += h.d[s] as u64 * (d[s] as u64 + d_extra(s) as u64);
        }
    }
    bits
}

pub fn fixed_bits(h: &Hist) -> u64 {
    let ll = fixed_litlen_lengths();
    let d = fixed_dist_lengths();
    data_bits(h, &ll[..LITLEN_SYMS], &d[..DIST_SYMS])
}

/// Some decoders (zlib 1.2.1 and older, some phones) fail with fewer than two distance codes. Two codes of
/// length 1 keep the code complete and cost nothing when no match uses them.
pub fn patch_distances(d: &mut [u8]) {
    let used: Vec<usize> = (0..d.len()).filter(|&i| d[i] != 0).collect();
    match used.len() {
        0 => {
            d[0] = 1;
            d[1] = 1;
        }
        1 => {
            if used[0] == 0 {
                d[1] = 1;
            } else {
                d[0] = 1;
            }
        }
        _ => {}
    }
}

/// A literal/length code with a single symbol (only end-of-block) is incomplete; give it a partner.
fn patch_litlen(ll: &mut [u8]) {
    let used = ll.iter().filter(|&&l| l != 0).count();
    if used == 1 {
        if ll[0] == 0 {
            ll[0] = 1;
        } else {
            ll[1] = 1;
        }
    }
}

/// Smooths counts so that the code lengths form longer runs (cheaper header), at a small data cost.
/// Runs of similar counts become equal; counts of zero stay zero.
fn smooth(counts: &[u32], tolerance: f64) -> Vec<u32> {
    let mut out = counts.to_vec();
    let n = counts.len();
    let mut i = 0;
    while i < n {
        if counts[i] == 0 {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut sum = counts[i] as u64;
        while j < n && counts[j] != 0 {
            let avg = (sum + counts[j] as u64) as f64 / (j - i + 1) as f64;
            let ok = (i..=j).all(|k| ((counts[k] as f64) - avg).abs() <= tolerance * avg.max(1.0));
            if !ok {
                break;
            }
            sum += counts[j] as u64;
            j += 1;
        }
        if j - i >= 4 {
            let avg = ((sum as f64) / (j - i) as f64).round().max(1.0) as u32;
            for v in out[i..j].iter_mut() {
                *v = avg;
            }
        }
        i = j;
    }
    out
}

/// The cheapest dynamic code we can find for a histogram, with its exact size.
pub fn best_dynamic(h: &Hist) -> DynCode {
    dynamic_search(h, &[(None, 15), (Some(0.25), 15), (Some(0.5), 15), (None, 12), (None, 10), (Some(1.0), 15)], true)
}

/// A cheaper search for comparing parses: fewer code variants and header strategies.
pub fn good_dynamic(h: &Hist) -> DynCode {
    dynamic_search(h, &[(None, 15), (Some(0.25), 15)], false)
}

fn dynamic_search(h: &Hist, variants: &[(Option<f64>, u32)], thorough: bool) -> DynCode {
    let mut best: Option<DynCode> = None;
    for &(tol, maxb) in variants {
        let llc = match tol {
            None => h.ll.to_vec(),
            Some(t) => smooth(&h.ll, t),
        };
        let dc = match tol {
            None => h.d.to_vec(),
            Some(t) => smooth(&h.d, t),
        };
        let used_ll = llc.iter().filter(|&&c| c > 0).count();
        if used_ll > (1usize << maxb) {
            continue;
        }
        let mut ll = limited_lengths(&llc, maxb);
        let mut d = limited_lengths(&dc, maxb.min(15));
        patch_litlen(&mut ll);
        patch_distances(&mut d);
        let data = data_bits(h, &ll, &d);
        if let Some(b) = &best {
            // The header costs at least 14 bits; skip variants that cannot win.
            if data + 14 >= b.bits {
                continue;
            }
        }
        let header = if thorough { best_header(&ll, &d) } else { quick_header(&ll, &d) };
        let bits = header.bits + data;
        if best.as_ref().map_or(true, |b| bits < b.bits) {
            best = Some(DynCode { ll, d, header, bits });
        }
    }
    best.unwrap()
}

/// Very fast estimate for choosing block boundaries: Shannon cost of the symbols, their extra bits, and a
/// header charge per used symbol.
pub fn entropy_estimate_bits(h: &Hist) -> u64 {
    fn part(c: &[u32], extra: fn(usize) -> u32) -> f64 {
        let total: u64 = c.iter().map(|&x| x as u64).sum();
        if total == 0 {
            return 0.0;
        }
        let lt = (total as f64).log2();
        let mut bits = 0.0;
        let mut used = 0;
        for (s, &x) in c.iter().enumerate() {
            if x > 0 {
                let xf = x as f64;
                bits += xf * (lt - xf.log2()) + xf * extra(s) as f64;
                used += 1;
            }
        }
        bits + 4.5 * used as f64
    }
    (part(&h.ll, ll_extra) + part(&h.d, d_extra) + 40.0) as u64
}

/// Quick size estimate: the plain optimal code with a cheap header estimate, without heap allocation.
pub fn estimate_dynamic_bits(h: &Hist) -> u64 {
    let mut ll = [0u8; LITLEN_SYMS];
    let mut d = [0u8; DIST_SYMS];
    lengths_into(&h.ll, 15, &mut ll);
    lengths_into(&h.d, 15, &mut d);
    patch_litlen(&mut ll);
    patch_distances(&mut d);
    data_bits(h, &ll, &d) + fast_header_bits(&ll, &d)
}

/// Header size with greedy run-length tokens, computed without allocation.
fn fast_header_bits(ll: &[u8], d: &[u8]) -> u64 {
    let hlit = (ll.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1)).max(257);
    let hdist = (d.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1)).max(1);
    let mut seq = [0u8; LITLEN_SYMS + DIST_SYMS];
    seq[..hlit].copy_from_slice(&ll[..hlit]);
    seq[hlit..hlit + hdist].copy_from_slice(&d[..hdist]);
    let seq = &seq[..hlit + hdist];
    let mut f = [0u32; 19];
    let mut extra = 0u64;
    let mut i = 0;
    while i < seq.len() {
        let v = seq[i];
        let mut run = 1;
        while i + run < seq.len() && seq[i + run] == v {
            run += 1;
        }
        let mut r = run;
        if v == 0 {
            while r >= 11 {
                let k = r.min(138);
                f[18] += 1;
                extra += 7;
                r -= k;
            }
            if r >= 3 {
                f[17] += 1;
                extra += 3;
                r = 0;
            }
            f[0] += r as u32;
        } else {
            f[v as usize] += 1;
            r -= 1;
            while r >= 3 {
                let k = r.min(6);
                f[16] += 1;
                extra += 2;
                r -= k;
            }
            f[v as usize] += r as u32;
        }
        i += run;
    }
    let mut cl = [0u8; 19];
    lengths_into(&f, 7, &mut cl);
    if cl.iter().filter(|&&x| x != 0).count() == 1 {
        cl[16] = 1;
    }
    let hclen = (CL_ORDER.iter().rposition(|&s| cl[s] != 0).map_or(0, |p| p + 1)).max(4);
    let mut bits = 14 + 3 * hclen as u64 + extra;
    for s in 0..19 {
        bits += f[s] as u64 * cl[s] as u64;
    }
    bits
}

fn sequence(ll: &[u8], d: &[u8]) -> (usize, usize, Vec<u8>) {
    let hlit = (ll.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1)).max(257);
    let hdist = (d.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1)).max(1);
    let mut seq = Vec::with_capacity(hlit + hdist);
    seq.extend_from_slice(&ll[..hlit]);
    seq.extend_from_slice(&d[..hdist]);
    (hlit, hdist, seq)
}

fn tok_extra_bits(sym: u8) -> u64 {
    match sym {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    }
}

/// Zlib-style run-length tokens of a length sequence.
fn greedy_tokens(seq: &[u8], use16: bool, use17: bool, use18: bool) -> Vec<(u8, u8)> {
    let mut t = Vec::new();
    let mut i = 0;
    while i < seq.len() {
        let v = seq[i];
        let mut run = 1;
        while i + run < seq.len() && seq[i + run] == v {
            run += 1;
        }
        if v == 0 && (use17 || use18) && run >= 3 {
            let mut r = run;
            while r >= 3 {
                if use18 && r >= 11 {
                    let k = r.min(138);
                    t.push((18, (k - 11) as u8));
                    r -= k;
                } else if use17 {
                    let k = r.min(10);
                    t.push((17, (k - 3) as u8));
                    r -= k;
                } else {
                    break;
                }
            }
            for _ in 0..r {
                t.push((0, 0));
            }
        } else if v != 0 && use16 && run >= 4 {
            t.push((v, 0));
            let mut r = run - 1;
            while r >= 3 {
                let k = r.min(6);
                t.push((16, (k - 3) as u8));
                r -= k;
            }
            for _ in 0..r {
                t.push((v, 0));
            }
        } else {
            for _ in 0..run {
                t.push((v, 0));
            }
        }
        i += run;
    }
    t
}

/// Cheapest tokens for a length sequence given the cost of each code-length symbol: a shortest path over
/// positions, where among equal costs the earliest step found is kept.
fn dp_tokens(seq: &[u8], cost: &[u64; 19]) -> Vec<(u8, u8)> {
    let n = seq.len();
    let mut best = vec![u64::MAX; n + 1];
    let mut from: Vec<(u8, u8, u16)> = vec![(0, 0, 0); n + 1]; // (sym, extra, run)
    best[0] = 0;
    // zrun[i]: zeros starting at i; vrun[i]: values equal to seq[i-1] starting at i.
    let mut zrun = vec![0usize; n + 1];
    let mut vrun = vec![0usize; n + 1];
    for i in (0..n).rev() {
        zrun[i] = if seq[i] == 0 { zrun[i + 1] + 1 } else { 0 };
    }
    for i in (1..n).rev() {
        vrun[i] = if seq[i] == seq[i - 1] { if i + 1 < n && seq[i + 1] == seq[i] { vrun[i + 1] + 1 } else { 1 } } else { 0 };
    }
    // Symbol 18 (11 to 138 zeros, one price) reaches position t only from t - 138 ..= t - 11, so it is
    // settled before any other symbol can reach t (they come from t - 10 or later). Its best start is the
    // cheapest one in that window (the earliest of equal ones), kept in a queue of starts with rising cost.
    let mut win: std::collections::VecDeque<(usize, u64)> = std::collections::VecDeque::new();
    for i in 0..n {
        let b = best[i];
        if b != u64::MAX && zrun[i] >= 11 {
            let c = b + cost[18] + 7;
            while win.back().map_or(false, |&(_, x)| x > c) {
                win.pop_back();
            }
            win.push_back((i, c));
        }
        let t = i + 11;
        if t <= n {
            // Starts too far back, or in a run of zeros that ends before t, can't reach t (nor anything later).
            while win.front().map_or(false, |&(j, _)| j + 138 < t || j + zrun[j] < t) {
                win.pop_front();
            }
            if let Some(&(j, c)) = win.front() {
                debug_assert!(best[t] == u64::MAX);
                best[t] = c;
                from[t] = (18, (t - j - 11) as u8, (t - j) as u16);
            }
        }
        if b == u64::MAX {
            continue;
        }
        let v = seq[i];
        let c = b + cost[v as usize];
        if c < best[i + 1] {
            best[i + 1] = c;
            from[i + 1] = (v, 0, 1);
        }
        if i >= 1 && vrun[i] >= 3 {
            let c = b + cost[16] + 2;
            for k in 3..=vrun[i].min(6) {
                if c < best[i + k] {
                    best[i + k] = c;
                    from[i + k] = (16, (k - 3) as u8, k as u16);
                }
            }
        }
        if zrun[i] >= 3 {
            let c = b + cost[17] + 3;
            for k in 3..=zrun[i].min(10) {
                if c < best[i + k] {
                    best[i + k] = c;
                    from[i + k] = (17, (k - 3) as u8, k as u16);
                }
            }
        }
    }
    let mut t = Vec::new();
    let mut i = n;
    while i > 0 {
        let (s, e, r) = from[i];
        t.push((s, e));
        i -= r as usize;
    }
    t.reverse();
    t
}

fn cl_lengths(tokens: &[(u8, u8)]) -> [u8; 19] {
    let mut f = [0u32; 19];
    for &(s, _) in tokens {
        f[s as usize] += 1;
    }
    let l = limited_lengths(&f, 7);
    let mut cl = [0u8; 19];
    cl.copy_from_slice(&l);
    // The code-length code must be complete: with one symbol, add a partner.
    if cl.iter().filter(|&&x| x != 0).count() == 1 {
        let other = CL_ORDER.iter().copied().find(|&s| cl[s] == 0).unwrap();
        cl[other] = 1;
    }
    cl
}

fn header_bits(cl: &[u8; 19], tokens: &[(u8, u8)]) -> (usize, u64) {
    let hclen = (CL_ORDER.iter().rposition(|&s| cl[s] != 0).map_or(0, |p| p + 1)).max(4);
    let mut bits = 14 + 3 * hclen as u64;
    for &(s, _) in tokens {
        bits += cl[s as usize] as u64 + tok_extra_bits(s);
    }
    (hclen, bits)
}


/// The cheapest header we can find for these code lengths.
pub fn best_header(ll: &[u8], d: &[u8]) -> Header {
    header_search(ll, d, &[0, 1, 2, 3, 4, 5, 6, 7], 4)
}

/// Fewer strategies, for comparing parses.
pub fn quick_header(ll: &[u8], d: &[u8]) -> Header {
    header_search(ll, d, &[7, 6], 2)
}

fn header_search(ll: &[u8], d: &[u8], flag_sets: &[u8], rounds: usize) -> Header {
    let (hlit, hdist, seq) = sequence(ll, d);
    let mut best: Option<Header> = None;
    // Token lists already tried, with what followed them. The searches from different starting points often
    // meet; a list seen before can't improve on the best (its size was compared already), and its next
    // list is known, so it is not worked out again. The result is the same as without this.
    let mut seen: Vec<(Vec<(u8, u8)>, Vec<(u8, u8)>)> = Vec::new();
    for &flags in flag_sets {
        let mut tokens = greedy_tokens(&seq, flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
        for _round in 0..rounds {
            let nt = match seen.iter().find(|(t, _)| *t == tokens) {
                Some((_, next)) => next.clone(),
                None => {
                    let cl = cl_lengths(&tokens);
                    let (hclen, bits) = header_bits(&cl, &tokens);
                    let improved = best.as_ref().map_or(true, |b| bits < b.bits);
                    if improved {
                        best = Some(Header { hlit, hdist, hclen, cl, tokens: tokens.clone(), bits });
                    }
                    // Re-tokenise for these code-length codes; unused symbols are priced at 7 bits plus a little.
                    let mut cost = [0u64; 19];
                    for s in 0..19 {
                        cost[s] = if cl[s] != 0 { cl[s] as u64 } else { 8 };
                    }
                    let nt = dp_tokens(&seq, &cost);
                    seen.push((tokens.clone(), nt.clone()));
                    nt
                }
            };
            if nt == tokens {
                break;
            }
            tokens = nt;
        }
    }
    best.unwrap()
}

/// Exact bits of a stored block sequence for `len` bytes, starting at bit offset `bitpos` (mod 8 matters).
pub fn stored_bits(len: usize, bitpos: u64) -> u64 {
    let mut pos = bitpos;
    let mut left = len;
    let start = bitpos;
    loop {
        let k = left.min(65535);
        pos += 3;
        pos = (pos + 7) / 8 * 8;
        pos += 32 + 8 * k as u64;
        left -= k;
        if left == 0 {
            break;
        }
    }
    pos - start
}

pub fn write_dynamic(w: &mut BitWriter, syms: &[Sym], code: &DynCode, last: bool) {
    w.put(last as u32, 1);
    w.put(2, 2);
    let h = &code.header;
    w.put((h.hlit - 257) as u32, 5);
    w.put((h.hdist - 1) as u32, 5);
    w.put((h.hclen - 4) as u32, 4);
    for &s in CL_ORDER.iter().take(h.hclen) {
        w.put(h.cl[s] as u32, 3);
    }
    let clc = canonical_codes(&h.cl);
    for &(s, e) in &h.tokens {
        w.put(clc[s as usize] as u32, h.cl[s as usize] as u32);
        let eb = tok_extra_bits(s) as u32;
        if eb > 0 {
            w.put(e as u32, eb);
        }
    }
    write_symbols(w, syms, &code.ll, &code.d);
}

pub fn write_fixed(w: &mut BitWriter, syms: &[Sym], last: bool) {
    w.put(last as u32, 1);
    w.put(1, 2);
    let ll = fixed_litlen_lengths();
    let d = fixed_dist_lengths();
    write_symbols(w, syms, &ll, &d);
}

fn write_symbols(w: &mut BitWriter, syms: &[Sym], ll: &[u8], d: &[u8]) {
    let llc = canonical_codes(ll);
    let dc = canonical_codes(d);
    for s in syms {
        if s.dist == 0 {
            let b = s.litlen as usize;
            w.put(llc[b] as u32, ll[b] as u32);
        } else {
            let l = s.litlen as usize;
            let c = LEN_CODE[l] as usize;
            w.put(llc[257 + c] as u32, ll[257 + c] as u32);
            if LEN_EXTRA[c] > 0 {
                w.put((l - LEN_BASE[c] as usize) as u32, LEN_EXTRA[c] as u32);
            }
            let dist = s.dist as usize;
            let k = dist_code(dist);
            w.put(dc[k] as u32, d[k] as u32);
            if DIST_EXTRA[k] > 0 {
                w.put((dist - DIST_BASE[k] as usize) as u32, DIST_EXTRA[k] as u32);
            }
        }
    }
    w.put(llc[END_OF_BLOCK] as u32, ll[END_OF_BLOCK] as u32);
}

pub fn write_stored(w: &mut BitWriter, bytes: &[u8], last: bool) {
    let mut chunks = bytes.chunks(65535).peekable();
    if bytes.is_empty() {
        w.put(last as u32, 1);
        w.put(0, 2);
        w.align();
        w.put(0, 16);
        w.put(0xffff, 16);
        return;
    }
    while let Some(c) = chunks.next() {
        let is_last = last && chunks.peek().is_none();
        w.put(is_last as u32, 1);
        w.put(0, 2);
        w.align();
        w.put(c.len() as u32, 16);
        w.put(!(c.len() as u32) & 0xffff, 16);
        for &b in c {
            w.put(b as u32, 8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The token search as it was first written (every symbol-18 step relaxed one by one).
    fn dp_tokens_reference(seq: &[u8], cost: &[u64; 19]) -> Vec<(u8, u8)> {
        let n = seq.len();
        let mut best = vec![u64::MAX; n + 1];
        let mut from: Vec<(u8, u8, u16)> = vec![(0, 0, 0); n + 1];
        best[0] = 0;
        let mut zrun = vec![0usize; n + 1];
        let mut vrun = vec![0usize; n + 1];
        for i in (0..n).rev() {
            zrun[i] = if seq[i] == 0 { zrun[i + 1] + 1 } else { 0 };
        }
        for i in (1..n).rev() {
            vrun[i] = if seq[i] == seq[i - 1] { if i + 1 < n && seq[i + 1] == seq[i] { vrun[i + 1] + 1 } else { 1 } } else { 0 };
        }
        for i in 0..n {
            let b = best[i];
            if b == u64::MAX {
                continue;
            }
            let v = seq[i];
            let c = b + cost[v as usize];
            if c < best[i + 1] {
                best[i + 1] = c;
                from[i + 1] = (v, 0, 1);
            }
            if i >= 1 && vrun[i] >= 3 {
                let c = b + cost[16] + 2;
                for k in 3..=vrun[i].min(6) {
                    if c < best[i + k] {
                        best[i + k] = c;
                        from[i + k] = (16, (k - 3) as u8, k as u16);
                    }
                }
            }
            if zrun[i] >= 3 {
                let c = b + cost[17] + 3;
                for k in 3..=zrun[i].min(10) {
                    if c < best[i + k] {
                        best[i + k] = c;
                        from[i + k] = (17, (k - 3) as u8, k as u16);
                    }
                }
                if zrun[i] >= 11 {
                    let c = b + cost[18] + 7;
                    for k in 11..=zrun[i].min(138) {
                        if c < best[i + k] {
                            best[i + k] = c;
                            from[i + k] = (18, (k - 11) as u8, k as u16);
                        }
                    }
                }
            }
        }
        let mut t = Vec::new();
        let mut i = n;
        while i > 0 {
            let (s, e, r) = from[i];
            t.push((s, e));
            i -= r as usize;
        }
        t.reverse();
        t
    }

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    #[test]
    fn token_search_matches_the_reference() {
        let mut s = 0x5eed_1234u64;
        for round in 0..3000 {
            // Code-length sequences with runs of zeros (some longer than 138) and of equal values.
            let n = 1 + (rng(&mut s) % 320) as usize;
            let mut seq = Vec::with_capacity(n);
            while seq.len() < n {
                let kind = rng(&mut s) % 4;
                let len = match kind {
                    0 => 1 + (rng(&mut s) % if round % 3 == 0 { 300 } else { 40 }) as usize,
                    1 => 1 + (rng(&mut s) % 12) as usize,
                    _ => 1,
                };
                let v = if kind == 0 { 0 } else { (rng(&mut s) % 16) as u8 };
                for _ in 0..len.min(n - seq.len()) {
                    seq.push(v);
                }
            }
            // Costs with many ties (small range) or spread out.
            let mut cost = [0u64; 19];
            for c in cost.iter_mut() {
                *c = if round % 2 == 0 { 1 + rng(&mut s) % 3 } else { 1 + rng(&mut s) % 9 };
            }
            assert_eq!(dp_tokens(&seq, &cost), dp_tokens_reference(&seq, &cost), "round {round}: {seq:?} {cost:?}");
        }
    }

    #[test]
    fn header_search_is_unchanged_by_the_memory_of_tried_tokens() {
        // Every header the search returns must describe the lengths it was given.
        let mut s = 77u64;
        for _ in 0..200 {
            let mut h = Hist::empty();
            for i in 0..LITLEN_SYMS {
                if rng(&mut s) % 3 == 0 {
                    h.ll[i] = (rng(&mut s) % 1000) as u32;
                }
            }
            for i in 0..DIST_SYMS {
                if rng(&mut s) % 2 == 0 {
                    h.d[i] = (rng(&mut s) % 300) as u32;
                }
            }
            for code in [best_dynamic(&h), good_dynamic(&h)] {
                // Expand the tokens back into lengths.
                let mut lens: Vec<u8> = Vec::new();
                for &(sym, e) in &code.header.tokens {
                    match sym {
                        16 => {
                            let last = *lens.last().unwrap();
                            lens.extend(std::iter::repeat(last).take(3 + e as usize));
                        }
                        17 => lens.extend(std::iter::repeat(0).take(3 + e as usize)),
                        18 => lens.extend(std::iter::repeat(0).take(11 + e as usize)),
                        v => lens.push(v),
                    }
                }
                let (hl, hd) = (code.header.hlit, code.header.hdist);
                assert_eq!(lens.len(), hl + hd);
                assert_eq!(&lens[..hl], &code.ll[..hl]);
                assert_eq!(&lens[hl..], &code.d[..hd]);
            }
        }
    }
}
