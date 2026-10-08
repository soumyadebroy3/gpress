//! Type 1 fonts in the gpress archive.
//!
//! A Type 1 font program (a FontFile stream; its dictionary has /Length1 /Length2 /Length3) keeps its private
//! part behind two layers of encryption: the whole part is eexec-encrypted (start value 55665), and inside it
//! every charstring and Subrs entry is encrypted again (start value 4330). Encrypted bytes look random, so both
//! layers are removed before the model codes the font, and put back on restore. Both ciphers are reversible
//! byte for byte: decrypting and encrypting again with the same start value gives back the same bytes,
//! whatever they are.
//!
//! The opened font: the clear-text part as it is, then the decrypted private part with every charstring and
//! Subrs entry decrypted in place (same length), then the rest (zeros, cleartomark) as it is. The hex form of
//! eexec is opened too when its lines are regular (the same number of digits on every line, one kind of line
//! end, one letter case), so the same text can be written back; otherwise the font stays as it is. The encoder
//! closes every opened font again and opens it only if that gives back exactly the original bytes.

use super::{find as find_from, reserve, Io, PackStats};
use crate::cm::model::{Geom, Kind};
use crate::cm::Coder;
use crate::pdf::object::{is_delim, is_white, Dict, Obj};
use std::borrow::Cow;

const EEXEC: u16 = 55665;
const CHARSTRING: u16 = 4330;

#[inline]
fn step(r: u16, c: u8) -> u16 {
    (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719)
}

/// Removes one layer of Type 1 encryption in place (`key`: 55665 for eexec, 4330 for charstrings).
pub fn decrypt(buf: &mut [u8], key: u16) {
    let mut r = key;
    for b in buf.iter_mut() {
        let c = *b;
        *b = c ^ (r >> 8) as u8;
        r = step(r, c);
    }
}

/// Puts one layer of Type 1 encryption back in place: the inverse of `decrypt` with the same key.
pub fn encrypt(buf: &mut [u8], key: u16) {
    let mut r = key;
    for b in buf.iter_mut() {
        let c = *b ^ (r >> 8) as u8;
        *b = c;
        r = step(r, c);
    }
}

/// Where the encrypted private part of a font program is, and how it is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Offset of the first encrypted byte (or of its first hex digit).
    pub start: usize,
    /// Number of encrypted bytes (in the hex form: the bytes its digits stand for).
    pub n: usize,
    /// The hex form's line layout; None for the binary form.
    pub hex: Option<Hex>,
}

/// A regular line layout of hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hex {
    /// Letters written A-F (else a-f).
    pub upper: bool,
    /// Digits per line; 0: a single line.
    pub line: usize,
    /// Line end: 0 LF, 1 CR, 2 CR LF.
    pub eol: u8,
    /// The last line ends with a line end too.
    pub last_eol: bool,
}

const EOLS: [&[u8]; 3] = [b"\n", b"\r", b"\r\n"];

impl Hex {
    /// Length of the text for `n` bytes (None if it doesn't fit a usize, or for an unknown line end). Checked
    /// arithmetic: a damaged file can ask for any `n`.
    fn text_len(&self, n: usize) -> Option<usize> {
        let eol = EOLS.get(self.eol as usize)?.len();
        let digits = n.checked_mul(2)?;
        let breaks = if self.line > 0 && digits > 0 { (digits - 1) / self.line } else { 0 } + self.last_eol as usize;
        breaks.checked_mul(eol)?.checked_add(digits)
    }

    fn text(&self, bytes: &[u8]) -> Vec<u8> {
        let digits: &[u8; 16] = if self.upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
        let eol = EOLS[self.eol as usize];
        let mut out = Vec::with_capacity(bytes.len() * 2 + bytes.len() / 16 + 2);
        let mut k = 0;
        for &b in bytes {
            for d in [b >> 4, b & 15] {
                if self.line > 0 && k > 0 && k % self.line == 0 {
                    out.extend_from_slice(eol);
                }
                out.push(digits[d as usize]);
                k += 1;
            }
        }
        if self.last_eol {
            out.extend_from_slice(eol);
        }
        out
    }
}

impl Layout {
    /// Length of the encrypted part as written in the font program.
    pub fn text_len(&self) -> Option<usize> {
        match &self.hex {
            None => Some(self.n),
            Some(h) => h.text_len(self.n),
        }
    }
}

/// The font program with both layers of encryption removed (see the module notes); None if the layout
/// doesn't fit it.
pub fn open(font: &[u8], l: &Layout) -> Option<Vec<u8>> {
    let end = l.start.checked_add(l.text_len()?)?;
    let text = font.get(l.start..end)?;
    let mut p = match l.hex {
        None => text.to_vec(),
        Some(_) => unhex(text, l.n)?,
    };
    decrypt(&mut p, EEXEC);
    for (o, n) in charstrings(&p) {
        decrypt(&mut p[o..o + n], CHARSTRING);
    }
    let mut t = Vec::with_capacity(font.len());
    t.extend_from_slice(&font[..l.start]);
    t.extend_from_slice(&p);
    t.extend_from_slice(&font[end..]);
    Some(t)
}

