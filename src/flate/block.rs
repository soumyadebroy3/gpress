//! Block costs and block writing: dynamic Huffman blocks with an optimised header, fixed blocks, stored blocks.

use super::bits::BitWriter;
use super::huffman::{canonical_codes, limited_lengths, limited_lengths_into, LengthCoder};
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
        if best.as_ref().is_none_or(|b| bits < b.bits) {
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

/// Quick size estimate: the plain optimal code with a cheap header estimate.
pub fn estimate_dynamic_bits(h: &Hist) -> u64 {
    Estimator::new().bits(h)
}

/// `estimate_dynamic_bits` for many histograms in a row, as the block-split search asks: each code keeps its
/// symbols' order from one histogram to the next, which is quick to repair when the counts changed a little.
/// The estimates are the same. What it keeps (several KB) is on the heap, made with it and freed with it, so
/// that a search that holds two of them needs little stack.
pub struct Estimator {
    coders: Box<Coders>,
}

struct Coders {
    ll: LengthCoder<LITLEN_SYMS>,
    d: LengthCoder<DIST_SYMS>,
    header: HeaderMemo,
}

/// What the header estimate keeps between calls: neighbouring histograms often give the same lengths.
struct HeaderMemo {
    /// The last length sequence.
    seq: [u8; LITLEN_SYMS + DIST_SYMS],
    /// Its length and header bits, once there is one.
    last: Option<(usize, u64)>,
    cl: LengthCoder<19>,
    /// The last code-length counts and the header bits they gave, without the extra bits.
    last_cl: Option<([u32; 19], u64)>,
}

impl Default for Estimator {
    fn default() -> Self {
        Self::new()
    }
}

impl Estimator {
    pub fn new() -> Estimator {
        // A constant, so that it is copied straight to the heap.
        const NEW: Coders = Coders {
            ll: LengthCoder::new(),
            d: LengthCoder::new(),
            header: HeaderMemo { seq: [0; LITLEN_SYMS + DIST_SYMS], last: None, cl: LengthCoder::new(), last_cl: None },
        };
        Estimator { coders: Box::new(NEW) }
    }

    /// The same as `estimate_dynamic_bits(h)`.
    pub fn bits(&mut self, h: &Hist) -> u64 {
        self.coders.bits(h)
    }
}

impl Coders {
    fn bits(&mut self, h: &Hist) -> u64 {
        // Data bits: count times length, plus the extra bits.
        let mut bits = self.ll.lengths(&h.ll, 15) + self.d.lengths(&h.d, 15);
        for (&c, &e) in h.ll[257..].iter().zip(&LEN_EXTRA) {
            bits += c as u64 * e as u64;
        }
        for (&c, &e) in h.d.iter().zip(&DIST_EXTRA) {
            bits += c as u64 * e as u64;
        }
        // The patches give lengths to unused symbols only, so the data bits stay. For the distances this is
        // `patch_distances` without its allocation.
        if self.ll.used() == 1 {
            patch_litlen(&mut self.ll.lens);
        }
        let used_d = self.d.used();
        let d = &mut self.d.lens;
        match used_d {
            0 => {
                d[0] = 1;
                d[1] = 1;
            }
            1 if d[0] != 0 => d[1] = 1,
            1 => d[0] = 1,
            _ => {}
        }
        bits + fast_header_bits(&self.ll.lens, &self.d.lens, &mut self.header)
    }
}

/// How many bytes from `i` on equal `seq[i]`, compared eight at a time.
fn run_length(seq: &[u8], i: usize) -> usize {
    let v = seq[i];
    let pattern = u64::from_le_bytes([v; 8]);
    let mut j = i + 1;
    while let Some(chunk) = seq.get(j..j + 8) {
        let x = u64::from_le_bytes(chunk.try_into().unwrap()) ^ pattern;
        if x != 0 {
            return j - i + x.trailing_zeros() as usize / 8;
        }
        j += 8;
    }
    while j < seq.len() && seq[j] == v {
        j += 1;
    }
    j - i
}

/// Header size with greedy run-length tokens, computed without allocation. The size depends on the length
/// sequence only, so the same sequence as last time gives the last size.
fn fast_header_bits(ll: &[u8], d: &[u8], memo: &mut HeaderMemo) -> u64 {
    let hlit = (ll.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1)).max(257);
    let hdist = (d.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1)).max(1);
    let len = hlit + hdist;
    if let Some((last_len, bits)) = memo.last {
        if last_len == len && memo.seq[..hlit] == ll[..hlit] && memo.seq[hlit..len] == d[..hdist] {
            return bits;
        }
    }
    memo.seq[..hlit].copy_from_slice(&ll[..hlit]);
    memo.seq[hlit..len].copy_from_slice(&d[..hdist]);
    let bits = header_size(&memo.seq[..len], &mut memo.cl, &mut memo.last_cl);
    memo.last = Some((len, bits));
    bits
}

