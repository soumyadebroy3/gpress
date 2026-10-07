//! JBIG2 (ITU-T T.88) generic regions, as normal-PDF output uses them: the MQ arithmetic coder (Annex E),
//! generic region coding with template 0, its nominal adaptive pixels and typical prediction (6.2), and the
//! embedded stream form that PDF's JBIG2Decode filter reads (no file header, no global segments: a page
//! information segment and one immediate lossless generic region segment).
//!
//! The bitmap convention is JBIG2's: 1 is a black pixel. (JBIG2Decode gives 1 for white, as PDF images
//! expect, so the filter's output is the inverse of this bitmap.)

use super::ccitt::{expand_row, row_changes, Image};

/// Probability estimation table (T.88 Table E.1): Qe, next index after an MPS, after an LPS, MPS switch.
const QE: [(u32, u8, u8, bool); 47] = [
    (0x5601, 1, 1, true),
    (0x3401, 2, 6, false),
    (0x1801, 3, 9, false),
    (0x0AC1, 4, 12, false),
    (0x0521, 5, 29, false),
    (0x0221, 38, 33, false),
    (0x5601, 7, 6, true),
    (0x5401, 8, 14, false),
    (0x4801, 9, 14, false),
    (0x3801, 10, 14, false),
    (0x3001, 11, 17, false),
    (0x2401, 12, 18, false),
    (0x1C01, 13, 20, false),
    (0x1601, 29, 21, false),
    (0x5601, 15, 14, true),
    (0x5401, 16, 14, false),
    (0x5101, 17, 15, false),
    (0x4801, 18, 16, false),
    (0x3801, 19, 17, false),
    (0x3401, 20, 18, false),
    (0x3001, 21, 19, false),
    (0x2801, 22, 19, false),
    (0x2401, 23, 20, false),
    (0x2201, 24, 21, false),
    (0x1C01, 25, 22, false),
    (0x1801, 26, 23, false),
    (0x1601, 27, 24, false),
    (0x1401, 28, 25, false),
    (0x1201, 29, 26, false),
    (0x1101, 30, 27, false),
    (0x0AC1, 31, 28, false),
    (0x09C1, 32, 29, false),
    (0x08A1, 33, 30, false),
    (0x0521, 34, 31, false),
    (0x0441, 35, 32, false),
    (0x02A1, 36, 33, false),
    (0x0221, 37, 34, false),
    (0x0141, 38, 35, false),
    (0x0111, 39, 36, false),
    (0x0085, 40, 37, false),
    (0x0049, 41, 38, false),
    (0x0025, 42, 39, false),
    (0x0015, 43, 40, false),
    (0x0009, 44, 41, false),
    (0x0005, 45, 42, false),
    (0x0001, 45, 43, false),
    (0x5601, 46, 46, false),
];

/// A context's state: table index (high 7 bits) and the more probable symbol (low bit).
type Cx = u8;

/// The MQ encoder (T.88 E.2), with the pending-byte carry handling of the flowcharts.
pub struct MqEncoder {
    a: u32,
    c: u32,
    ct: u32,
    /// The byte not yet written (a carry may still change it); None before the first.
    b: Option<u32>,
    out: Vec<u8>,
}

impl MqEncoder {
    pub fn new() -> MqEncoder {
        MqEncoder { a: 0x8000, c: 0, ct: 12, b: None, out: Vec::new() }
    }

