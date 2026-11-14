//! The lossy image step of a normal PDF: images resampled to the resolution they are shown at, and photos
//! written as JPEG, within floors that hold at every level (see `Lossy`). Off unless asked for.

/// What the lossy image step may do to a normal PDF's images. `Lossy::OFF` changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lossy {
    /// A photo shown at more than this many pixels per inch is resampled down to it (never below 150, whatever
    /// is asked); 0: photos are never resampled.
    pub photo_ppi: u16,
    /// Line art (screenshots, charts, diagrams) and scans shown at more than this are resampled down to it
    /// (never below 300, whatever is asked); 0: never.
    pub art_ppi: u16,
    /// JPEG quality on libjpeg's scale (kept within 50 to 95) for a photo that is resampled or stored without
    /// JPEG; 0: no image becomes a JPEG.
    pub jpeg_quality: u8,
    /// Also write an existing JPEG photo again at `jpeg_quality` when that is at least a tenth smaller.
    pub rejpeg: bool,
}

impl Lossy {
    pub const OFF: Lossy = Lossy { photo_ppi: 0, art_ppi: 0, jpeg_quality: 0, rejpeg: false };

    /// The settings of a level: 0 is off; 1, 2 and 3 are what efforts 60, 80 and 100 mean.
    pub fn level(n: u8) -> Lossy {
        match n {
            0 => Lossy::OFF,
            1 => Lossy { photo_ppi: 300, art_ppi: 300, jpeg_quality: 85, rejpeg: false },
            2 => Lossy { photo_ppi: 150, art_ppi: 300, jpeg_quality: 85, rejpeg: false },
            _ => Lossy { photo_ppi: 150, art_ppi: 300, jpeg_quality: 75, rejpeg: true },
        }
    }

    pub fn is_off(&self) -> bool {
        *self == Lossy::OFF
    }
}

// ---------------------------------------------------------------- the step
//
// It runs on the loaded objects before the lossless pipeline plans its work (`rewrite::rewrite`): an image it
// reduces gets a new dictionary and new data there, and everything after it (the stream jobs, the writer, the
// self-check) treats the new image like any other stream.
//
// Which images: 8-bit grey or RGB image XObjects of more than 65,536 pixels, stored with general filters or as
// one JPEG (see `eligible`), whose every use in the file the page walk measured (`place::shown`). Anything
// else is left to the lossless pipeline exactly as it is.
//
// What is done: an image shown at 5/4 of its floor or more is resampled down to the floor (an area average).
// The floor is the photo setting for a photo and the line-art setting for line art and for a scan (an image
// that covers most of a page). A photo that was resampled, or is not a JPEG, is written as a baseline JPEG;
// line art stays samples behind Flate and a JPEG that looks like line art is left alone.
//
// What is kept: a candidate of at most nine tenths of the image's lossless size (the smaller of its stored
// bytes and a quick lossless encode of it), whose JPEG, decoded again, stays close to the samples it was made
// from (`WHOLE_MSE`, `BLOCK_MSE`). Any other candidate is dropped.

use super::filters::{self, chain, decode_stage, is_general, Stage};
use super::images;
use super::object::{Dict, Obj, Stream};
use super::place::{self, Shown};
use super::rewrite::{image_level, png_parms, predictor_sample, Options};
use crate::clock::Deadline;
use crate::flate;
use crate::par;
use std::borrow::Cow;
use std::collections::BTreeMap;

/// An image of at most this many pixels is never touched (logos, icons, small figures).
const SMALL: u64 = 65_536;

/// The floors no setting goes below, in pixels per inch as shown: photos, and line art or scans.
const PHOTO_FLOOR: u16 = 150;
const ART_FLOOR: u16 = 300;

/// What the step did to the images of one file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// Images resampled to a lower resolution.
    pub resampled: usize,
    /// Photos stored without JPEG that became a JPEG (resampled or not).
    pub to_jpeg: usize,
    /// JPEG photos written again at a lower quality (not resampled).
    pub rejpeg: usize,
}