/// The header size of a length sequence; the code-length code is worked out again only when its counts differ
/// from `last`.
fn header_size(seq: &[u8], clc: &mut LengthCoder<19>, last: &mut Option<([u32; 19], u64)>) -> u64 {
    let mut f = [0u32; 19];
    let mut extra = 0u64;
    let mut i = 0;
    while i < seq.len() {
        let v = seq[i];
        let run = run_length(seq, i);
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
    if let Some((lf, bits)) = *last {
        if lf == f {
            return bits + extra;
        }
    }
    clc.lengths(&f, 7);
    let cl = &mut clc.lens;
    if cl.iter().filter(|&&x| x != 0).count() == 1 {
        cl[16] = 1;
    }
    let hclen = (CL_ORDER.iter().rposition(|&s| cl[s] != 0).map_or(0, |p| p + 1)).max(4);
    let mut bits = 14 + 3 * hclen as u64;
    for s in 0..19 {
        bits += f[s] as u64 * cl[s] as u64;
    }
    *last = Some((f, bits));
    bits + extra
}

/// Most lengths a header can send: 288 literal/length codes and 32 distance codes.
const MAX_LENS: usize = 288 + 32;

/// The lengths a header sends as runs of equal lengths, worked out once per header search.
struct Seq {
    /// (length, how many in a row), in order.
    runs: [(u8, u16); MAX_LENS],
    nruns: usize,
    /// The longest run of zeros.
    zeros: usize,
}

impl Seq {
    fn new(lens: &[u8]) -> Seq {
        debug_assert!(lens.len() <= MAX_LENS);
        let mut s = Seq { runs: [(0, 0); MAX_LENS], nruns: 0, zeros: 0 };
        for run in lens[..lens.len().min(MAX_LENS)].chunk_by(|a, b| a == b) {
            s.runs[s.nruns] = (run[0], run.len() as u16);
            s.nruns += 1;
            if run[0] == 0 {
                s.zeros = s.zeros.max(run.len());
            }
        }
        s
    }
}

/// hlit, hdist and the lengths they send. Lengths past the end of a slice count as 0; a header sends at most
/// 288 and 32.
fn sequence(ll: &[u8], d: &[u8]) -> (usize, usize, Seq) {
    let ll = &ll[..ll.len().min(288)];
    let d = &d[..d.len().min(32)];
    let hlit = (ll.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1)).max(257);
    let hdist = (d.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1)).max(1);
    let mut lens = [0u8; MAX_LENS];
    let (nl, nd) = (ll.len().min(hlit), d.len().min(hdist));
    lens[..nl].copy_from_slice(&ll[..nl]);
    lens[hlit..hlit + nd].copy_from_slice(&d[..nd]);
    (hlit, hdist, Seq::new(&lens[..hlit + hdist]))
}

fn tok_extra_bits(sym: u8) -> u64 {
    match sym {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    }
}

/// Room for the code-length tokens of one length sequence: (symbol 0 to 18, the extra bits of 16 to 18).
/// Each token stands for one length or more, so a sequence never has more tokens than lengths.
type Tokens = [(u8, u8); MAX_LENS];

/// Zlib-style run-length tokens of a length sequence, written into `out`.
fn greedy_tokens<'a>(seq: &Seq, use16: bool, use17: bool, use18: bool, out: &'a mut Tokens) -> &'a [(u8, u8)] {
    let mut k = 0;
    for &(v, run) in &seq.runs[..seq.nruns] {
        let run = run as usize;
        if v == 0 && (use17 || use18) && run >= 3 {
            let mut r = run;
            while r >= 3 {
                if use18 && r >= 11 {
                    let m = r.min(138);
                    out[k] = (18, (m - 11) as u8);
                    k += 1;
                    r -= m;
                } else if use17 {
                    let m = r.min(10);
                    out[k] = (17, (m - 3) as u8);
                    k += 1;
                    r -= m;
                } else {
                    break;
                }
            }
            out[k..k + r].fill((0, 0));
            k += r;
        } else if v != 0 && use16 && run >= 4 {
            out[k] = (v, 0);
            k += 1;
            let mut r = run - 1;
            while r >= 3 {
                let m = r.min(6);
                out[k] = (16, (m - 3) as u8);
                k += 1;
                r -= m;
            }
            out[k..k + r].fill((v, 0));
            k += r;
        } else {
            out[k..k + run].fill((v, 0));
            k += run;
        }
    }
    &out[..k]
}