/// Puts both layers of encryption back: the inverse of `open`. None if the layout doesn't fit.
pub fn close(t: &[u8], l: &Layout) -> Option<Vec<u8>> {
    l.text_len()?;
    let end = l.start.checked_add(l.n)?;
    let mut p = t.get(l.start..end)?.to_vec();
    for (o, n) in charstrings(&p) {
        encrypt(&mut p[o..o + n], CHARSTRING);
    }
    encrypt(&mut p, EEXEC);
    let mut f = Vec::with_capacity(t.len() + l.n + 16);
    f.extend_from_slice(&t[..l.start]);
    match &l.hex {
        None => f.extend_from_slice(&p),
        Some(h) => f.extend_from_slice(&h.text(&p)),
    }
    f.extend_from_slice(&t[end..]);
    Some(f)
}

/// The charstrings and Subrs entries in a decrypted private part, as (offset, length), when their own
/// encryption is on (lenIV not negative). Each is a binary string read by RD (or -|): "length RD ", then that
/// many bytes. Only the text around them is read, which their own encryption doesn't change, so `open` and
/// `close` find the same strings.
pub fn charstrings(p: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut len_iv: i64 = 4;
    // The private part starts with 4 random bytes.
    let mut i = p.len().min(4);
    let mut prev: &[u8] = b"";
    while i < p.len() {
        let c = p[i];
        if is_white(c) {
            i += 1;
            continue;
        }
        let s = i;
        match c {
            b'%' => {
                while i < p.len() && p[i] != b'\r' && p[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'(' => i = skip_string(p, i),
            b'<' if p.get(i + 1) == Some(&b'<') => i += 2,
            b'<' => i = p[i..].iter().position(|&x| x == b'>').map_or(p.len(), |k| i + k + 1),
            b'>' if p.get(i + 1) == Some(&b'>') => i += 2,
            b'/' => {
                i += 1;
                while i < p.len() && !is_white(p[i]) && !is_delim(p[i]) {
                    i += 1;
                }
            }
            _ if is_delim(c) => i += 1,
            _ => {
                while i < p.len() && !is_white(p[i]) && !is_delim(p[i]) {
                    i += 1;
                }
            }
        }
        let tok = &p[s..i];
        // The binary string starts after the one white-space byte that ends the RD token.
        if (tok == b"RD" || tok == b"-|") && i < p.len() && is_white(p[i]) {
            if let Some(n) = number(prev).filter(|&v| v >= 0) {
                let start = i + 1;
                let n = n as usize;
                if n > p.len() - start {
                    break;
                }
                if len_iv >= 0 && n > 0 {
                    out.push((start, n));
                }
                i = start + n;
                prev = b"";
                continue;
            }
        }
        if prev == b"/lenIV" {
            if let Some(v) = number(tok) {
                len_iv = v;
            }
        }
        prev = tok;
    }
    out
}

/// A literal string "( ... )" with nested parentheses and backslash escapes: the offset just past it.
fn skip_string(p: &[u8], mut i: usize) -> usize {
    let mut depth = 0u32;
    while i < p.len() {
        match p[i] {
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    p.len()
}

/// A small decimal integer token (optional sign, at most 9 digits).
fn number(t: &[u8]) -> Option<i64> {
    let (neg, digits) = match t.first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.len() > 9 || !digits.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let v = digits.iter().fold(0i64, |v, &c| v * 10 + (c - b'0') as i64);
    Some(if neg { -v } else { v })
}

/// Decrypted bytes that must be text for a layout to be taken.
const MIN_TEXT: usize = 32;

fn texty(b: &[u8]) -> bool {
    b.iter().all(|&c| matches!(c, 9 | 10 | 12 | 13 | 32..=126))
}

fn rfind(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).rposition(|w| w == n)
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// The bytes of a hex text (white space between digits ignored); None unless there are exactly `n`.
fn unhex(text: &[u8], n: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(n.min(text.len() / 2));
    let mut hi: Option<u8> = None;
    for &c in text {
        let v = match hex_value(c) {
            Some(v) => v,
            None if is_white(c) => continue,
            None => return None,
        };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => out.push(h << 4 | v),
        }
    }
    (hi.is_none() && out.len() == n).then_some(out)
}

/// Encoder side: the layout of a font program's encrypted part, if it has one that decrypts to text. `len1` and
/// `len2` are the dictionary's /Length1 and /Length2 (0 when unknown), used as hints only.
pub fn find(font: &[u8], len1: usize, len2: usize) -> Option<Layout> {
    let mut starts = Vec::new();
    if len1 > 0 {
        starts.push(len1);
    }
    // After "eexec": past all the white space that follows it, and past one or two bytes of it only (the first
    // encrypted byte may look like white space).
    let mut from = 0;
    for _ in 0..4 {
        let Some(k) = find_from(font, b"eexec", from) else { break };
        let a = k + 5;
        let mut s = a;
        while s < font.len() && is_white(font[s]) {
            s += 1;
        }
        starts.extend([s, a + 1, a + 2].into_iter().filter(|&x| x <= s));
        from = a;
    }
    let mut seen = Vec::new();
    for s in starts {
        if seen.contains(&s) {
            continue;
        }
        seen.push(s);
        // A PFB file: a binary segment header (128, 2, length) before the encrypted bytes.
        let (s, limit) = match font.get(s..s + 6) {
            Some(h) if h[0] == 0x80 && h[1] == 2 => {
                let n = u32::from_le_bytes([h[2], h[3], h[4], h[5]]) as usize;
                (s + 6, (s + 6).saturating_add(n).min(font.len()))
            }
            _ => (s, font.len()),
        };
        if let Some(l) = binary_at(font, s, limit, len2).or_else(|| hex_at(font, s)) {
            return Some(l);
        }
    }
    None
}

/// The binary form at `s` (up to `limit`): it decrypts to text after the 4 random bytes.
fn binary_at(font: &[u8], s: usize, limit: usize, len2: usize) -> Option<Layout> {
    if limit <= s || limit - s < 4 + MIN_TEXT {
        return None;
    }
    let mut head = font[s..s + 4 + MIN_TEXT].to_vec();
    decrypt(&mut head, EEXEC);
    if !texty(&head[4..]) {
        return None;
    }
    let mut p = font[s..limit].to_vec();
    decrypt(&mut p, EEXEC);
    Some(Layout { start: s, n: private_len(&p, len2), hex: None })
}

/// How much of the decrypted text is the private part: up to /Length2 when the text there ends with
/// "closefile" (as it should), else just past the last "closefile" and the white space after it, else up to
/// /Length2, else all of it.
fn private_len(p: &[u8], len2: usize) -> usize {
    let fits = len2 > 0 && len2 <= p.len();
    if fits {
        let mut e = len2;
        while e > 0 && is_white(p[e - 1]) {
            e -= 1;
        }
        if p[..e].ends_with(b"closefile") {
            return len2;
        }
    }
    if let Some(k) = rfind(p, b"closefile") {
        let mut e = k + 9;
        while e < p.len() && is_white(p[e]) {
            e += 1;
        }
        return e;
    }
    if fits {
        len2
    } else {
        p.len()
    }
}

/// The hex form at `s`: digits (and line ends) that decrypt to text, up to just past "closefile" and the white
/// space after it, in a regular line layout.
fn hex_at(font: &[u8], s: usize) -> Option<Layout> {
    font.get(s).and_then(|&c| hex_value(c))?;
    // The digits from `s` as bytes, up to the first byte that is neither a digit nor white space. The first
    // bytes must decrypt to text before the rest is read.
    let mut bytes = Vec::new();
    let mut hi: Option<u8> = None;
    for &c in &font[s..] {
        match hex_value(c) {
            Some(v) => match hi.take() {
                None => hi = Some(v),
                Some(h) => {
                    bytes.push(h << 4 | v);
                    if bytes.len() == 4 + MIN_TEXT {
                        let mut head = bytes.clone();
                        decrypt(&mut head, EEXEC);
                        if !texty(&head[4..]) {
                            return None;
                        }
                    }
                }
            },
            None if is_white(c) => {}
            None => break,
        }
    }
    if bytes.len() < 4 + MIN_TEXT {
        return None;
    }
    decrypt(&mut bytes, EEXEC);
    let k = rfind(&bytes, b"closefile")? + 9;
    let mut n = k;
    while n < bytes.len() && is_white(bytes[n]) {
        n += 1;
    }
    // The offsets just past the digits of bytes k-1 .. n-1.
    let mut ends = Vec::with_capacity(n - k + 1);
    let mut digits = 0;
    for (i, &c) in font[s..].iter().enumerate() {
        if hex_value(c).is_some() {
            digits += 1;
            if digits % 2 == 0 && digits / 2 >= k {
                ends.push(s + i + 1);
                if ends.len() == n - k + 1 {
                    break;
                }
            }
        }
    }
    // The encrypted part ends where a line does: white space decrypted from the digits after it is dropped.
    while n > k && !matches!(font.get(ends[n - k]), Some(b'\r' | b'\n')) {
        n -= 1;
    }
    let end = ends[n - k];
    let text = &font[s..end];
    let upper = text.iter().any(|c| (b'A'..=b'F').contains(c));
    // The first line end gives the line length; a single line takes its line end from what follows it.
    let w = text.iter().position(|&c| is_white(c));
    let after = &text[w.unwrap_or(text.len())..];
    let after = if w.is_some() { after } else { &font[end..] };
    let eol = if after.starts_with(b"\r\n") {
        2
    } else if after.first() == Some(&b'\r') {
        1
    } else if after.first() == Some(&b'\n') || w.is_none() {
        0
    } else {
        return None;
    };
    let last_eol = font[end..].starts_with(EOLS[eol as usize]);
    Some(Layout { start: s, n, hex: Some(Hex { upper, line: w.unwrap_or(0), eol, last_eol }) })
}

// ---------------------------------------------------------------- coding

/// Flag before a Type 1 font program: kept as it is, opened from binary eexec, opened from hex eexec.
const AS_IS: u8 = 0;
const BINARY: u8 = 1;
const HEX: u8 = 2;

/// A stream dictionary that announces a Type 1 font program.
pub(super) fn is_type1(dict: Option<&Dict>) -> bool {
    dict.is_some_and(|d| d.has(b"Length2"))
}

/// The dictionary of the unfiltered stream whose bytes follow `out`, if it announces a Type 1 font program.
/// A quick look for "/Length2" comes first, so other streams don't pay for parsing their dictionary.
pub(super) fn raw_font_dict(out: &[u8]) -> Option<Dict> {
    find_from(&out[out.len().saturating_sub(4096)..], b"/Length2", 0)?;
    super::dict_before(out, out.len().saturating_sub(1)).filter(|d| is_type1(Some(d)))
}

/// The dictionary's /Length1 and /Length2 when they are plain numbers that fit `n` bytes (else 0): hints for
/// the layout, which is coded as its difference from them.
fn hints(dict: Option<&Dict>, n: usize) -> (usize, usize) {
    let hint = |k: &[u8]| match dict.and_then(|d| d.get(k)) {
        Some(Obj::Int(v)) if *v > 0 && *v as u64 <= n as u64 => *v as usize,
        _ => 0,
    };
    (hint(b"Length1"), hint(b"Length2"))
}

/// Encoder side: the layout and the opened font, if the opened font closes back to exactly the same bytes.
fn opened(font: &[u8], len1: usize, len2: usize) -> Option<(Layout, Vec<u8>)> {
    let l = find(font, len1, len2)?;
    let t = open(font, &l)?;
    (close(&t, &l)? == font).then_some((l, t))
}

/// For measuring (`expand`): the font program opened when the stream that ends `prev` announces a Type 1
/// font, with the bytes its layout takes in a pack; as it is otherwise.
pub(super) fn expanded<'a>(prev: &[u8], data: &'a [u8]) -> (Cow<'a, [u8]>, usize) {
    let dict = super::dict_before(prev, prev.len().saturating_sub(1));
    if is_type1(dict.as_ref()) {
        let (h1, h2) = hints(dict.as_ref(), data.len());
        if let Some((_, t)) = opened(data, h1, h2) {
            return (Cow::Owned(t), 3);
        }
    }
    (Cow::Borrowed(data), 0)
}

/// Codes a Type 1 font program of `n` bytes (`src` when encoding) and appends it to `out`: opened when it
/// closes back to exactly the same bytes, as it is (model set to `plain`) otherwise. Both sides run this.
pub(super) fn code<C: Coder>(
    io: &mut Io<C>,
    src: Option<&[u8]>,
    n: usize,
    dict: Option<&Dict>,
    plain: Kind,
    out: &mut Vec<u8>,
    stats: &mut PackStats,
) {
    let (h1, h2) = hints(dict, n);
    let opened = src.map(|f| opened(f, h1, h2));
    let lay = opened.as_ref().and_then(|o| o.as_ref()).map(|(l, _)| *l);
    io.m.set_kind(Kind::Meta, Geom::default(), 0);
    let form = io.byte(opened.as_ref().map(|o| match o {
        None => AS_IS,
        Some((l, _)) if l.hex.is_some() => HEX,
        Some(_) => BINARY,
    }));
    if form == AS_IS {
        io.m.set_kind(plain, Geom::default(), 0);
        io.bytes(src, n, out);
        return;
    }
    let l = code_layout(io, lay.as_ref(), form == HEX, n, h1, h2);
    // Anything that doesn't fit the font's length can only come from a damaged file. Every real layout has its
    // start and count inside the font; that is checked before any length is worked out from them.
    let Some(l) = l.filter(|l| form <= HEX && !io.damaged && l.start <= n && l.n <= n) else {
        io.damaged = true;
        return;
    };
    let (start, count) = (l.start, l.n);
    let Some(end) = l.text_len().and_then(|x| start.checked_add(x)).filter(|&e| e <= n) else {
        io.damaged = true;
        return;
    };
    let t_len = n - end + start + count;
    let t_in = opened.as_ref().and_then(|o| o.as_ref()).map(|(_, t)| t.as_slice());
    let mut t = Vec::with_capacity(reserve(t_len));
    for (kind, a, b) in
        [(Kind::Font, 0, start), (Kind::Charstring, start, start + count), (Kind::Font, start + count, t_len)]
    {
        io.m.set_kind(kind, Geom::default(), 0);
        io.bytes(t_in.map(|x| &x[a..b]), b - a, &mut t);
        if io.damaged {
            return;
        }
    }
    match close(&t, &l) {
        Some(f) if f.len() == n => {
            out.extend_from_slice(&f);
            stats.type1 += 1;
        }
        _ => io.damaged = true,
    }
}

/// The layout of an opened font (`l` when encoding): start and count as differences from the hints, then the
/// hex form's line layout. None if a field can't be what it stands for; `code` checks the layout against the
/// font's length.
fn code_layout<C: Coder>(
    io: &mut Io<C>,
    l: Option<&Layout>,
    hex: bool,
    n: usize,
    h1: usize,
    h2: usize,
) -> Option<Layout> {
    let start = delta(io, l.map(|l| l.start), h1);
    let count = delta(io, l.map(|l| l.n), h2);
    let hex = if hex {
        let h = l.and_then(|l| l.hex);
        let upper = io.byte(h.map(|h| h.upper as u8));
        let line = io.length(h.map(|h| h.line as u64), n);
        let eol = io.byte(h.map(|h| h.eol));
        let last = io.byte(h.map(|h| h.last_eol as u8));
        if upper > 1 || eol > 2 || last > 1 {
            return None;
        }
        Some(Hex { upper: upper == 1, line, eol, last_eol: last == 1 })
    } else {
        None
    };
    Some(Layout { start: start?, n: count?, hex })
}

/// A count coded as its difference from a hint (usually 0); None if the result can't be a count.
fn delta<C: Coder>(io: &mut Io<C>, v: Option<usize>, hint: usize) -> Option<usize> {
    let d = v.map(|v| v as i64 - hint as i64);
    let z = io.varint(d.map(|d| ((d << 1) ^ (d >> 63)) as u64));
    let d = (z >> 1) as i64 ^ -((z & 1) as i64);
    usize::try_from((hint as i64).checked_add(d)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cm::model::Model;
    use crate::cm::{Decoder, Encoder};

    fn unhex_str(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// Vectors made with fontTools (fontTools.misc.eexec.encrypt), an implementation we didn't write.
    #[test]
    fn ciphers_match_known_vectors() {
        let plain = b"\x00\x00\x00\x00dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n";
        let want = unhex_str("d9d66f633b846a989b9974b0179fc6cc4452954d3a4fc272596999ba876cc696185cbab11491f08a053b187b0adb1613ea4e6a25c471c0db78b865e8f6845f9a8691983ad38c1c60b04b9cd89e6f23c5c81e5bc47a690c9c1bd2f0f746");
        let mut c = plain.to_vec();
        encrypt(&mut c, EEXEC);
        assert_eq!(c, want);
        decrypt(&mut c, EEXEC);
        assert_eq!(c, plain);
        let cs = unhex_str("000000008bef0d7692010e");
        let mut c = cs.clone();
        encrypt(&mut c, CHARSTRING);
        assert_eq!(c, unhex_str("10bf317079df3e7502f1cc"));
        decrypt(&mut c, CHARSTRING);
        assert_eq!(c, cs);
    }

    #[test]
    fn decrypt_then_encrypt_gives_back_any_bytes() {
        let mut x: u32 = 99;
        let data: Vec<u8> = (0..5000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        for key in [EEXEC, CHARSTRING, 0, 65535] {
            let mut b = data.clone();
            decrypt(&mut b, key);
            assert_ne!(b, data);
            encrypt(&mut b, key);
            assert_eq!(b, data, "key {key}");
        }
    }

    /// A small Type 1 font: (font program, its opened form, /Length1, /Length2). `len_iv`: None leaves /lenIV
    /// out (default 4). `rd`: "RD" or "-|".
    fn font(len_iv: Option<i64>, rd: &str, glyphs: usize) -> (Vec<u8>, Vec<u8>, usize, usize) {
        let (nd, np) = if rd == "RD" { ("ND", "NP") } else { ("|-", "|") };
        let skip = len_iv.unwrap_or(4);
        let charstring = |k: usize| -> (Vec<u8>, Vec<u8>) {
            // lenIV random bytes, then: 0 500 hsbw, k k rlineto (small numbers), closepath, endchar.
            let mut plain: Vec<u8> = (0..skip.max(0) as usize).map(|j| (j * 37 + 11) as u8).collect();
            plain.extend_from_slice(&[139, 247, 0x88, 13]);
            for j in 0..(k % 7 + 2) {
                plain.extend_from_slice(&[139 + (j as u8 % 50), 139 - (k as u8 % 40), 5]);
            }
            plain.extend_from_slice(&[9, 14]);
            let mut enc = plain.clone();
            if skip >= 0 {
                encrypt(&mut enc, CHARSTRING);
            }
            (plain, enc)
        };
        let clear = b"%!PS-AdobeFont-1.0: Test 001.000\n/FontName /Test def\n/Encoding StandardEncoding def\ncurrentdict end\ncurrentfile eexec\n".to_vec();
        let (mut p_plain, mut p_enc): (Vec<u8>, Vec<u8>) = (vec![0, 0, 0, 0], vec![0, 0, 0, 0]);
        let text = |s: &str, a: &mut Vec<u8>, b: &mut Vec<u8>| {
            a.extend_from_slice(s.as_bytes());
            b.extend_from_slice(s.as_bytes());
        };
        text(&format!("dup /Private 8 dict dup begin\n/{rd}{{string currentfile exch readstring pop}}executeonly def\n/{nd}{{noaccess def}}executeonly def\n/{np}{{noaccess put}}executeonly def\n"), &mut p_plain, &mut p_enc);
        if let Some(v) = len_iv {
            text(&format!("/lenIV {v} def\n"), &mut p_plain, &mut p_enc);
        }
        text("/MinFeature{16 16} def\n/password 5839 def\n/Subrs 3 array\n", &mut p_plain, &mut p_enc);
        for k in 0..3 {
            let (pl, en) = charstring(k + 100);
            text(&format!("dup {k} {} {rd} ", pl.len()), &mut p_plain, &mut p_enc);
            p_plain.extend_from_slice(&pl);
            p_enc.extend_from_slice(&en);
            text(&format!(" {np}\n"), &mut p_plain, &mut p_enc);
        }
        text(&format!("ND\n2 index /CharStrings {glyphs} dict dup begin\n"), &mut p_plain, &mut p_enc);
        for k in 0..glyphs {
            let (pl, en) = charstring(k);
            text(&format!("/g{k} {} {rd} ", pl.len()), &mut p_plain, &mut p_enc);
            p_plain.extend_from_slice(&pl);
            p_enc.extend_from_slice(&en);
            text(&format!(" {nd}\n"), &mut p_plain, &mut p_enc);
        }
        text(
            "end\nend\nreadonly put\nnoaccess put\ndup/FontName get exch definefont pop\nmark currentfile closefile\n",
            &mut p_plain,
            &mut p_enc,
        );
        let mut cipher = p_enc;
        encrypt(&mut cipher, EEXEC);
        let mut trailer = Vec::new();
        for _ in 0..8 {
            trailer.extend_from_slice(&[b'0'; 64]);
            trailer.push(b'\n');
        }
        trailer.extend_from_slice(b"cleartomark\n");
        let f = [&clear[..], &cipher, &trailer].concat();
        let t = [&clear[..], &p_plain, &trailer].concat();
        (f, t, clear.len(), cipher.len())
    }

    fn check_opens(f: &[u8], want: &[u8], l1: usize, l2: usize) -> Layout {
        let l = find(f, l1, l2).expect("layout");
        let t = open(f, &l).expect("open");
        assert_eq!(t, want);
        assert_eq!(close(&t, &l).unwrap(), f);
        l
    }

    #[test]
    fn opens_binary_eexec_and_charstrings() {
        for (len_iv, rd) in
            [(None, "RD"), (Some(4), "-|"), (Some(0), "RD"), (Some(1), "RD"), (Some(3), "-|"), (Some(-1), "RD")]
        {
            let (f, t, l1, l2) = font(len_iv, rd, 40);
            let l = check_opens(&f, &t, l1, l2);
            assert_eq!(l, Layout { start: l1, n: l2, hex: None }, "{len_iv:?} {rd}");
            // Wrong or missing dictionary lengths: found from the bytes.
            assert_eq!(check_opens(&f, &t, 0, 0), l);
            assert_eq!(check_opens(&f, &t, l1 - 3, l2 + 1000), l);
        }
    }

    #[test]
    fn len_iv_minus_one_leaves_charstrings_as_they_are() {
        let (f, t, l1, l2) = font(Some(-1), "RD", 5);
        check_opens(&f, &t, l1, l2);
        // The opened charstrings are the stored ones: plain already, never decrypted.
        assert!(charstrings(&t[l1..l1 + l2]).is_empty());
        assert_eq!(&f[..l1], &t[..l1]);
    }

    #[test]
    fn finds_every_charstring_and_nothing_else() {
        let (_, t, l1, l2) = font(None, "RD", 12);
        let p = &t[l1..l1 + l2];
        let cs = charstrings(p);
        assert_eq!(cs.len(), 3 + 12);
        for &(o, n) in &cs {
            // Each starts after "RD " and ends before " NP" or " ND".
            assert_eq!(&p[o - 3..o], b"RD ");
            assert!(p[o + n..].starts_with(b" NP") || p[o + n..].starts_with(b" ND"));
        }
        // A length past the end stops the scan: nothing is decrypted from there on.
        let half = cs[8].0 + cs[8].1 + 4;
        assert_eq!(&p[half - 4..half], b" ND\n");
        let mut cut = p[..half].to_vec();
        cut.extend_from_slice(b"/z 99999 RD ");
        cut.extend_from_slice(&p[half..]);
        assert_eq!(charstrings(&cut), cs[..9]);
    }

    #[test]
    fn opens_pfb_segments() {
        let (f, t, l1, l2) = font(None, "RD", 10);
        let seg = |kind: u8, n: usize| [&[0x80, kind][..], &(n as u32).to_le_bytes()].concat();
        let trailer = f[l1 + l2..].to_vec();
        let pfb =
            [&seg(1, l1)[..], &f[..l1], &seg(2, l2), &f[l1..l1 + l2], &seg(1, trailer.len()), &trailer, &[0x80, 3]]
                .concat();
        let want =
            [&seg(1, l1)[..], &t[..l1], &seg(2, l2), &t[l1..l1 + l2], &seg(1, trailer.len()), &trailer, &[0x80, 3]]
                .concat();
        let l = check_opens(&pfb, &want, l1, l2);
        assert_eq!(l.start, 6 + l1 + 6);
    }

    #[test]
    fn opens_regular_hex_and_leaves_irregular_hex_alone() {
        let (f, t, l1, l2) = font(None, "RD", 30);
        let cipher = &f[l1..l1 + l2];
        let trailer = &f[l1 + l2..];
        for h in [
            Hex { upper: true, line: 64, eol: 0, last_eol: true },
            Hex { upper: false, line: 0, eol: 0, last_eol: true },
            Hex { upper: false, line: 77, eol: 2, last_eol: true },
            Hex { upper: true, line: 32, eol: 1, last_eol: false },
        ] {
            let hex = h.text(cipher);
            assert_eq!(hex.len(), h.text_len(cipher.len()).unwrap());
            let mut hf = f[..l1].to_vec();
            hf.extend_from_slice(&hex);
            if !h.last_eol {
                hf.push(b'\n');
            }
            hf.extend_from_slice(trailer);
            let l = find(&hf, l1, 0).expect("hex layout");
            assert_eq!(l.hex, Some(h));
            // /Length1 pointing at the line end before the digits: the digits still start the layout.
            assert_eq!(find(&hf, l1 - 1, 0), Some(l));
            let opened = open(&hf, &l).unwrap();
            assert_eq!(&opened[l1..l1 + l2], &t[l1..l1 + l2]);
            assert_eq!(close(&opened, &l).unwrap(), hf);
        }
        // Irregular, with the same digits: one line break moved, mixed letter case, spaces between digits. Such
        // a layout doesn't write back the same text, so the font is kept as it is.
        let h = Hex { upper: true, line: 64, eol: 0, last_eol: true };
        let hex = h.text(cipher);
        let mut moved = hex.clone();
        let nl = moved.iter().enumerate().filter(|(_, &c)| c == b'\n').nth(2).unwrap().0;
        moved.remove(nl);
        moved.insert(nl + 10, b'\n');
        let mut mixed = hex.clone();
        let k = mixed.iter().rposition(|c| (b'A'..=b'F').contains(c)).unwrap();
        mixed[k] = mixed[k].to_ascii_lowercase();
        let spaced: Vec<u8> =
            hex.iter().enumerate().flat_map(|(i, &c)| if i % 9 == 8 { vec![c, b' '] } else { vec![c] }).collect();
        for bad in [moved, mixed, spaced] {
            let hf = [&f[..l1], &bad[..], trailer].concat();
            assert_eq!(unhex(&bad, cipher.len()).as_deref(), Some(cipher), "same digits");
            if let Some(l) = find(&hf, l1, 0) {
                let opened = open(&hf, &l).expect("the digits read back");
                assert_ne!(close(&opened, &l).unwrap(), hf);
            }
        }
    }

    #[test]
    fn layouts_that_dont_fit_are_refused() {
        let (f, t, l1, l2) = font(None, "RD", 8);
        let l = Layout { start: l1, n: l2, hex: None };
        for bad in [Layout { start: f.len(), n: 1, ..l }, Layout { n: f.len(), ..l }, Layout { start: usize::MAX, ..l }]
        {
            assert!(open(&f, &bad).is_none());
            assert!(close(&t, &bad).is_none());
        }
        let hex = Layout { hex: Some(Hex { upper: true, line: 64, eol: 2, last_eol: true }), ..l };
        assert!(open(&f, &hex).is_none(), "binary bytes read as hex");
        // Hex text lengths too large for a usize have no length (they used to wrap around to a few bytes:
        // 2 and 0 for the first two on 64 bits).
        let one_digit = |eol, last_eol| Some(Hex { upper: true, line: 1, eol, last_eol });
        for (n, h) in [
            (usize::MAX / 6 + 1, one_digit(2, true)),
            (usize::MAX / 4 + 1, one_digit(0, true)),
            (usize::MAX / 2, hex.hex),
            (usize::MAX, one_digit(0, false)),
        ] {
            let huge = Layout { start: 0, n, hex: h };
            assert_eq!(huge.text_len(), None, "{huge:?}");
            assert!(open(&f, &huge).is_none());
            assert!(close(&t, &huge).is_none());
        }
        // The largest that fits is worked out exactly: 2n digits and 2n - 1 line ends.
        assert_eq!(Layout { start: 0, n: usize::MAX / 4 + 1, hex: one_digit(0, false) }.text_len(), Some(usize::MAX));
        assert_eq!(Layout { start: 0, n: 3, hex: one_digit(2, true) }.text_len(), Some(18));
        assert_eq!(Layout { start: 0, n: 0, hex: hex.hex }.text_len(), Some(2));
    }

    /// Counts the decisions a coder makes.
    struct Counted<C> {
        c: C,
        decisions: u64,
    }

    impl<C: Coder> Coder for Counted<C> {
        fn code(&mut self, bit: Option<u32>, p1: i32) -> u32 {
            self.decisions += 1;
            self.c.code(bit, p1)
        }
        fn code16(&mut self, bit: Option<u32>, p16: u32) -> u32 {
            self.decisions += 1;
            self.c.code16(bit, p16)
        }
        fn exhausted(&self) -> bool {
            self.c.exhausted()
        }
    }

    /// What `code` writes for an opened font (no dictionary, so no hints), with a layout of the test's
    /// choosing: the flag, the layout (written by `code_layout`, as `code` writes it), then `parts`, each with
    /// its model kind. The real encoder only writes layouts it found in the font.
    fn write_opened(l: &Layout, n: usize, parts: &[(Kind, &[u8])]) -> Vec<u8> {
        let mut m = Model::new(14);
        let mut io = Io { m: &mut m, c: Encoder::new(), damaged: false, plain: false, budget: u64::MAX };
        io.m.set_kind(Kind::Meta, Geom::default(), 0);
        io.byte(Some(if l.hex.is_some() { HEX } else { BINARY }));
        code_layout(&mut io, Some(l), l.hex.is_some(), n, 0, 0);
        let mut sink = Vec::new();
        for &(kind, bytes) in parts {
            io.m.set_kind(kind, Geom::default(), 0);
            io.bytes(Some(bytes), bytes.len(), &mut sink);
        }
        let Io { c, .. } = io;
        c.finish()
    }

    /// `code` restoring a font of `n` bytes from `data`: the bytes it gave, whether it found the data damaged,
    /// and how many decisions it decoded.
    fn read_font(data: &[u8], n: usize) -> (Vec<u8>, bool, u64) {
        let mut m = Model::new(14);
        let mut io = Io {
            m: &mut m,
            c: Counted { c: Decoder::new(data), decisions: 0 },
            damaged: false,
            plain: false,
            budget: u64::MAX,
        };
        let (mut out, mut stats) = (Vec::new(), PackStats::default());
        code(&mut io, None, n, None, Kind::Font, &mut out, &mut stats);
        (out, io.damaged, io.c.decisions)
    }

    /// A damaged or crafted file can give a font any layout. One that can't fit the font is refused as soon as
    /// it is read, before anything is decoded for it, so neither time nor memory grows with what the file
    /// claims. (The first two below used to pass the length check, wrapped around to a few bytes, and their
    /// private part was then decoded until the data ran out.)
    #[test]
    fn layouts_in_a_file_that_dont_fit_are_refused_at_once() {
        let (f, t, l1, l2) = font(None, "RD", 8);
        let n = f.len();
        // The writer above writes what `code` writes: the real layout restores the font.
        let real = Layout { start: l1, n: l2, hex: None };
        let data = write_opened(
            &real,
            n,
            &[(Kind::Font, &t[..l1]), (Kind::Charstring, &t[l1..l1 + l2]), (Kind::Font, &t[l1 + l2..])],
        );
        assert_eq!(read_font(&data, n).0, f);
        // Each layout is followed by 256 KiB of zeros where the private part would be: read as one, they take
        // over 250,000 decisions.
        let zeros = vec![0u8; 1 << 18];
        let one_digit = |eol, last_eol| Some(Hex { upper: true, line: 1, eol, last_eol });
        for bad in [
            Layout { start: l1, n: usize::MAX / 6 + 1, hex: one_digit(2, true) },
            Layout { start: l1, n: usize::MAX / 4 + 1, hex: one_digit(0, true) },
            Layout { start: l1, n: n + 1, hex: one_digit(0, false) },
            Layout { start: l1, n: n / 2, hex: one_digit(0, false) },
            Layout { start: l1, n: n - l1 + 1, hex: None },
            Layout { start: l1, n: usize::MAX, hex: None },
            Layout { start: n + 1, n: 0, hex: None },
        ] {
            let data = write_opened(&bad, n, &[(Kind::Font, &t[..l1]), (Kind::Charstring, &zeros)]);
            let (out, damaged, decisions) = read_font(&data, n);
            assert!(damaged && out.is_empty(), "{bad:?}");
            assert!(decisions < 1000, "{bad:?}: {decisions} decisions decoded");
        }
    }

    #[test]
    fn leaves_other_data_alone() {
        let mut x: u32 = 7;
        let random: Vec<u8> = (0..20000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        assert!(find(&random, 100, 5000).is_none());
        let mut fake = b"%!PS-AdobeFont-1.0\ncurrentfile eexec\n".to_vec();
        fake.extend_from_slice(&random);
        assert!(find(&fake, 37, 20000).is_none());
        // A TrueType font announced as Type 1 (as in some test files), and too little to decrypt.
        assert!(find(b"\x00\x01\x00\x00\x00\x0f\x000\x00\x03\x00 OS/2", 16, 0).is_none());
        let (f, _, l1, _) = font(None, "RD", 3);
        assert!(find(&f[..l1 + 20], l1, 0).is_none());
    }
}