/// The lossy step over the objects of one file. Every image it may work on (`eligible`) whose every use the
/// page walk measured gets a candidate made on the pool (`candidate`); the candidates that pass replace their
/// image's dictionary and data, in object order, so the result is the same for any number of threads.
pub fn reduce(objs: &mut BTreeMap<u32, (u16, Obj)>, trailer: &Dict, opt: &Options, pool: Option<&par::Pool>) -> Done {
    let dbg = std::env::var_os("PDFE_DEBUG").is_some();
    let shown = place::shown(objs, trailer, opt.max_stream, opt.deadline);
    let images: Vec<(u32, Image)> = shown
        .iter()
        .filter_map(|(&n, &shown)| match objs.get(&n) {
            Some((_, Obj::Stream(s))) => Some((n, eligible(s, shown, objs, opt)?)),
            _ => None,
        })
        .collect();
    // The biggest first, as the stream jobs are started.
    let mut order: Vec<usize> = (0..images.len()).collect();
    order.sort_by_key(|&k| (std::cmp::Reverse(images[k].1.s.data.len()), k));
    let made = par::map(pool, images.len(), Some(&order), |k| {
        let (n, img) = &images[k];
        // Out of time: nothing more is made (the whole rewrite is given up after the step).
        let made = if opt.deadline.passed() { Err("out of time") } else { candidate(img, opt) };
        if dbg {
            let (w, h, stored) = (img.width, img.height, img.s.data.len());
            match &made {
                Ok(c) => eprintln!("lossy: image {n}, {w}x{h}, {stored} bytes -> {} bytes ({})", c.data.len(), c.what),
                Err(why) => eprintln!("lossy: image {n}, {w}x{h}, {stored} bytes, left alone: {why}"),
            }
        }
        made.ok()
    });
    let numbers: Vec<u32> = images.iter().map(|i| i.0).collect();
    drop(images);
    let mut done = Done::default();
    for (n, c) in numbers.into_iter().zip(made) {
        let (Some(c), Some((_, Obj::Stream(s)))) = (c, objs.get_mut(&n)) else { continue };
        done.resampled += c.resampled as usize;
        done.to_jpeg += c.to_jpeg as usize;
        done.rejpeg += (c.what == REJPEG) as usize;
        s.dict = c.dict;
        s.data = c.data;
    }
    done
}

/// What an object refers to: a reference followed once, anything else as it is.
fn resolve<'o>(o: &'o Obj, objs: &'o BTreeMap<u32, (u16, Obj)>) -> Option<&'o Obj> {
    match o {
        Obj::Ref(n, _) => objs.get(n).map(|x| &x.1),
        x => Some(x),
    }
}

/// An image the step may work on, as its dictionary describes it.
struct Image<'a> {
    s: &'a Stream,
    width: usize,
    height: usize,
    /// Colour components: 1 or 3.
    channels: usize,
    /// Its general filters, in order.
    general: Vec<Stage>,
    /// A JPEG is what they decode to (else the samples).
    jpeg: bool,
    shown: Shown,
}