    #[inline]
    pub fn encode(&mut self, cx: &mut Cx, d: u32) {
        let (qe, nmps, nlps, switch) = QE[(*cx >> 1) as usize];
        let mps = (*cx & 1) as u32;
        self.a -= qe;
        if d == mps {
            if self.a & 0x8000 != 0 {
                self.c += qe;
                return;
            }
            if self.a < qe {
                self.a = qe;
            } else {
                self.c += qe;
            }
            *cx = nmps << 1 | mps as u8;
        } else {
            if self.a < qe {
                self.c += qe;
            } else {
                self.a = qe;
            }
            let m = if switch { 1 - mps } else { mps };
            *cx = nlps << 1 | m as u8;
        }
        loop {
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.ct == 0 {
                self.byte_out();
            }
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }

    fn byte_out(&mut self) {
        let mut b = self.b.unwrap_or(0);
        if b != 0xFF && self.c >= 0x800_0000 {
            // Carry into the byte not yet written.
            b += 1;
            self.b = self.b.map(|_| b);
            self.c &= 0x7FF_FFFF;
        }
        if let Some(x) = self.b {
            self.out.push(x as u8);
        }
        if b == 0xFF {
            // After 0xFF only 7 bits go into the next byte (bit stuffing).
            self.b = Some(self.c >> 20);
            self.c &= 0xF_FFFF;
            self.ct = 7;
        } else {
            self.b = Some(self.c >> 19);
            self.c &= 0x7_FFFF;
            self.ct = 8;
        }
    }

    /// FLUSH (E.2.9), then the 0xFF 0xAC end marker.
    pub fn finish(mut self) -> Vec<u8> {
        let tempc = self.c + self.a;
        self.c |= 0xFFFF;
        if self.c >= tempc {
            self.c -= 0x8000;
        }
        self.c <<= self.ct;
        self.byte_out();
        self.c <<= self.ct;
        self.byte_out();
        let b = self.b.unwrap_or(0);
        self.out.push(b as u8);
        if b != 0xFF {
            self.out.push(0xFF);
        }
        self.out.push(0xAC);
        self.out
    }
}

impl Default for MqEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// The MQ decoder (T.88 E.3, with the code register split in two halves).
pub struct MqDecoder<'a> {
    data: &'a [u8],
    bp: usize,
    chigh: u32,
    clow: u32,
    ct: u32,
    a: u32,
}