/// Cheapest tokens for a length sequence given the cost of each code-length symbol: a shortest path over
/// positions, where among equal costs the earliest step found is kept. The tokens are written into `out`.
///
/// No token crosses from one run of equal lengths into the next (16 repeats the length before it and can't
/// start a run; 17 and 18 send zeros), so the path goes through each run on its own, and its way through a
/// run depends only on the run's length and the cost of its value.
fn dp_tokens<'a>(seq: &Seq, cost: &[u32; 19], out: &'a mut Tokens) -> &'a [(u8, u8)] {
    let mut best = [0u32; MAX_LENS + 1];
    let mut zeros = [(0u8, 0u8); MAX_LENS + 1];
    let mut steps = [0u8; MAX_LENS + 1];
    if seq.zeros >= 3 {
        zero_steps(seq.zeros, cost, &mut best, &mut zeros);
    }
    // Walk back from the end, filling `out` from its end.
    let mut k = out.len();
    for &(v, len) in seq.runs[..seq.nruns].iter().rev() {
        let len = len as usize;
        // Runs too short for 16 or 17 are sent one length at a time.
        if len < 3 || (v != 0 && len < 4) {
            out[k - len..k].fill((v, 0));
            k -= len;
            continue;
        }
        let mut x = len;
        if v == 0 {
            while x > 0 {
                let (s, e) = zeros[x];
                k -= 1;
                out[k] = (s, e);
                x -= match s {
                    16 | 17 => 3 + e as usize,
                    18 => 11 + e as usize,
                    _ => 1,
                };
            }
        } else {
            value_steps(v, len, cost, &mut best, &mut steps);
            while x > 0 {
                let r = steps[x];
                k -= 1;
                out[k] = if r == 1 { (v, 0) } else { (16, r - 3) };
                x -= r as usize;
            }
        }
    }
    &out[k..]
}

/// The starts of one symbol in a window that slides along a run, with the cost of reaching each, kept
/// cheapest first (the earliest of equal costs): a start is dropped when a later one is cheaper. The window
/// never holds more than N starts, so a ring of N holds it.
struct Window<const N: usize> {
    q: [(u16, u32); N],
    head: usize,
    tail: usize,
}

impl<const N: usize> Window<N> {
    fn new() -> Self {
        Window { q: [(0, 0); N], head: 0, tail: 0 }
    }

    fn push(&mut self, j: usize, c: u32) {
        while self.tail > self.head && self.q[(self.tail - 1) % N].1 > c {
            self.tail -= 1;
        }
        self.q[self.tail % N] = (j as u16, c);
        self.tail += 1;
    }

    /// The cheapest start from `lo` on, with its cost; the last start pushed must be at `lo` or later.
    fn first(&mut self, lo: usize) -> (usize, u32) {
        while (self.q[self.head % N].0 as usize) < lo {
            self.head += 1;
        }
        let (j, c) = self.q[self.head % N];
        (j as usize, c)
    }
}

/// The cheapest way through a run of zeros, for each run length up to `len`: the last token on the way to x
/// zeros. Among equal costs the first step in the order of a search over the whole sequence is kept: symbol
/// 18, then 16 and 17 by start (16 first from the same start), then a single zero.
fn zero_steps(len: usize, cost: &[u32; 19], best: &mut [u32; MAX_LENS + 1], from: &mut [(u8, u8); MAX_LENS + 1]) {
    let (c0, c16, c17, c18) = (cost[0], cost[16] + 2, cost[17] + 3, cost[18] + 7);
    // A path costs at most 15 bits per token, so u32 never overflows.
    best[0] = 0;
    // The starts of 18 (11 to 138 zeros), 17 (3 to 10) and 16 (3 to 6, not from the start of the run). Each
    // window holds at most one start more than its width before the oldest is dropped.
    let (mut w18, mut w17, mut w16) = (Window::<256>::new(), Window::<16>::new(), Window::<8>::new());
    // Lengths in a row, from 11 on, that cost c18. Up to 138, one 18 from the start of the run costs c18.
    let mut flat = 0;
    let mut x = 1;
    while x <= len {
        let (mut c, mut f) = (u32::MAX, (0, 0));
        if x >= 11 {
            w18.push(x - 11, best[x - 11]);
            let (j, b) = w18.first(x.saturating_sub(138));
            (c, f) = (b + c18, (18, (x - j - 11) as u8));
        }
        if x >= 3 {
            w17.push(x - 3, best[x - 3]);
            let (j, b) = w17.first(x.saturating_sub(10));
            let mut g = (b + c17, (17, (x - j - 3) as u8));
            if x >= 4 {
                w16.push(x - 3, best[x - 3]);
                let (i, b) = w16.first(x.saturating_sub(6));
                // The earlier start wins a tie; from the same start, 16 comes first.
                if b + c16 < g.0 || (b + c16 == g.0 && i <= j) {
                    g = (b + c16, (16, (x - i - 3) as u8));
                }
            }
            if g.0 < c {
                (c, f) = g;
            }
        }
        if best[x - 1] + c0 < c {
            (c, f) = (best[x - 1] + c0, (0, 0));
        }
        best[x] = c;
        from[x] = f;
        flat = if x >= 11 && c == c18 { flat + 1 } else { 0 };
        x += 1;
        if flat == 10 && x <= len.min(138) {
            // Every other step to x starts at one of the last ten lengths and costs more than c18, and so on up
            // to 138: there one 18 from the start of the run is the cheapest step.
            let end = len.min(138);
            while x <= end {
                w18.push(x - 11, best[x - 11]);
                best[x] = c18;
                from[x] = (18, (x - 11) as u8);
                x += 1;
            }
            // The windows of 17 and 16 hold only the last few starts: set them up as the next length expects.
            if x <= len {
                (w17, w16) = (Window::new(), Window::new());
                for j in x - 10..x - 3 {
                    w17.push(j, best[j]);
                }
                for j in x - 6..x - 3 {
                    w16.push(j, best[j]);
                }
            }
            flat = 0;
        }
    }
}