/// The colour components of a colour space the step works in: 1 for DeviceGray, CalGray and ICCBased with one
/// component, 3 for DeviceRGB, CalRGB and ICCBased with three. None for every other space (indexed, CMYK, Lab,
/// separations and so on, where averaging samples or coding them as a JPEG would not mean the same colours).
fn channels(cs: &Obj, objs: &BTreeMap<u32, (u16, Obj)>) -> Option<usize> {
    match resolve(cs, objs)? {
        Obj::Name(n) => match n.as_slice() {
            b"DeviceGray" => Some(1),
            b"DeviceRGB" => Some(3),
            _ => None,
        },
        Obj::Array(a) => match a.first()?.as_name()? {
            b"CalGray" => Some(1),
            b"CalRGB" => Some(3),
            b"ICCBased" => match resolve(a.get(1)?, objs)? {
                Obj::Stream(profile) => match profile.dict.get(b"N")?.as_int()? {
                    1 => Some(1),
                    3 => Some(3),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// Whether the step may work on image `s`, from its dictionary: an image XObject with 8 bits per component in
/// a grey or RGB colour space (`channels`), more than `SMALL` pixels, at most 65,535 each way, decoding within
/// `max_stream`; without /ImageMask, /Decode, /Alternates, /OPI or an external file; with no colour-key /Mask
/// (those name sample values) and no soft mask that carries /Matte (its colours are blended with this image's);
/// stored with general filters only, or with one JPEG without parameters after them. A mask or soft mask image
/// is its own object and keeps its own size.
fn eligible<'a>(s: &'a Stream, shown: Shown, objs: &BTreeMap<u32, (u16, Obj)>, opt: &Options) -> Option<Image<'a>> {
    let d = &s.dict;
    if d.name(b"Subtype") != Some(b"Image") || d.get(b"BitsPerComponent") != Some(&Obj::Int(8)) {
        return None;
    }
    if [&b"ImageMask"[..], b"Decode", b"Alternates", b"OPI", b"F"].iter().any(|k| d.has(k)) {
        return None;
    }
    if !matches!(d.get(b"Mask").map(|m| resolve(m, objs)), None | Some(Some(Obj::Stream(_)))) {
        return None;
    }
    if let Some(m) = d.get(b"SMask") {
        match resolve(m, objs) {
            Some(Obj::Stream(m)) if !m.dict.has(b"Matte") => {}
            _ => return None,
        }
    }
    let side = |k: &[u8]| d.get(k)?.as_int().filter(|v| (1..=65_535).contains(v)).map(|v| v as usize);
    let (width, height) = (side(b"Width")?, side(b"Height")?);
    let channels = channels(d.get(b"ColorSpace")?, objs)?;
    let pixels = width as u64 * height as u64;
    if pixels <= SMALL || pixels * channels as u64 > opt.max_stream as u64 {
        return None;
    }
    let mut general = chain(d)?;
    let k = general.iter().take_while(|st| is_general(&st.name)).count();
    let jpeg = match &general[k..] {
        [] => false,
        [st] if st.name == b"DCTDecode" && st.parms.is_none() => true,
        _ => return None,
    };
    general.truncate(k);
    Some(Image { s, width, height, channels, general, jpeg, shown })
}

/// A candidate that passed: the image's new dictionary and data, and what was done to it.
struct Candidate {
    dict: Dict,
    data: Vec<u8>,
    resampled: bool,
    to_jpeg: bool,
    /// For the debug line; `REJPEG` for a JPEG written again without resampling.
    what: &'static str,
}

const REJPEG: &str = "JPEG again";

/// The pixels an image of `w` by `h` shown at `ppi` is resampled to for `floor`, both in pixels per inch: None
/// below 5/4 of the floor (resampling by a few percent costs sharpness and saves little); else the size at
/// which its lower resolution is the floor, each side rounded up.
fn target(w: usize, h: usize, ppi: f64, floor: f64) -> Option<(usize, usize)> {
    if ppi * 4.0 < floor * 5.0 {
        return None;
    }
    // The next whole number at or above `x` (which is positive and below the side).
    let up = |x: f64| {
        let t = x as usize;
        if (t as f64) < x {
            t + 1
        } else {
            t
        }
    };
    let scale = floor / ppi;
    Some((up(w as f64 * scale).clamp(1, w), up(h as f64 * scale).clamp(1, h)))
}

/// The candidate for one image, or why it is left alone.
fn candidate(img: &Image, opt: &Options) -> Result<Candidate, &'static str> {
    let lossy = opt.lossy;
    let (w, h, stored) = (img.width, img.height, img.s.data.len());
    // The image's resolution as shown: the lower of its two directions at its largest placement.
    if !(img.shown.width > 0.0 && img.shown.height > 0.0) {
        return Err("shown at no size");
    }
    let ppi = (w as f64 / img.shown.width).min(h as f64 / img.shown.height);
    let floor = |asked: u16, least: u16| (asked > 0).then(|| asked.max(least) as f64);
    let art_floor = floor(lossy.art_ppi, ART_FLOOR);
    // A scan gets the line-art floor whatever its samples look like.
    let photo_floor = if img.shown.scan { art_floor } else { floor(lossy.photo_ppi, PHOTO_FLOOR) };
    let quality = (cfg!(feature = "jpeg") && lossy.jpeg_quality > 0).then(|| lossy.jpeg_quality.clamp(50, 95));
    let size = |f: Option<f64>| f.and_then(|f| target(w, h, ppi, f));
    // Nothing these settings could do to it whatever its samples are: it is not decoded.
    let any = match (img.jpeg, quality) {
        (true, None) => false,
        (true, Some(_)) => lossy.rejpeg || size(photo_floor).is_some(),
        (false, None) => size(art_floor).is_some(),
        (false, Some(_)) => true,
    };
    if !any {
        return Err("nothing to do at its resolution");
    }
    let mut data = Cow::Borrowed(&img.s.data[..]);
    for st in &img.general {
        data = Cow::Owned(decode_stage(st, &data, opt.max_stream).map_err(|_| "its filters do not decode")?);
        let predicted = matches!(st.name.as_slice(), b"FlateDecode" | b"LZWDecode")
            && filters::predictor(st).ok_or("odd predictor parameters")?.0 > 1;
        if predicted {
            data = Cow::Owned(filters::unpredict(st, &data).map_err(|_| "its predictor does not decode")?);
        }
    }
    if img.jpeg {
        let quality = quality.ok_or("no JPEG in these settings")?;
        return from_jpeg(img, &data, ppi, photo_floor, quality, opt);
    }
    // Fewer samples than the size needs: readers differ on such an image.
    let px = data.get(..w * h * img.channels).ok_or("fewer samples than its size")?;
    if is_photo(&luma(px, img.channels), w, h) {
        let quality = quality.ok_or("a photo, and no JPEG in these settings")?;
        return from_photo(img, px, size(photo_floor), quality, opt);
    }
    let (tw, th) = size(art_floor).ok_or("line art at or near its floor")?;
    let small = resample(px, w, h, img.channels, tw, th, opt.deadline).ok_or("out of time")?;
    let (parms, z) = quick_flate(&small, tw, img.channels, opt);
    if !worth(z.len(), stored) || !worth(z.len(), quick_flate(px, w, img.channels, opt).1.len()) {
        return Err("line art: not a tenth smaller");
    }
    let dict = new_dict(&img.s.dict, tw, th, b"FlateDecode", parms, z.len());
    Ok(Candidate { dict, data: z, resampled: true, to_jpeg: false, what: "line art resampled" })
}

/// Whether a candidate of `new` bytes is worth it against `old`: at most nine tenths of it.
fn worth(new: usize, old: usize) -> bool {
    new as u64 * 10 <= old as u64 * 9
}

/// The image's dictionary for new data: every entry kept but the size, the filter entries and the length.
fn new_dict(d: &Dict, w: usize, h: usize, filter: &[u8], parms: Option<Dict>, len: usize) -> Dict {
    let mut d = d.clone();
    d.set(b"Width", Obj::Int(w as i64));
    d.set(b"Height", Obj::Int(h as i64));
    d.set(b"Filter", Obj::Name(filter.to_vec()));
    d.remove(b"DecodeParms");
    if let Some(p) = parms {
        d.set(b"DecodeParms", Obj::Dict(p));
    }
    d.set(b"Length", Obj::Int(len as i64));
    d
}

/// Samples (rows of `w` pixels of `ch` bytes) as the fast path stores an image: Flate at effort 0's level for
/// images, with the PNG predictor an estimate prefers (as `rewrite::repredict_fast` chooses it). Gives the
/// /DecodeParms, when there is a predictor, and the data.
fn quick_flate(px: &[u8], w: usize, ch: usize, opt: &Options) -> (Option<Dict>, Vec<u8>) {
    let g = images::Geometry { colors: ch, bpc: 8, columns: w };
    let (row, bpp) = (g.row(), g.bpp());
    let rows = px.len() / row;
    let bands = predictor_sample(rows, row, 0).unwrap_or_else(|| vec![(0, rows)]);
    let settings = flate::Options {
        fast: image_level(0),
        shared: opt.flate.shared.clone(),
        deadline: opt.deadline,
        ..flate::Options::effort(0)
    };
    match images::estimate_choice(px, row, bpp, 8, &bands) {
        Some(how) => (Some(png_parms(&g)), flate::deflate_zlib(&images::png_filter(px, row, bpp, how), &settings)),
        None => (None, flate::deflate_zlib(px, &settings)),
    }
}

/// The luma of 8-bit samples, one byte a pixel: the samples themselves for grey, JFIF's weights for RGB.
fn luma(px: &[u8], channels: usize) -> Cow<'_, [u8]> {
    if channels != 3 {
        return Cow::Borrowed(px);
    }
    let y = |p: &[u8; 3]| ((19595 * p[0] as u32 + 38470 * p[1] as u32 + 7471 * p[2] as u32 + 32768) >> 16) as u8;
    Cow::Owned(px.as_chunks::<3>().0.iter().map(y).collect())
}

/// A run of equal samples this long, along a row or down a column, is flat (see `is_photo`).
const RUN: u16 = 32;
/// A step of this many levels between neighbouring samples is a hard edge.
const EDGE: u8 = 64;

/// Whether `luma` (`w` by `h`) is a clear continuous-tone picture: a photo. Everything else, and everything in
/// doubt, is line art, which has the safer floor and never becomes a JPEG. Decided from three counts, in whole
/// numbers, so that every machine decides the same. A photo has all of:
/// - flat samples, the ones that go on a run of at least `RUN` equal samples along their row or down their
///   column, are at most 1/16 of all: screenshots, charts, diagrams and renders on a plain ground have large
///   areas of one value (a run that long is more than the 8 by 8 blocks a JPEG photo decodes to in smooth
///   places);
/// - hard edges, samples at least `EDGE` levels from their left or upper neighbour, are at most 1/128 of all:
///   text and line work are hard edges, while a camera's picture is soft from one sample to the next;
/// - its 8 most frequent levels hold at most half of the samples: a few levels that hold most of a picture
///   are a ground and its inks.
fn is_photo(luma: &[u8], w: usize, h: usize) -> bool {
    if w < 2 || h < 2 || luma.len() < w * h {
        return false;
    }
    let (mut flat, mut hard) = (0u64, 0u64);
    let mut levels = [0u64; 256];
    // The length of the run of equal samples that ends at each column of the row before, capped at `RUN`.
    let mut down = vec![0u16; w];
    for y in 0..h {
        let row = &luma[y * w..(y + 1) * w];
        let above = if y > 0 { &luma[(y - 1) * w..y * w] } else { row };
        let mut along = 0u16;
        for x in 0..w {
            let v = row[x];
            levels[v as usize] += 1;
            let left = if x > 0 { row[x - 1] } else { v };
            along = if x > 0 && v == left { (along + 1).min(RUN) } else { 0 };
            down[x] = if y > 0 && v == above[x] { (down[x] + 1).min(RUN) } else { 0 };
            flat += (along + 1 >= RUN || down[x] + 1 >= RUN) as u64;
            hard += (v.abs_diff(left) >= EDGE || v.abs_diff(above[x]) >= EDGE) as u64;
        }
    }
    let all = (w * h) as u64;
    levels.sort_unstable_by(|a, b| b.cmp(a));
    let top: u64 = levels[..8].iter().sum();
    flat * 16 <= all && hard * 128 <= all && top * 2 <= all
}

/// Each output position's share of the input along one axis, for an area average of `s` input samples down to
/// `d`: the first input sample it covers and the weight of each one it covers, in units where an input sample
/// weighs `d` and the weights of one output add up to `s`.
fn spans(s: usize, d: usize) -> Vec<(usize, Vec<u32>)> {
    (0..d)
        .map(|o| {
            let (from, to) = (o * s, (o + 1) * s);
            let first = from / d;
            let weights = (first..=(to - 1) / d).map(|i| (((i + 1) * d).min(to) - (i * d).max(from)) as u32).collect();
            (first, weights)
        })
        .collect()
}

/// `src` (`sw` by `sh` pixels of `ch` bytes, each side at most 65,535) brought down to `dw` by `dh` by an area
/// average: every output sample is the mean of the input samples its place covers, each weighted by the share
/// covered, rounded to the nearest. Whole numbers only. None when the deadline passes.
fn resample(src: &[u8], sw: usize, sh: usize, ch: usize, dw: usize, dh: usize, deadline: Deadline) -> Option<Vec<u8>> {
    let (across, down) = (spans(sw, dw), spans(sh, dh));
    let total = sw as u64 * sh as u64;
    let mut out = Vec::with_capacity(dw * dh * ch);
    // One input row averaged across (sums, at most 65,535 x 255), and which row it is.
    let mut flat = vec![0u32; dw * ch];
    let mut flat_of = usize::MAX;
    let mut sums = vec![0u64; dw * ch];
    for (y, (first, weights)) in down.iter().enumerate() {
        if y % 64 == 0 && deadline.passed() {
            return None;
        }
        sums.fill(0);
        for (j, &wy) in weights.iter().enumerate() {
            let r = first + j;
            if flat_of != r {
                let row = &src[r * sw * ch..(r + 1) * sw * ch];
                for (x, (first, weights)) in across.iter().enumerate() {
                    for c in 0..ch {
                        let at = |i: usize| row[(first + i) * ch + c] as u32;
                        flat[x * ch + c] = weights.iter().enumerate().map(|(i, &wx)| wx * at(i)).sum();
                    }
                }
                flat_of = r;
            }
            for (s, &f) in sums.iter_mut().zip(&flat) {
                *s += wy as u64 * f as u64;
            }
        }
        out.extend(sums.iter().map(|&s| ((s + total / 2) / total) as u8));
    }
    Some(out)
}

#[cfg(feature = "jpeg")]
use crate::jpeg::{self, Plane};

/// The most luma error a JPEG candidate may have against the samples it was made from, as mean squared error
/// in hundredths: 20.56 over the whole image (a PSNR of 35 dB) and 163.34 in its worst block of 16 by 16
/// (26 dB). The brief's starting values: at quality 75 and up a photo stays well inside both, while a
/// candidate with ringing around hard edges (line art taken for a photo) falls out on the block floor.
#[cfg(feature = "jpeg")]
const WHOLE_MSE: u64 = 2056;
#[cfg(feature = "jpeg")]
const BLOCK_MSE: u64 = 16334;

/// The luma error of `b` against `a` (both `w` by `h`), as mean squared error in hundredths: over the whole
/// image, and in the worst of its blocks of 16 by 16 (the last block of a row or column takes the samples left
/// over, so every block has at least 16 each way where the image does).
#[cfg(feature = "jpeg")]
fn errors(a: &[u8], b: &[u8], w: usize, h: usize) -> (u64, u64) {
    let (across, down) = ((w / 16).max(1), (h / 16).max(1));
    let (mut whole, mut worst) = (0u64, 0u64);
    for by in 0..down {
        let (y0, y1) = (by * 16, if by + 1 == down { h } else { (by + 1) * 16 });
        let mut blocks = vec![0u64; across];
        for y in y0..y1 {
            let (ra, rb) = (&a[y * w..(y + 1) * w], &b[y * w..(y + 1) * w]);
            for x in 0..w {
                let d = ra[x].abs_diff(rb[x]) as u64;
                blocks[(x / 16).min(across - 1)] += d * d;
            }
        }
        for (bx, &sum) in blocks.iter().enumerate() {
            let width = if bx + 1 == across { w - bx * 16 } else { 16 };
            worst = worst.max(sum * 100 / (width * (y1 - y0)) as u64);
            whole += sum;
        }
    }
    (whole * 100 / (w as u64 * h as u64).max(1), worst)
}

/// A photo's planes (grey, or Y with Cb and Cr at any size) as a JPEG at `quality`: brought to `size` when
/// there is one (the chroma planes to half of it each way, which the encoder asks for; a chroma plane that is
/// smaller than that is not enlarged: None), coded, and given optimal Huffman tables. With the luma plane the
/// JPEG was made from, for the check.
#[cfg(feature = "jpeg")]
fn photo_jpeg(
    mut planes: Vec<Plane>,
    size: Option<(usize, usize)>,
    quality: u8,
    deadline: Deadline,
) -> Result<(Vec<u8>, Plane), &'static str> {
    let (w, h) = size.unwrap_or((planes[0].w, planes[0].h));
    for (i, p) in planes.iter_mut().enumerate() {
        let (pw, ph) = if i == 0 { (w, h) } else { (w.div_ceil(2), h.div_ceil(2)) };
        if p.w < pw || p.h < ph {
            return Err("chroma too coarse to code again");
        }
        if (p.w, p.h) != (pw, ph) {
            let px = resample(&p.px, p.w, p.h, 1, pw, ph, deadline).ok_or("out of time")?;
            *p = Plane { w: pw, h: ph, px };
        }
    }
    let coded = jpeg::encode(&planes, quality, deadline).ok_or("the JPEG encoder refused it")?;
    let coded = jpeg::optimize(&coded).unwrap_or(coded);
    Ok((coded, planes.swap_remove(0)))
}