impl<'a> MqDecoder<'a> {
    pub fn new(data: &'a [u8]) -> MqDecoder<'a> {
        let mut d = MqDecoder { data, bp: 0, chigh: *data.first().unwrap_or(&0xFF) as u32, clow: 0, ct: 0, a: 0 };
        d.byte_in();
        d.chigh = ((d.chigh << 7) & 0xFFFF) | ((d.clow >> 9) & 0x7F);
        d.clow = (d.clow << 7) & 0xFFFF;
        d.ct -= 7;
        d.a = 0x8000;
        d
    }

    #[inline]
    fn at(&self, i: usize) -> u32 {
        // Past the end the decoder reads 0xFF (the end marker's role).
        *self.data.get(i).unwrap_or(&0xFF) as u32
    }

    fn byte_in(&mut self) {
        if self.at(self.bp) == 0xFF {
            if self.at(self.bp + 1) > 0x8F {
                self.clow += 0xFF00;
                self.ct = 8;
            } else {
                self.bp += 1;
                self.clow += self.at(self.bp) << 9;
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.clow += self.at(self.bp) << 8;
            self.ct = 8;
        }
        if self.clow > 0xFFFF {
            self.chigh += self.clow >> 16;
            self.clow &= 0xFFFF;
        }
    }

    #[inline]
    pub fn decode(&mut self, cx: &mut Cx) -> u32 {
        let (qe, nmps, nlps, switch) = QE[(*cx >> 1) as usize];
        let mut mps = (*cx & 1) as u32;
        let mut a = self.a.wrapping_sub(qe);
        let d;
        let next;
        if self.chigh < qe {
            if a < qe {
                a = qe;
                d = mps;
                next = nmps;
            } else {
                a = qe;
                d = 1 - mps;
                if switch {
                    mps = d;
                }
                next = nlps;
            }
        } else {
            self.chigh -= qe;
            if a & 0x8000 != 0 {
                self.a = a;
                return mps;
            }
            if a < qe {
                d = 1 - mps;
                if switch {
                    mps = d;
                }
                next = nlps;
            } else {
                d = mps;
                next = nmps;
            }
        }
        loop {
            if self.ct == 0 {
                self.byte_in();
            }
            a <<= 1;
            self.chigh = ((self.chigh << 1) & 0xFFFF) | ((self.clow >> 15) & 1);
            self.clow = (self.clow << 1) & 0xFFFF;
            self.ct -= 1;
            if a & 0x8000 != 0 {
                break;
            }
        }
        self.a = a;
        *cx = next << 1 | mps as u8;
        d
    }

}

// ---------------------------------------------------------------- generic region, template 0

/// The context typical prediction uses for template 0 (T.88 6.2.5.7).
const TPGDON_CX: usize = 0x9B25;
/// Pixels of margin around each row buffer (template 0 reaches 4 to the left and 3 to the right).
const M: usize = 4;

/// Template 0 with the nominal adaptive pixels A1 (3,-1), A2 (-3,-1), A3 (2,-2), A4 (-2,-2), bits as in
/// T.88 Figure 3: current row x-1..x-4 in bits 0-3, A1 in bit 4, row above x+2..x-2 in bits 5-9, A2 in bit
/// 10, A3 in bit 11, two rows above x+1..x-1 in bits 12-14, A4 in bit 15. Rows are padded with M white
/// pixels on both sides; `x` is the pixel's index without the padding.
struct Template {
    l0: usize,
    l1: usize,
    l2: usize,
}

impl Template {
    #[inline]
    fn start(r1: &[u8], r2: &[u8]) -> Template {
        // Row above: x-3..x+2 for x = 0 (x+3 comes in at the first pixel); two above: x-2..x+1.
        let mut t = Template { l0: 0, l1: 0, l2: 0 };
        for i in 0..6 {
            t.l1 = t.l1 << 1 | r1[M - 3 + i] as usize;
        }
        for i in 0..4 {
            t.l2 = t.l2 << 1 | r2[M - 2 + i] as usize;
        }
        t
    }
    /// The context of pixel x (call for x = 0, 1, ... in order, then `push` the pixel's value).
    #[inline]
    fn context(&mut self, r1: &[u8], r2: &[u8], x: usize) -> usize {
        self.l1 = (self.l1 << 1 | r1[M + x + 3] as usize) & 0x7F;
        self.l2 = (self.l2 << 1 | r2[M + x + 2] as usize) & 0x1F;
        self.l0 | self.l1 << 4 | self.l2 << 11
    }
    #[inline]
    fn push(&mut self, pixel: u8) {
        self.l0 = (self.l0 << 1 | pixel as usize) & 0xF;
    }
}

/// Generic region coding of `img` (inverted if asked: then black and white swap), template 0, typical
/// prediction on. Returns the arithmetic-coded data, ending with the 0xFF 0xAC marker.
pub fn encode_generic(img: &Image, invert: bool) -> Vec<u8> {
    let w = img.width;
    let mut cx = vec![0u8; 1 << 16];
    let mut e = MqEncoder::new();
    let mut rows = [vec![0u8; w + 2 * M], vec![0u8; w + 2 * M], vec![0u8; w + 2 * M]];
    let mut ltp = 0u32;
    for y in 0..img.rows() {
        // Same as the row above (above the first row: white, which is all 0 after any inversion... the
        // decoder's row "above" the image is 0, so with inversion the first row must be compared to 0 too).
        let same = if y == 0 {
            row_is_zero(img.row(0), w, invert)
        } else {
            img.row(y) == img.row(y - 1)
        };
        let sltp = (same as u32) ^ ltp;
        e.encode(&mut cx[TPGDON_CX], sltp);
        ltp = same as u32;
        rows.rotate_right(1); // rows[0] = current, rows[1] = above, rows[2] = two above
        {
            let cur = &mut rows[0];
            expand_row(img.row(y), &mut cur[M..M + w]);
            if invert {
                cur[M..M + w].iter_mut().for_each(|p| *p ^= 1);
            }
        }
        if same {
            continue;
        }
        let (cur, rest) = rows.split_at_mut(1);
        let (cur, r1, r2) = (&cur[0], &rest[0], &rest[1]);
        let mut t = Template::start(r1, r2);
        for x in 0..w {
            let c = t.context(r1, r2, x);
            let p = cur[M + x];
            e.encode(&mut cx[c], p as u32);
            t.push(p);
        }
    }
    e.finish()
}

fn row_is_zero(changes: &[u32], w: usize, invert: bool) -> bool {
    if invert {
        // Inverted, an all-black row is all 0.
        changes == [0] || (w == 0)
    } else {
        changes.is_empty()
    }
}

/// Decodes generic region data (template 0, nominal adaptive pixels, typical prediction on) into an image
/// (inverted if asked). Any bytes decode to some image (past the end the decoder reads the end marker), so
/// callers compare the result with the image they expect; None once it has more than `max_changes`
/// changing elements (it can't be the expected image, and memory stays bounded).
pub fn decode_generic(data: &[u8], w: usize, h: usize, invert: bool, max_changes: usize) -> Option<Image> {
    let mut cx = vec![0u8; 1 << 16];
    let mut d = MqDecoder::new(data);
    let mut rows = [vec![0u8; w + 2 * M], vec![0u8; w + 2 * M], vec![0u8; w + 2 * M]];
    let mut img = Image::new(w);
    let mut ltp = 0u32;
    let mut ch = Vec::new();
    let mut flipped = vec![0u8; w];
    for _ in 0..h {
        ltp ^= d.decode(&mut cx[TPGDON_CX]);
        rows.rotate_right(1);
        let (cur, rest) = rows.split_at_mut(1);
        let (cur, r1, r2) = (&mut cur[0], &rest[0], &rest[1]);
        if ltp == 1 {
            cur.copy_from_slice(r1);
        } else {
            cur.fill(0);
            let mut t = Template::start(r1, r2);
            for x in 0..w {
                let c = t.context(r1, r2, x);
                let p = d.decode(&mut cx[c]) as u8;
                cur[M + x] = p;
                t.push(p);
            }
        }
        if invert {
            for x in 0..w {
                flipped[x] = cur[M + x] ^ 1;
            }
            row_changes(&flipped, &mut ch);
        } else {
            row_changes(&cur[M..M + w], &mut ch);
        }
        if img.changes.len() + ch.len() > max_changes {
            return None;
        }
        img.push_row(&ch);
    }
    Some(img)
}

// ---------------------------------------------------------------- the embedded stream PDF reads

const PAGE_INFO: u8 = 48;
const IMMEDIATE_LOSSLESS_GENERIC: u8 = 39;
/// Generic region flags: arithmetic coding (MMR 0), template 0, typical prediction on.
const GENERIC_FLAGS: u8 = 0x08;
/// The nominal adaptive pixels of template 0: (3,-1), (-3,-1), (2,-2), (-2,-2).
const NOMINAL_AT: [u8; 8] = [3, 0xFF, 0xFD, 0xFF, 2, 0xFE, 0xFE, 0xFE];

fn segment_header(out: &mut Vec<u8>, number: u32, kind: u8, len: usize) {
    out.extend_from_slice(&number.to_be_bytes());
    out.push(kind); // not deferred, page association in one byte
    out.push(0); // refers to no other segment
    out.push(1); // page 1
    out.extend_from_slice(&(len as u32).to_be_bytes());
}

/// A JBIG2Decode stream for `img` (inverted if asked).
pub fn pdf_stream(img: &Image, invert: bool) -> Vec<u8> {
    let (w, h) = (img.width as u32, img.rows() as u32);
    let data = encode_generic(img, invert);
    let mut out = Vec::with_capacity(data.len() + 64);
    segment_header(&mut out, 0, PAGE_INFO, 19);
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&[0; 8]); // resolution unknown
    out.push(0x01); // eventually lossless; default pixel 0; combination operator OR
    out.extend_from_slice(&[0, 0]); // not striped
    segment_header(&mut out, 1, IMMEDIATE_LOSSLESS_GENERIC, 17 + 1 + 8 + data.len());
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&[0; 8]); // at 0, 0
    out.push(0); // combination operator OR
    out.push(GENERIC_FLAGS);
    out.extend_from_slice(&NOMINAL_AT);
    out.extend_from_slice(&data);
    out
}