/// The same for a run of `len` lengths `v` (not zero): 16 by start, then a single length. The last step to x
/// is kept as its length (1, or 3 to 6 for 16), since a length `v` of 16 to 18 would read as a repeat.
fn value_steps(v: u8, len: usize, cost: &[u32; 19], best: &mut [u32; MAX_LENS + 1], step: &mut [u8; MAX_LENS + 1]) {
    let (c1, c16) = (cost[v as usize], cost[16] + 2);
    best[0] = 0;
    for x in 1..=len {
        let (mut c, mut r) = (u32::MAX, 0);
        // The first length of a run can't be repeated by 16.
        if x >= 4 {
            for i in x.saturating_sub(6).max(1)..=x - 3 {
                if best[i] + c16 < c {
                    (c, r) = (best[i] + c16, x - i);
                }
            }
        }
        if best[x - 1] + c1 < c {
            (c, r) = (best[x - 1] + c1, 1);
        }
        best[x] = c;
        step[x] = r as u8;
    }
}

/// The code-length code for tokens with these symbol counts.
fn cl_lengths(f: &[u32; 19]) -> [u8; 19] {
    let mut cl = [0u8; 19];
    limited_lengths_into(f, 7, &mut cl);
    // The code-length code must be complete: with one symbol, add a partner.
    if cl.iter().filter(|&&x| x != 0).count() == 1 {
        if let Some(other) = CL_ORDER.iter().copied().find(|&s| cl[s] == 0) {
            cl[other] = 1;
        }
    }
    cl
}