/// Whether the JPEG `coded` decodes to luma close enough to `y`, the plane it was made from.
#[cfg(feature = "jpeg")]
fn close(coded: &[u8], y: &Plane, opt: &Options) -> Result<(), &'static str> {
    let back = jpeg::decode(coded, opt.max_stream, opt.deadline).map_err(|_| "the new JPEG does not decode")?;
    let luma = back.planes.first().filter(|p| (p.w, p.h) == (y.w, y.h)).ok_or("the new JPEG has another size")?;
    let (whole, worst) = errors(&y.px, &luma.px, y.w, y.h);
    if whole > WHOLE_MSE {
        return Err("quality: the whole image");
    }
    if worst > BLOCK_MSE {
        return Err("quality: its worst block");
    }
    Ok(())
}

/// A JPEG image (`data`: the JPEG file): decoded to its own planes, and when their luma is a photo's, resampled
/// to its floor or, with `rejpeg`, only coded again. A JPEG that looks like line art is left alone: line art is
/// never made a JPEG, and its samples behind Flate would need a colour conversion.
#[cfg(feature = "jpeg")]
fn from_jpeg(
    img: &Image,
    data: &[u8],
    ppi: f64,
    floor: Option<f64>,
    quality: u8,
    opt: &Options,
) -> Result<Candidate, &'static str> {
    let (w, h) = (img.width, img.height);
    let pic = jpeg::decode(data, opt.max_stream, opt.deadline).map_err(|r| r.0)?;
    if (pic.width, pic.height, pic.planes.len()) != (w, h, img.channels) {
        return Err("the JPEG and the dictionary disagree");
    }
    if !is_photo(&pic.planes[0].px, w, h) {
        return Err("a JPEG that looks like line art");
    }
    let size = floor.and_then(|f| target(w, h, ppi, f));
    if size.is_none() && !opt.lossy.rejpeg {
        return Err("a JPEG photo at or near its floor");
    }
    let (coded, y) = photo_jpeg(pic.planes, size, quality, opt.deadline)?;
    let lossless = jpeg::optimize(data).map_or(data.len(), |j| j.len());
    if !worth(coded.len(), img.s.data.len()) || !worth(coded.len(), lossless) {
        return Err("JPEG: not a tenth smaller");
    }
    close(&coded, &y, opt)?;
    let dict = new_dict(&img.s.dict, y.w, y.h, b"DCTDecode", None, coded.len());
    let what = if size.is_some() { "JPEG resampled" } else { REJPEG };
    Ok(Candidate { dict, data: coded, resampled: size.is_some(), to_jpeg: false, what })
}