/// Reads back a stream exactly as `pdf_stream` writes it (anything else: None) and decodes the image, if it
/// has at most `max_changes` changing elements.
pub fn read_pdf_stream(s: &[u8], invert: bool, max_changes: usize) -> Option<Image> {
    let u32_at = |i: usize| -> Option<u32> { Some(u32::from_be_bytes(s.get(i..i + 4)?.try_into().ok()?)) };
    // Page information segment.
    if u32_at(0)? != 0 || s.get(4..7)? != [PAGE_INFO, 0, 1] || u32_at(7)? != 19 {
        return None;
    }
    let (w, h) = (u32_at(11)? as usize, u32_at(15)? as usize);
    if s.get(19..30)? != [0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0] {
        return None;
    }
    // Generic region segment.
    if u32_at(30)? != 1 || s.get(34..37)? != [IMMEDIATE_LOSSLESS_GENERIC, 0, 1] {
        return None;
    }
    let len = u32_at(37)? as usize;
    if 41 + len != s.len() || len < 26 {
        return None;
    }
    if u32_at(41)? as usize != w || u32_at(45)? as usize != h || s.get(49..58)? != [0; 9] || s[58] != GENERIC_FLAGS || s.get(59..67)? != NOMINAL_AT {
        return None;
    }
    if w == 0 || w > super::ccitt::MAX_COLUMNS || h > 1 << 20 {
        return None;
    }
    decode_generic(&s[67..], w, h, invert, max_changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    #[test]
    fn mq_coder_round_trips() {
        for skew in [1u64, 3, 20, 400] {
            let mut s = 99 + skew;
            let mut cx_e = vec![0u8; 64];
            let bits: Vec<(usize, u32)> = (0..100_000).map(|_| ((rng(&mut s) % 64) as usize, (rng(&mut s) % skew == 0) as u32)).collect();
            let mut e = MqEncoder::new();
            for &(c, b) in &bits {
                e.encode(&mut cx_e[c], b);
            }
            let data = e.finish();
            assert_eq!(&data[data.len() - 2..], &[0xFF, 0xAC]);
            let mut cx_d = vec![0u8; 64];
            let mut d = MqDecoder::new(&data);
            for &(c, b) in &bits {
                assert_eq!(d.decode(&mut cx_d[c]), b);
            }
        }
    }

    fn sample(w: usize, h: usize, seed: u64) -> Image {
        let mut s = seed;
        let mut img = Image::new(w);
        let mut px = vec![0u8; w];
        let mut ch = Vec::new();
        for y in 0..h {
            if y % 7 == 3 {
                px.fill(0);
            } else if y % 13 == 5 {
                px.fill(1);
            } else if y % 5 != 0 {
                for x in 0..w {
                    if rng(&mut s) % 9 == 0 {
                        px[x] ^= 1;
                    }
                }
            }
            row_changes(&px, &mut ch);
            img.push_row(&ch);
        }
        img
    }

    #[test]
    fn generic_regions_round_trip() {
        for &(w, h) in &[(1usize, 1usize), (1, 9), (3, 4), (8, 8), (9, 30), (64, 3), (65, 40), (300, 77)] {
            for invert in [false, true] {
                let img = sample(w, h, (w * 7 + h) as u64);
                let s = pdf_stream(&img, invert);
                let back = read_pdf_stream(&s, invert, usize::MAX).unwrap();
                assert_eq!(back, img, "{w}x{h} invert={invert}");
            }
        }
        // All white, all black (typical prediction covers every row of these).
        for invert in [false, true] {
            let mut img = Image::new(50);
            for _ in 0..20 {
                img.push_row(&[]);
            }
            assert_eq!(read_pdf_stream(&pdf_stream(&img, invert), invert, usize::MAX).unwrap(), img);
            let mut img = Image::new(50);
            for _ in 0..20 {
                img.push_row(&[0]);
            }
            assert_eq!(read_pdf_stream(&pdf_stream(&img, invert), invert, usize::MAX).unwrap(), img);
        }
    }

    #[test]
    fn damaged_streams_are_refused_or_decode_to_something_checked() {
        let img = sample(120, 60, 4);
        let s = pdf_stream(&img, false);
        // A changed header is refused; the caller compares whatever decodes with the image it expects.
        for i in 0..67 {
            let mut b = s.clone();
            b[i] ^= 0x10;
            if let Some(x) = read_pdf_stream(&b, false, usize::MAX) {
                assert_eq!(x.width, 120);
            }
        }
        assert!(read_pdf_stream(&s[..s.len() - 1], false, usize::MAX).is_none());
        assert!(read_pdf_stream(&[], false, usize::MAX).is_none());
        // A decode that runs past the expected number of changes stops.
        assert!(read_pdf_stream(&s, false, img.changes.len() - 1).is_none());
    }
}