/// hclen and the header's bits for a code-length code and the symbol counts of the tokens.
fn header_bits(cl: &[u8; 19], f: &[u32; 19]) -> (usize, u64) {
    let hclen = (CL_ORDER.iter().rposition(|&s| cl[s] != 0).map_or(0, |p| p + 1)).max(4);
    let mut bits = 14 + 3 * hclen as u64;
    for (s, (&l, &c)) in cl.iter().zip(f).enumerate() {
        bits += c as u64 * (l as u64 + tok_extra_bits(s as u8));
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

/// A token list met in a header search.
struct Met {
    /// Its tokens, how many, and a fingerprint of them.
    tokens: Tokens,
    len: usize,
    hash: u64,
    /// The earliest round it was reached in (usize::MAX: none yet). It is priced when first reached.
    round: usize,
    /// Once priced: how often each symbol is used, the code-length code, and the header's hclen and bits.
    priced: bool,
    f: [u32; 19],
    cl: [u8; 19],
    hclen: usize,
    bits: u64,
    /// The list the token search gives for its code-length code, once worked out.
    next: Option<usize>,
}

/// The distinct token lists of one header search. There can be 32 lists of 320 tokens, some 26 KB: on the
/// stack they would add as much to the stack a thread needs (and would all have to be zeroed for each search),
/// so they are in one allocation, made for the search and freed after it.
struct Lists {
    met: Vec<Met>,
}

impl Lists {
    fn tokens(&self, i: usize) -> &[(u8, u8)] {
        let m = &self.met[i];
        &m.tokens[..m.len]
    }

    /// The index of this list, added if new.
    fn find_or_add(&mut self, t: &[(u8, u8)]) -> usize {
        let hash =
            t.iter().fold(0u64, |h, &(s, e)| (h ^ (((s as u64) << 8) | e as u64)).wrapping_mul(0x0100_0000_01b3));
        if let Some(i) = self.met.iter().position(|m| m.hash == hash && m.len == t.len() && m.tokens[..m.len] == *t) {
            return i;
        }
        let mut tokens = [(0, 0); MAX_LENS];
        tokens[..t.len()].copy_from_slice(t);
        self.met.push(Met {
            tokens,
            len: t.len(),
            hash,
            round: usize::MAX,
            priced: false,
            f: [0; 19],
            cl: [0; 19],
            hclen: 0,
            bits: 0,
            next: None,
        });
        self.met.len() - 1
    }

    fn price(&mut self, i: usize) {
        let mut f = [0u32; 19];
        for &(s, _) in self.tokens(i) {
            f[s as usize] += 1;
        }
        // The code-length code and the size depend only on the symbol counts, which lists often share.
        let (cl, hclen, bits) = match self.met.iter().find(|m| m.priced && m.f == f) {
            Some(m) => (m.cl, m.hclen, m.bits),
            None => {
                let cl = cl_lengths(&f);
                let (hclen, bits) = header_bits(&cl, &f);
                (cl, hclen, bits)
            }
        };
        let m = &mut self.met[i];
        (m.priced, m.f, m.cl, m.hclen, m.bits) = (true, f, cl, hclen, bits);
    }

    /// The list that follows a priced one. It depends only on the code-length code, so lists with the same
    /// code share it and the token search runs once per code.
    fn next(&mut self, i: usize, seq: &Seq, buf: &mut Tokens) -> usize {
        if let Some(n) = self.met[i].next {
            return n;
        }
        let cl = self.met[i].cl;
        let n = match self.met.iter().find_map(|m| m.next.filter(|_| m.cl == cl)) {
            Some(n) => n,
            None => {
                // Re-tokenise for these code-length codes; unused symbols are priced at 7 bits plus a little.
                let cost = cl.map(|l| if l != 0 { l as u32 } else { 8 });
                self.find_or_add(dp_tokens(seq, &cost, buf))
            }
        };
        self.met[i].next = Some(n);
        n
    }
}

/// Starting from the greedy tokens of each flag set, re-tokenise for the code-length code of the last list,
/// for `rounds` lists, and keep the first header of the fewest bits.
fn header_search(ll: &[u8], d: &[u8], flag_sets: &[u8], rounds: usize) -> Header {
    let (hlit, hdist, seq) = sequence(ll, d);
    let mut buf: Tokens = [(0, 0); MAX_LENS];
    // Every list is kept once, priced once, and its next list worked out once. The searches from different
    // starting points often meet; a list reached before with as many rounds left (or more) is followed no
    // further, since all that comes after it was priced then. The lists are priced in the same order as
    // when every search is followed to the end, so the result is the same.
    let most = flag_sets.len() * rounds; // each search adds at most one new list per round
    let mut lists = Lists { met: Vec::with_capacity(most) };
    let mut best: Option<usize> = None;
    for &flags in flag_sets {
        let mut i = lists.find_or_add(greedy_tokens(&seq, flags & 1 != 0, flags & 2 != 0, flags & 4 != 0, &mut buf));
        for round in 0..rounds {
            let m = &mut lists.met[i];
            if m.round <= round {
                break;
            }
            let first = m.round == usize::MAX;
            m.round = round;
            if first {
                lists.price(i);
                if best.is_none_or(|b| lists.met[i].bits < lists.met[b].bits) {
                    best = Some(i);
                }
            }
            // The list after the last round would not be priced.
            if round + 1 == rounds {
                break;
            }
            let next = lists.next(i, &seq, &mut buf);
            if next == i {
                break;
            }
            i = next;
        }
    }
    let b = best.unwrap();
    let m = &lists.met[b];
    Header { hlit, hdist, hclen: m.hclen, cl: m.cl, tokens: lists.tokens(b).to_vec(), bits: m.bits }
}

/// Exact bits of a stored block sequence for `len` bytes, starting at bit offset `bitpos` (mod 8 matters).
pub fn stored_bits(len: usize, bitpos: u64) -> u64 {
    let mut pos = bitpos;
    let mut left = len;
    let start = bitpos;
    loop {
        let k = left.min(65535);
        pos += 3;
        pos = pos.div_ceil(8) * 8;
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

    /// The header search as it was before it worked in fixed buffers and skipped repeated work (a518968), on the
    /// package-merge of then, kept to show that the results are unchanged.
    pub(super) mod old {
        use super::super::super::huffman::old::limited_lengths;
        use super::*;

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
        /// Code-length tokens: (symbol 0 to 18, the extra bits of 16 to 18).
        type Tokens = Vec<(u8, u8)>;

        fn greedy_tokens(seq: &[u8], use16: bool, use17: bool, use18: bool) -> Tokens {
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
        pub fn dp_tokens(seq: &[u8], cost: &[u64; 19]) -> Vec<(u8, u8)> {
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
                vrun[i] = if seq[i] == seq[i - 1] {
                    if i + 1 < n && seq[i + 1] == seq[i] {
                        vrun[i + 1] + 1
                    } else {
                        1
                    }
                } else {
                    0
                };
            }
            // Symbol 18 (11 to 138 zeros, one price) reaches position t only from t - 138 ..= t - 11, so it is
            // settled before any other symbol can reach t (they come from t - 10 or later). Its best start is the
            // cheapest one in that window (the earliest of equal ones), kept in a queue of starts with rising cost.
            let mut win: std::collections::VecDeque<(usize, u64)> = std::collections::VecDeque::new();
            for i in 0..n {
                let b = best[i];
                if b != u64::MAX && zrun[i] >= 11 {
                    let c = b + cost[18] + 7;
                    while win.back().is_some_and(|&(_, x)| x > c) {
                        win.pop_back();
                    }
                    win.push_back((i, c));
                }
                let t = i + 11;
                if t <= n {
                    // Starts too far back, or in a run of zeros that ends before t, can't reach t (nor anything later).
                    while win.front().is_some_and(|&(j, _)| j + 138 < t || j + zrun[j] < t) {
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

        pub fn header_search(ll: &[u8], d: &[u8], flag_sets: &[u8], rounds: usize) -> Header {
            let (hlit, hdist, seq) = sequence(ll, d);
            let mut best: Option<Header> = None;
            // Token lists already tried, with what followed them. The searches from different starting points often
            // meet; a list seen before can't improve on the best (its size was compared already), and its next
            // list is known, so it is not worked out again. The result is the same as without this.
            let mut seen: Vec<(Tokens, Tokens)> = Vec::new();
            for &flags in flag_sets {
                let mut tokens = greedy_tokens(&seq, flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
                for _round in 0..rounds {
                    let nt = match seen.iter().find(|(t, _)| *t == tokens) {
                        Some((_, next)) => next.clone(),
                        None => {
                            let cl = cl_lengths(&tokens);
                            let (hclen, bits) = header_bits(&cl, &tokens);
                            let improved = best.as_ref().is_none_or(|b| bits < b.bits);
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
    }

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
            vrun[i] = if seq[i] == seq[i - 1] {
                if i + 1 < n && seq[i + 1] == seq[i] {
                    vrun[i + 1] + 1
                } else {
                    1
                }
            } else {
                0
            };
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
            // Code-length sequences with runs of zeros (some longer than 138) and of equal values. Some have
            // lengths 16 to 18, which can't be sent but must still give the same tokens.
            let n = 1 + (rng(&mut s) % 320) as usize;
            let top = if round % 5 == 4 { 19 } else { 16 };
            let mut seq = Vec::with_capacity(n);
            while seq.len() < n {
                let kind = rng(&mut s) % 4;
                let len = match kind {
                    0 => 1 + (rng(&mut s) % if round % 3 == 0 { 300 } else { 40 }) as usize,
                    1 => 1 + (rng(&mut s) % 12) as usize,
                    _ => 1,
                };
                let v = if kind == 0 { 0 } else { (rng(&mut s) % top) as u8 };
                for _ in 0..len.min(n - seq.len()) {
                    seq.push(v);
                }
            }
            // Costs with many ties (small range) or spread out.
            let mut cost = [0u64; 19];
            for c in cost.iter_mut() {
                *c = if round % 2 == 0 { 1 + rng(&mut s) % 3 } else { 1 + rng(&mut s) % 9 };
            }
            let want = dp_tokens_reference(&seq, &cost);
            let mut out = [(0, 0); MAX_LENS];
            let got = dp_tokens(&Seq::new(&seq), &cost.map(|c| c as u32), &mut out);
            assert_eq!(got, &want[..], "round {round}: {seq:?} {cost:?}");
            assert_eq!(old::dp_tokens(&seq, &cost), want, "round {round}: {seq:?} {cost:?}");
        }
    }

    /// Random lengths in runs: zeros (some longer than 138 when `long`), equal values, single values.
    fn runs(s: &mut u64, out: &mut [u8], long: bool) {
        let mut i = 0;
        while i < out.len() {
            let kind = rng(s) % 4;
            let len = match kind {
                0 => 1 + (rng(s) % if long { 300 } else { 40 }) as usize,
                1 => 1 + (rng(s) % 12) as usize,
                _ => 1,
            };
            let v = if kind == 0 { 0 } else { (rng(s) % 16) as u8 };
            let end = (i + len).min(out.len());
            out[i..end].fill(v);
            i = end;
        }
    }

    /// Code lengths of many shapes for case `k`: from random histograms (some smoothed into runs), raw runs,
    /// lone symbols, empty distance codes, and hlit and hdist at their limits.
    fn lengths(s: &mut u64, k: usize) -> (Vec<u8>, Vec<u8>) {
        let mut ll = vec![0u8; LITLEN_SYMS];
        let mut d = vec![0u8; DIST_SYMS];
        match k % 6 {
            0 | 1 => {
                let mut h = Hist::empty();
                let keep = 1 + rng(s) % 6;
                let top = [3, 1000, 100_000][(rng(s) % 3) as usize];
                for c in h.ll.iter_mut().chain(h.d.iter_mut()) {
                    if rng(s).is_multiple_of(keep) {
                        *c = (rng(s) % top) as u32;
                    }
                }
                let (lc, dc) =
                    if k % 6 == 1 { (smooth(&h.ll, 0.5), smooth(&h.d, 0.5)) } else { (h.ll.to_vec(), h.d.to_vec()) };
                let maxb = [15, 12, 10, 9][(rng(s) % 4) as usize];
                ll = limited_lengths(&lc, maxb);
                d = limited_lengths(&dc, maxb);
                if rng(s).is_multiple_of(2) {
                    patch_litlen(&mut ll);
                    patch_distances(&mut d);
                }
            }
            2 => {
                let long = rng(s).is_multiple_of(2);
                runs(s, &mut ll, long);
                runs(s, &mut d, long);
            }
            3 => {
                ll[(rng(s) % LITLEN_SYMS as u64) as usize] = 1 + (rng(s) % 15) as u8;
                if rng(s).is_multiple_of(2) {
                    d[(rng(s) % DIST_SYMS as u64) as usize] = 1 + (rng(s) % 15) as u8;
                }
            }
            4 => runs(s, &mut ll, true),
            _ => {
                let long = rng(s).is_multiple_of(2);
                runs(s, &mut ll, long);
                runs(s, &mut d, false);
                match rng(s) % 3 {
                    0 => ll[257..].fill(0),
                    1 => ll[LITLEN_SYMS - 1] = 1 + (rng(s) % 15) as u8,
                    _ => ll.fill(0),
                }
                if rng(s).is_multiple_of(2) {
                    d[DIST_SYMS - 1] = 1 + (rng(s) % 15) as u8;
                }
                // The most a header can send: 288 and 32 lengths.
                if rng(s).is_multiple_of(4) {
                    ll.extend([0, 1 + (rng(s) % 15) as u8]);
                    d.extend([0, 1 + (rng(s) % 15) as u8]);
                }
            }
        }
        (ll, d)
    }

    /// The lengths a header's tokens stand for.
    fn expand(h: &Header) -> Vec<u8> {
        let mut lens: Vec<u8> = Vec::new();
        for &(sym, e) in &h.tokens {
            match sym {
                16 => {
                    let last = *lens.last().unwrap();
                    lens.extend(std::iter::repeat_n(last, 3 + e as usize));
                }
                17 => lens.extend(std::iter::repeat_n(0, 3 + e as usize)),
                18 => lens.extend(std::iter::repeat_n(0, 11 + e as usize)),
                v => lens.push(v),
            }
        }
        lens
    }

    /// The headers of best_header, quick_header and header_search (random flags and rounds) for these lengths,
    /// checked field by field against the old search that followed every strategy to its end.
    fn same_as_old(s: &mut u64, k: usize, ll: &[u8], d: &[u8]) -> [Header; 3] {
        let flags: Vec<u8> = (0..1 + rng(s) % 10).map(|_| (rng(s) % 8) as u8).collect();
        let rounds = 1 + (rng(s) % 6) as usize;
        let pairs = [
            (best_header(ll, d), old::best_header(ll, d)),
            (quick_header(ll, d), old::quick_header(ll, d)),
            (header_search(ll, d, &flags, rounds), old::header_search(ll, d, &flags, rounds)),
        ];
        for (new, old) in &pairs {
            assert_eq!(
                (new.hlit, new.hdist, new.hclen, new.cl, new.bits, &new.tokens),
                (old.hlit, old.hdist, old.hclen, old.cl, old.bits, &old.tokens),
                "case {k}, flags {flags:?}, rounds {rounds}: {ll:?} {d:?}"
            );
        }
        pairs.map(|(new, _)| new)
    }

    #[test]
    fn header_search_is_unchanged_by_the_memory_of_tried_tokens() {
        let mut s = 77u64;
        for k in 0..3000 {
            let (ll, d) = lengths(&mut s, k);
            for new in &same_as_old(&mut s, k, &ll, &d) {
                let lens = expand(new);
                assert_eq!(lens[..new.hlit], ll[..new.hlit]);
                assert_eq!(lens[new.hlit..], d[..new.hdist]);
            }
        }
        // Every header the search returns must describe the lengths it was given.
        for _ in 0..200 {
            let mut h = Hist::empty();
            for i in 0..LITLEN_SYMS {
                if rng(&mut s).is_multiple_of(3) {
                    h.ll[i] = (rng(&mut s) % 1000) as u32;
                }
            }
            for i in 0..DIST_SYMS {
                if rng(&mut s).is_multiple_of(2) {
                    h.d[i] = (rng(&mut s) % 300) as u32;
                }
            }
            for code in [best_dynamic(&h), good_dynamic(&h)] {
                let lens = expand(&code.header);
                let (hl, hd) = (code.header.hlit, code.header.hdist);
                assert_eq!(lens.len(), hl + hd);
                assert_eq!(&lens[..hl], &code.ll[..hl]);
                assert_eq!(&lens[hl..], &code.d[..hd]);
            }
        }
    }

    #[test]
    fn lengths_16_to_18_give_the_old_header() {
        // Such lengths can't be sent and the encoder never makes them, but the search must not panic on them.
        let mut ll = vec![0u8; LITLEN_SYMS];
        ll[10..20].fill(16);
        ll[256] = 1;
        same_as_old(&mut 5u64, 0, &ll, &[1, 1]);
        let mut s = 99u64;
        for k in 0..600 {
            let (mut ll, mut d) = lengths(&mut s, k);
            // Some values become 16, 17 or 18, keeping the runs.
            for to in 16..19 {
                let from = 1 + (rng(&mut s) % 15) as u8;
                for l in ll.iter_mut().chain(d.iter_mut()).filter(|l| **l == from) {
                    *l = to;
                }
            }
            same_as_old(&mut s, k, &ll, &d);
        }
    }
}

#[cfg(test)]
mod estimate_tests {
    use super::super::huffman::{old, tests::walk};
    use super::*;

    /// The estimate as it was before `Estimator`, on the old length code.
    fn old_estimate_dynamic_bits(h: &Hist) -> u64 {
        let mut ll = [0u8; LITLEN_SYMS];
        let mut d = [0u8; DIST_SYMS];
        old::lengths_into(&h.ll, 15, &mut ll);
        old::lengths_into(&h.d, 15, &mut d);
        patch_litlen(&mut ll);
        patch_distances(&mut d);
        data_bits(h, &ll, &d) + old_fast_header_bits(&ll, &d)
    }

    fn old_fast_header_bits(ll: &[u8], d: &[u8]) -> u64 {
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
        old::lengths_into(&f, 7, &mut cl);
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

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// Symbols whose statistics drift along the way, like real data.
    fn random_syms(s: &mut u64, n: usize) -> Vec<Sym> {
        let mut out = Vec::with_capacity(n);
        let (mut lits, mut far) = (256, 32768);
        while out.len() < n {
            if rng(s).is_multiple_of(300) {
                lits = 1 + rng(s) % 256;
                far = 1 + rng(s) % 32768;
            }
            if rng(s).is_multiple_of(3) {
                let len = 3 + rng(s) % (1 + rng(s) % 256);
                out.push(Sym::mat(len as usize, 1 + (rng(s) % far) as usize));
            } else {
                out.push(Sym::lit((rng(s) % lits) as u8));
            }
        }
        out
    }

    #[test]
    fn estimator_gives_the_estimates_of_before() {
        let mut s = 0xe571_3a7eu64;
        // As in the block-split search: the blocks between candidate cuts, `i` walking up or down for each `j`.
        for round in 0..6 {
            let syms = random_syms(&mut s, 2000 + round * 1500);
            let g = 32 + (rng(&mut s) % 64) as usize;
            let mut pref = vec![Hist { ll: [0; LITLEN_SYMS], d: [0; DIST_SYMS] }];
            for c in syms.chunks(g) {
                let mut h = pref.last().unwrap().clone();
                c.iter().for_each(|x| h.add(x));
                pref.push(h);
            }
            let mut es = Estimator::new();
            for j in 1..pref.len() {
                let lo = j.saturating_sub(96);
                let mut is: Vec<usize> = (lo..j).collect();
                if round % 2 == 0 && j % 2 == 0 {
                    is.reverse();
                }
                for i in is {
                    let h = Hist::diff(&pref[j], &pref[i]);
                    assert_eq!(es.bits(&h), old_estimate_dynamic_bits(&h), "round {round}, {i}..{j}");
                }
            }
        }
        // Random walks over related histograms, now and then an unrelated one, with every kind of count.
        let mut es = Estimator::new();
        let mut h = Hist::empty();
        for k in 0..30_000 {
            walk(&mut s, &mut h.ll);
            walk(&mut s, &mut h.d);
            let want = old_estimate_dynamic_bits(&h);
            assert_eq!(es.bits(&h), want, "{:?} {:?}", h.ll, h.d);
            if k % 16 == 0 {
                assert_eq!(estimate_dynamic_bits(&h), want);
            }
        }
    }
}