/// A photo stored as samples (`px`): written as a JPEG, resampled to `size` first when there is one.
#[cfg(feature = "jpeg")]
fn from_photo(
    img: &Image,
    px: &[u8],
    size: Option<(usize, usize)>,
    quality: u8,
    opt: &Options,
) -> Result<Candidate, &'static str> {
    let (w, h) = (img.width, img.height);
    let planes = if img.channels == 3 {
        jpeg::ycc_planes(px, w, h).ok_or("the JPEG encoder refused it")?
    } else {
        vec![Plane { w, h, px: px.to_vec() }]
    };
    let (coded, y) = photo_jpeg(planes, size, quality, opt.deadline)?;
    if !worth(coded.len(), img.s.data.len()) || !worth(coded.len(), quick_flate(px, w, img.channels, opt).1.len()) {
        return Err("photo: not a tenth smaller as a JPEG");
    }
    close(&coded, &y, opt)?;
    let dict = new_dict(&img.s.dict, y.w, y.h, b"DCTDecode", None, coded.len());
    let what = if size.is_some() { "photo resampled, to JPEG" } else { "photo to JPEG" };
    Ok(Candidate { dict, data: coded, resampled: size.is_some(), to_jpeg: true, what })
}

// Without the `jpeg` feature an image that would need a JPEG is left alone (`candidate` never gets this far:
// it has no JPEG quality then).
#[cfg(not(feature = "jpeg"))]
fn from_jpeg(_: &Image, _: &[u8], _: f64, _: Option<f64>, _: u8, _: &Options) -> Result<Candidate, &'static str> {
    Err("needs the jpeg feature")
}

#[cfg(not(feature = "jpeg"))]
fn from_photo(_: &Image, _: &[u8], _: Option<(usize, usize)>, _: u8, _: &Options) -> Result<Candidate, &'static str> {
    Err("needs the jpeg feature")
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

    /// A soft picture: two slow ramps and a little noise, like a camera's.
    fn photo(w: usize, h: usize) -> Vec<u8> {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        (0..w * h).map(|i| (40 + (i % w) * 150 / w + (i / w) * 50 / h + (rng(&mut s) % 9) as usize) as u8).collect()
    }

    /// Dark boxes on a white ground, like a diagram.
    fn diagram(w: usize, h: usize) -> Vec<u8> {
        let mut px = vec![255u8; w * h];
        for k in 0..12 {
            let (x0, y0) = (w * (k % 4) / 4 + 5, h * (k / 4) / 3 + 5);
            for y in y0..(y0 + h / 6).min(h) {
                for x in x0..(x0 + w / 8).min(w) {
                    px[y * w + x] = 20 + 10 * k as u8;
                }
            }
        }
        px
    }

    #[test]
    fn levels_are_the_contracts() {
        assert!(Lossy::level(0).is_off() && !Lossy::level(1).is_off());
        assert_eq!(Lossy::level(1), Lossy { photo_ppi: 300, art_ppi: 300, jpeg_quality: 85, rejpeg: false });
        assert_eq!(Lossy::level(2).photo_ppi, 150);
        assert_eq!(Lossy::level(3), Lossy::level(200));
        assert!(Lossy::level(3).rejpeg && Lossy::level(3).jpeg_quality == 75);
    }

    #[test]
    fn photos_and_line_art_are_told_apart() {
        let (w, h) = (320, 240);
        assert!(is_photo(&photo(w, h), w, h));
        assert!(!is_photo(&diagram(w, h), w, h), "flat areas");
        // Text-like: a soft ground with many hard edges and no flat run.
        let mut text = photo(w, h);
        for (i, v) in text.iter_mut().enumerate() {
            if (i % w) % 6 < 2 && (i / w) % 9 < 6 {
                *v = 0;
            }
        }
        assert!(!is_photo(&text, w, h), "hard edges");
        // A few levels hold most of it, without a run of 32: a two-level checker of 4 by 4.
        let checker: Vec<u8> =
            (0..w * h).map(|i| if ((i % w) / 4 + (i / w) / 4) % 2 == 0 { 100 } else { 130 }).collect();
        assert!(!is_photo(&checker, w, h), "few levels");
        // A flat band over a tenth of a photo: more than 1/16 of it is flat.
        let mut banded = photo(w, h);
        banded[..w * h / 10].fill(255);
        assert!(!is_photo(&banded, w, h));
        assert!(!is_photo(&[1, 2, 3], 3, 1) && !is_photo(&[], 0, 0));
    }

    #[test]
    fn resampling_is_an_exact_area_average() {
        let none = Deadline::NONE;
        // Halving averages squares of four, rounded to the nearest.
        let src = [10u8, 20, 30, 40, 50, 60, 70, 80, 1, 2, 3, 4, 5, 6, 7, 9];
        assert_eq!(resample(&src, 4, 4, 1, 2, 2, none).unwrap(), vec![35, 55, 4, 6]);
        // The same size gives the same samples; a constant image stays constant at any size.
        assert_eq!(resample(&src, 4, 4, 1, 4, 4, none).unwrap(), src.to_vec());
        for (dw, dh) in [(1, 1), (3, 2), (5, 7), (7, 5)] {
            assert_eq!(resample(&[77u8; 7 * 9], 7, 9, 1, dw, dh, none).unwrap(), vec![77u8; dw * dh]);
        }
        // Three to two: each output covers one whole sample and half of the middle one.
        assert_eq!(resample(&[0, 90, 30], 3, 1, 1, 2, 1, none).unwrap(), vec![30, 50]);
        // Channels are averaged apart.
        let rgb = [0u8, 100, 200, 100, 200, 0, 50, 50, 50, 250, 50, 150];
        assert_eq!(resample(&rgb, 2, 2, 3, 1, 1, none).unwrap(), vec![100, 100, 100]);
        // The weights of every output add up to the input's length.
        for (s, d) in [(7usize, 3usize), (1000, 333), (65_535, 1), (5, 5)] {
            for (first, weights) in spans(s, d) {
                assert_eq!(weights.iter().sum::<u32>() as usize, s);
                assert!(first + weights.len() <= s);
            }
        }
    }

    #[test]
    fn the_target_size_keeps_the_floor() {
        // 600 pixels over an inch, floor 300: half the size. Below 5/4 of the floor: left alone.
        assert_eq!(target(600, 300, 600.0, 300.0), Some((300, 150)));
        assert_eq!(target(600, 600, 374.0, 300.0), None);
        assert_eq!(target(600, 600, 375.0, 300.0), Some((480, 480)));
        // Sides are rounded up, so the resolution never goes under the floor.
        let (w, h) = target(1001, 333, 451.0, 150.0).unwrap();
        assert!(w as f64 * 451.0 / 1001.0 >= 150.0 && h as f64 * 451.0 / 333.0 >= 150.0);
        assert_eq!(target(3, 3, 1e9, 150.0), Some((1, 1)));
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn errors_are_counted_whole_and_by_block() {
        let (w, h) = (40, 33);
        let a = vec![100u8; w * h];
        assert_eq!(errors(&a, &a, w, h), (0, 0));
        // One sample off by 16 in the last block (24 wide, 17 high): 256 over 40 x 33, and over 24 x 17.
        let mut b = a.clone();
        b[w * h - 1] = 116;
        assert_eq!(errors(&a, &b, w, h), (25600 / (40 * 33), 25600 / (24 * 17)));
        // Every sample off by 5: 25 everywhere.
        let c = vec![105u8; w * h];
        assert_eq!(errors(&a, &c, w, h), (2500, 2500));
    }

    #[test]
    fn worth_is_a_tenth() {
        assert!(worth(90, 100) && !worth(91, 100) && worth(0, 0) && !worth(1, 1));
    }
}
