//! The lossy image step of a normal PDF: images resampled to the resolution they are shown at, and photos
//! written as JPEG, within floors that hold at every level (see `Lossy`, and `LossyRules` for the floors and
//! the other values the step works by, each a setting with a default). Off unless asked for.

/// What the lossy image step may do to a normal PDF's images. `Lossy::OFF` changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lossy {
    /// A photo shown at 5/4 of this many pixels per inch or more (the rule `lossy_resample_margin`) is resampled
    /// down to it (never below the rule `lossy_photo_floor`, 150 unless it is set) on its way to a JPEG, so not
    /// while `jpeg_quality` is 0; 0: photos are never resampled.
    pub photo_ppi: u16,
    /// Line art (screenshots, charts, diagrams) and scans shown at 5/4 of this or more are resampled down to it
    /// (never below the rule `lossy_art_floor`, 300 unless it is set); 0: never.
    pub art_ppi: u16,
    /// JPEG quality on libjpeg's scale (kept within the rules `lossy_quality_min` and `lossy_quality_max`, 50
    /// and 95) for a photo that is resampled or stored without JPEG; 0: no image becomes a JPEG.
    pub jpeg_quality: u8,
    /// Also write an existing JPEG photo again at `jpeg_quality` when that is at least a tenth smaller (the
    /// rule `lossy_min_saving`).
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

crate::rules::rule_group! {
    /// The rules of the lossy image step that are the same at every level: the group `lossy_` of
    /// `Options::set_rule` (`lossy_min_pixels` and so on). `LossyRules::default()` is what the step does when no
    /// rule is set, and `set` takes a value within the rule's range only, so the step never works with one
    /// outside it. docs/reference.md ("Rules") has the table, with why each range ends where it does.
    pub struct LossyRules {
        /// An image of at most this many pixels is never touched (logos, icons). Under 10,000 an image has too
        /// few samples to tell a photo from line art.
        min_pixels = 65_536, 10_000, 4_294_967_295;
        /// The least `photo_ppi` can be, in pixels per inch: a lower one is raised to it.
        photo_floor = 150, 72, 65_535;
        /// The same for `art_ppi` (line art and scans).
        art_floor = 300, 150, 65_535;
        /// An image is resampled only when it is shown at this percent of its floor or more.
        resample_margin = 125, 100, 1000;
        /// A lossy result is kept only when it is this percent smaller than the image's lossless size or more;
        /// the same for a JPEG written again. That size is a quick encode: the strongest one was measured up to
        /// 5% smaller on photographs, so under 10 a lossy output could be larger than the lossless one.
        min_saving = 10, 10, 99;
        /// The least `jpeg_quality` can be.
        quality_min = 50, 1, 100;
        /// The most it can be (it wins over `quality_min`), and where the quality steps stop.
        quality_max = 95, 1, 100;
        /// A JPEG that fails the two checks below is coded again this much higher.
        quality_step = 10, 1, 99;
        /// How many qualities are tried, the first one counted; 1: none after it.
        quality_tries = 3, 1, 10;
        /// The least peak signal-to-noise ratio, in dB, of a new JPEG's luma against the samples it was made
        /// from, over the whole image.
        min_psnr = 35, 1, 99;
        /// The same in its worst block.
        min_block_psnr = 26, 1, 99;
        /// The side of those blocks, in pixels.
        psnr_block = 16, 8, 64;
        /// An image drawn over this percent of its page or more is a scan: it gets the line-art floor whatever
        /// it looks like.
        scan_cover = 70, 1, 100;
        /// Content operators (and one for every 64 bytes decoded) the page walk may read in one file; over it
        /// no image of the file is reduced.
        walk_operators = 20_000_000, 1, 2_000_000_000;
        /// Image and form draws the walk may make in one file, a form's counted every time the form is drawn;
        /// over it no image of the file is reduced.
        walk_draws = 1_000_000, 1, 10_000_000;
    }
}

// ---------------------------------------------------------------- the step
//
// It runs on the loaded objects before the lossless pipeline plans its work (`rewrite::rewrite`): an image it
// reduces gets a new dictionary and new data there, and everything after it (the stream jobs, the writer, the
// self-check) treats the new image like any other stream.
//
// The numbers below are the defaults of `LossyRules`, which holds each of them.
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
// from (`mse_limit`); a JPEG that does not is first coded again at a higher quality
// (`photo_jpeg`). Any other candidate is dropped.

use super::filters::{self, chain, decode_stage, is_general, Stage};
use super::images;
use super::object::{to_bytes, Dict, Obj, Stream};
use super::place::{self, Shown};
use super::rewrite::{image_level, png_parms, predictor_sample, Options};
use crate::clock::Deadline;
use crate::flate;
use crate::par;
use std::borrow::Cow;
use std::collections::BTreeMap;

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
    // No image the step may work on: the pages are not walked.
    let none = Shown { width: 0.0, height: 0.0, scan: false };
    if !objs.values().any(|(_, o)| matches!(o, Obj::Stream(s) if eligible(s, none, objs, opt).is_some())) {
        return Done::default();
    }
    let shown = place::shown(objs, trailer, opt.max_stream, opt.deadline, &opt.rules);
    let images: Vec<(u32, Image)> = shown
        .iter()
        .filter_map(|(&n, &shown)| match objs.get(&n) {
            Some((_, Obj::Stream(s))) => Some((n, eligible(s, shown, objs, opt)?)),
            _ => None,
        })
        .collect();
    // The biggest first, as the stream jobs are started. Sorted as pairs of plain numbers, a sort the engine
    // already has: a sort by a key of its own is several KB more in the WebAssembly module.
    let size = |i: &(u32, Image)| u32::MAX - u32::try_from(i.1.s.data.len()).unwrap_or(u32::MAX);
    let mut order: Vec<(u32, usize)> = images.iter().enumerate().map(|(k, i)| (size(i), k)).collect();
    order.sort_unstable();
    let order: Vec<usize> = order.into_iter().map(|o| o.1).collect();
    let made = par::map(pool, images.len(), Some(&order), |k| {
        let (n, img) = &images[k];
        // Out of time: nothing more is made (the whole rewrite is given up after the step).
        let made = if opt.deadline.passed() {
            Err("out of time")
        } else {
            candidate(img, opt).and_then(|c| smaller(c, img.s))
        };
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

/// The candidate when it is smaller than the stream it replaces with both dictionaries counted: the
/// /DecodeParms of a predictor can cost more than resampling a nearly blank image saves.
fn smaller(c: Candidate, s: &Stream) -> Result<Candidate, &'static str> {
    let size = |d: &Dict, data: &[u8]| to_bytes(&Obj::Dict(d.clone())).len() + data.len();
    if size(&c.dict, &c.data) < size(&s.dict, &s.data) {
        Ok(c)
    } else {
        Err("not smaller with its dictionary")
    }
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
/// a grey or RGB colour space (`channels`), more than `min_pixels` pixels, at most 65,535 each way, decoding within
/// `max_stream`; without /ImageMask, /Decode, /Alternates, /OPI or an external file, and without the short
/// keys of an inline image (/W, /H and so on: a reader that takes one for the full key would go on reading
/// the old value); with no colour-key /Mask (those name sample values) and no soft mask that carries /Matte
/// (its colours are blended with this image's); stored with general filters only, or with one JPEG without
/// parameters after them. A mask or soft mask image is its own object and keeps its own size.
fn eligible<'a>(s: &'a Stream, shown: Shown, objs: &BTreeMap<u32, (u16, Obj)>, opt: &Options) -> Option<Image<'a>> {
    let d = &s.dict;
    if d.name(b"Subtype") != Some(b"Image") || d.get(b"BitsPerComponent") != Some(&Obj::Int(8)) {
        return None;
    }
    let refused: [&[u8]; 12] =
        [b"ImageMask", b"Decode", b"Alternates", b"OPI", b"F", b"W", b"H", b"BPC", b"CS", b"D", b"DP", b"IM"];
    if refused.iter().any(|k| d.has(k)) {
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
    if pixels <= opt.rules.min_pixels || pixels * channels as u64 > opt.max_stream as u64 {
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
/// below `margin` percent of the floor (resampling by a few percent costs sharpness and saves little); else the
/// size at which its lower resolution is the floor, each side rounded up, and None when that is the size it
/// has (a margin of 100, or a side of a few pixels). `floor` is a whole number, so the product is exact and the
/// division rounds the same way on every machine (at 125 it is exact too).
fn target(w: usize, h: usize, ppi: f64, floor: f64, margin: u64) -> Option<(usize, usize)> {
    if ppi < floor * margin as f64 / 100.0 {
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
    let to = (up(w as f64 * scale).clamp(1, w), up(h as f64 * scale).clamp(1, h));
    (to != (w, h)).then_some(to)
}

/// The candidate for one image, or why it is left alone.
fn candidate(img: &Image, opt: &Options) -> Result<Candidate, &'static str> {
    let (lossy, rules) = (opt.lossy, opt.rules);
    let (w, h, stored) = (img.width, img.height, img.s.data.len());
    // The image's resolution as shown: the lower of its two directions at its largest placement.
    if !(img.shown.width > 0.0 && img.shown.height > 0.0) {
        return Err("shown at no size");
    }
    let ppi = (w as f64 / img.shown.width).min(h as f64 / img.shown.height);
    let floor = |asked: u16, least: u64| (asked > 0).then(|| (asked as u64).max(least) as f64);
    let art_floor = floor(lossy.art_ppi, rules.art_floor);
    // A scan gets the line-art floor whatever its samples look like.
    let photo_floor = if img.shown.scan { art_floor } else { floor(lossy.photo_ppi, rules.photo_floor) };
    // `quality_max` wins when the two cross (`clamp` would panic).
    let within = |q: u8| (q as u64).max(rules.quality_min).min(rules.quality_max) as u8;
    let quality = (cfg!(feature = "jpeg") && lossy.jpeg_quality > 0).then(|| within(lossy.jpeg_quality));
    let size = |f: Option<f64>| f.and_then(|f| target(w, h, ppi, f, rules.resample_margin));
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
        return from_jpeg(img, &data, size(photo_floor), quality, opt);
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
    let worth = |old: usize| worth(z.len(), old, rules.min_saving);
    if !worth(stored) || !worth(quick_flate(px, w, img.channels, opt).1.len()) {
        return Err("line art: not smaller by `min_saving`");
    }
    let dict = new_dict(&img.s.dict, tw, th, b"FlateDecode", parms, z.len());
    Ok(Candidate { dict, data: z, resampled: true, to_jpeg: false, what: "line art resampled" })
}

/// Whether a candidate of `new` bytes is worth it against `old`: smaller by `saving` percent or more (10: at
/// most nine tenths of it).
fn worth(new: usize, old: usize, saving: u64) -> bool {
    new as u64 * 100 <= old as u64 * (100 - saving.min(100))
}

/// The image's dictionary for new data: every entry kept but the size, the filter entries, the length and
/// /DL (a hint of how many bytes the data decodes to, which is no longer true).
fn new_dict(d: &Dict, w: usize, h: usize, filter: &[u8], parms: Option<Dict>, len: usize) -> Dict {
    let mut d = d.clone();
    d.remove(b"DL");
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
/// A step of this many levels, and less than `EDGE`, is a soft edge.
const SOFT: u8 = 32;
/// The side of the tiles small print is looked for in. At 32, each of four drawings of labels on a photograph
/// (6 and 8 point at 504 samples an inch: black, white, outlined) has 8 or more tiles of small print; at 64
/// none has more than one.
const TILE: usize = 32;

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
///   are a ground and its inks;
/// - no whole tile of `TILE` by `TILE` samples has hard edges on more than 1/8 of it that outnumber its soft
///   edges (samples `SOFT` levels or more from a neighbour, and less than `EDGE`): small print. A few labels,
///   a caption or a subtitle on a photograph are too few hard edges for the count over the whole picture,
///   but where they stand the edges are dense and sudden, and a photo's floor would blur them. Among the
///   photographs the three counts above take, measured as stored and as JPEGs of three qualities, one tile
///   is like that: the glinting corner of a picture frame.
///
/// Not found: a word or two (a tile may hold too little of them), and print that is soft itself (a
/// photographed or scanned note), which no count of edges tells from a photograph.
fn is_photo(luma: &[u8], w: usize, h: usize) -> bool {
    if w < 2 || h < 2 || luma.len() < w * h {
        return false;
    }
    let (mut flat, mut hard) = (0u64, 0u64);
    // The hard and the soft edges of each whole tile in the band of rows being read.
    let mut tiles = vec![(0u32, 0u32); w / TILE];
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
            let step = v.abs_diff(left).max(v.abs_diff(above[x]));
            hard += (step >= EDGE) as u64;
            if let Some(tile) = tiles.get_mut(x / TILE) {
                tile.0 += (step >= EDGE) as u32;
                tile.1 += (SOFT..EDGE).contains(&step) as u32;
            }
        }
        if y % TILE == TILE - 1 {
            if tiles.iter().any(|&(hard, soft)| hard as usize * 8 > TILE * TILE && hard > soft) {
                return false;
            }
            tiles.fill((0, 0));
        }
    }
    let all = (w * h) as u64;
    // The 8 most frequent levels, picked one at a time: so few need no sort.
    let mut top = 0;
    for _ in 0..8 {
        let most = (0..256).max_by_key(|&k| levels[k]).unwrap_or(0);
        top += std::mem::take(&mut levels[most]);
    }
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

/// The most luma error a JPEG candidate may have against the samples it was made from to keep a peak
/// signal-to-noise ratio of `db` dB, as mean squared error in hundredths: 255 squared, times 100, over 10 to
/// the power of a tenth of `db`, in whole numbers (the ten powers below are times a million), so that every
/// machine gets the same limit. The defaults: 2056 over the whole image (35 dB) and 16334 in its worst block of
/// 16 by 16 (26 dB). A smooth photo is well inside both at quality 75; one with much fine detail needs 85 or
/// more for the first (two landscapes of the test corpus: 32.8 and 35.9 dB at 75, 35.7 and 38.4 dB at 85). A
/// JPEG with ringing around hard edges (line art taken for a photo) falls out on the block floor.
#[cfg(feature = "jpeg")]
fn mse_limit(db: u64) -> u64 {
    const TENTHS: [u64; 10] =
        [1_000_000, 1_258_925, 1_584_893, 1_995_262, 2_511_886, 3_162_278, 3_981_072, 5_011_872, 6_309_573, 7_943_282];
    let over = 10u64.pow((db.min(99) / 10) as u32) * TENTHS[(db % 10) as usize];
    (6_502_500 * 1_000_000 + over / 2) / over
}

/// The luma error of `b` against `a` (both `w` by `h`), as mean squared error in hundredths: over the whole
/// image, and in the worst of its blocks of `side` by `side` (the last block of a row or column takes the
/// samples left over, so every block has at least `side` each way where the image does).
#[cfg(feature = "jpeg")]
fn errors(a: &[u8], b: &[u8], w: usize, h: usize, side: usize) -> (u64, u64) {
    let (across, down) = ((w / side).max(1), (h / side).max(1));
    let (mut whole, mut worst) = (0u64, 0u64);
    for by in 0..down {
        let (y0, y1) = (by * side, if by + 1 == down { h } else { (by + 1) * side });
        let mut blocks = vec![0u64; across];
        for y in y0..y1 {
            let (ra, rb) = (&a[y * w..(y + 1) * w], &b[y * w..(y + 1) * w]);
            for x in 0..w {
                let d = ra[x].abs_diff(rb[x]) as u64;
                blocks[(x / side).min(across - 1)] += d * d;
            }
        }
        for (bx, &sum) in blocks.iter().enumerate() {
            let width = if bx + 1 == across { w - bx * side } else { side };
            worst = worst.max(sum * 100 / (width * (y1 - y0)) as u64);
            whole += sum;
        }
    }
    (whole * 100 / (w as u64 * h as u64).max(1), worst)
}

/// A photo's planes (grey, or Y with Cb and Cr at any size) as a JPEG, with its size: the planes brought to
/// `size` when there is one (the chroma planes to half of it each way, which the encoder asks for; a chroma
/// plane smaller than that is not enlarged, the photo is left alone), coded at `quality`, and given optimal
/// Huffman tables. The JPEG must be worth it against `stored` and `lossless()` (the image's stored bytes and
/// its quick lossless size), and decode close to the planes it was made from: when it does not, it is coded
/// again `quality_step` higher, `quality_tries` times in all and at most at `quality_max` (10 and then 20
/// higher, at most 95), so that a photo with much fine detail is still written, with more bits, instead of
/// staying as large as it was.
#[cfg(feature = "jpeg")]
fn photo_jpeg(
    mut planes: Vec<Plane>,
    size: Option<(usize, usize)>,
    quality: u8,
    stored: usize,
    lossless: impl FnOnce() -> usize,
    opt: &Options,
) -> Result<(Vec<u8>, (usize, usize)), &'static str> {
    let (w, h) = size.unwrap_or((planes[0].w, planes[0].h));
    for (i, p) in planes.iter_mut().enumerate() {
        let (pw, ph) = if i == 0 { (w, h) } else { (w.div_ceil(2), h.div_ceil(2)) };
        if p.w < pw || p.h < ph {
            return Err("chroma too coarse to code again");
        }
        if (p.w, p.h) != (pw, ph) {
            let px = resample(&p.px, p.w, p.h, 1, pw, ph, opt.deadline).ok_or("out of time")?;
            *p = Plane { w: pw, h: ph, px };
        }
    }
    let (mut lossless, mut limit) = (Some(lossless), stored);
    let mut why = "";
    let rules = opt.rules;
    // The quality of each try; `quality` is within `quality_max` already.
    let at = |t: u64| (quality as u64 + t * rules.quality_step).min(rules.quality_max.max(quality as u64)) as u8;
    for t in 0..rules.quality_tries {
        let q = at(t);
        if t > 0 && q == at(t - 1) {
            break;
        }
        let coded = jpeg::encode(&planes, q, opt.deadline).ok_or("the JPEG encoder refused it")?;
        let coded = jpeg::optimize(&coded).unwrap_or(coded);
        // The quick lossless size is worked out once, and only for a JPEG that beats the stored bytes.
        if worth(coded.len(), limit, rules.min_saving) {
            if let Some(f) = lossless.take() {
                limit = limit.min(f());
            }
        }
        if !worth(coded.len(), limit, rules.min_saving) {
            return Err("not smaller by `min_saving` as a JPEG");
        }
        match close(&coded, &planes[0], opt) {
            Ok(()) => return Ok((coded, (w, h))),
            Err(e) => why = e,
        }
    }
    Err(why)
}

/// Whether the JPEG `coded` decodes to luma close enough to `y`, the plane it was made from.
#[cfg(feature = "jpeg")]
fn close(coded: &[u8], y: &Plane, opt: &Options) -> Result<(), &'static str> {
    let back = jpeg::decode(coded, opt.max_stream, opt.deadline).map_err(|_| "the new JPEG does not decode")?;
    let luma = back.planes.first().filter(|p| (p.w, p.h) == (y.w, y.h)).ok_or("the new JPEG has another size")?;
    let (whole, worst) = errors(&y.px, &luma.px, y.w, y.h, opt.rules.psnr_block as usize);
    if whole > mse_limit(opt.rules.min_psnr) {
        return Err("quality: the whole image");
    }
    if worst > mse_limit(opt.rules.min_block_psnr) {
        return Err("quality: its worst block");
    }
    Ok(())
}

/// A JPEG image (`data`: the JPEG file): decoded to its own planes, and when their luma is a photo's, resampled
/// to `size` or, without one, only coded again. A JPEG that looks like line art is left alone: line art is
/// never made a JPEG, and its samples behind Flate would need a colour conversion.
#[cfg(feature = "jpeg")]
fn from_jpeg(
    img: &Image,
    data: &[u8],
    size: Option<(usize, usize)>,
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
    let lossless = || jpeg::optimize(data).map_or(data.len(), |j| j.len());
    let (coded, (w, h)) = photo_jpeg(pic.planes, size, quality, img.s.data.len(), lossless, opt)?;
    let dict = new_dict(&img.s.dict, w, h, b"DCTDecode", None, coded.len());
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
    let lossless = || quick_flate(px, w, img.channels, opt).1.len();
    let (coded, (w, h)) = photo_jpeg(planes, size, quality, img.s.data.len(), lossless, opt)?;
    let dict = new_dict(&img.s.dict, w, h, b"DCTDecode", None, coded.len());
    let what = if size.is_some() { "photo resampled, to JPEG" } else { "photo to JPEG" };
    Ok(Candidate { dict, data: coded, resampled: size.is_some(), to_jpeg: true, what })
}

// Without the `jpeg` feature an image that would need a JPEG is left alone (`candidate` never gets this far:
// it has no JPEG quality then).
#[cfg(not(feature = "jpeg"))]
fn from_jpeg(_: &Image, _: &[u8], _: Option<(usize, usize)>, _: u8, _: &Options) -> Result<Candidate, &'static str> {
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
        // One label on a photo, 48 by 16 samples of strokes two samples wide: one sample in 274 is a hard
        // edge, too few for the count over the whole picture, but a tile the label stands in is full of them.
        let label = |ink: fn(u8) -> u8| {
            let mut px = photo(w, h);
            for y in 100..116 {
                for x in (96..144).filter(|x| x % 6 < 2) {
                    px[y * w + x] = ink(px[y * w + x]);
                }
            }
            px
        };
        assert!(!is_photo(&label(|_| 0), w, h), "small print");
        // The same strokes as a shadow, 40 levels darker: soft edges are a photo's own.
        assert!(is_photo(&label(|v| v - 40), w, h), "a soft pattern");
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
        assert_eq!(target(600, 300, 600.0, 300.0, 125), Some((300, 150)));
        assert_eq!(target(600, 600, 374.0, 300.0, 125), None);
        assert_eq!(target(600, 600, 375.0, 300.0, 125), Some((480, 480)));
        // Sides are rounded up, so the resolution never goes under the floor.
        let (w, h) = target(1001, 333, 451.0, 150.0, 125).unwrap();
        assert!(w as f64 * 451.0 / 1001.0 >= 150.0 && h as f64 * 451.0 / 333.0 >= 150.0);
        assert_eq!(target(3, 3, 1e9, 150.0, 125), Some((1, 1)));
        // A margin of 100 takes anything over the floor, but never gives an image its own size back.
        assert_eq!(target(600, 600, 300.0, 300.0, 100), None);
        assert_eq!(target(600, 600, 300.1, 300.0, 100), None);
        assert_eq!(target(600, 600, 301.0, 300.0, 100), Some((599, 599)));
        assert_eq!(target(4, 4, 187.5, 150.0, 125), None);
    }

    #[test]
    fn a_result_counts_with_its_dictionary() {
        let s = Stream { dict: Dict::default(), data: vec![0; 200] };
        let of = |parms: Option<Dict>, len: usize| Candidate {
            dict: new_dict(&s.dict, 3, 4, b"FlateDecode", parms, len),
            data: vec![0; len],
            resampled: true,
            to_jpeg: false,
            what: "",
        };
        // The entries a new stream needs are about 50 bytes, and a predictor's /DecodeParms nearly 70 more:
        // 100 bytes of data are smaller than the 200 they replace without it, and not with it.
        assert!(smaller(of(None, 100), &s).is_ok() && smaller(of(None, 180), &s).is_err());
        let parms = || Some(png_parms(&images::Geometry { colors: 3, bpc: 8, columns: 3 }));
        assert!(smaller(of(parms(), 100), &s).is_err() && smaller(of(parms(), 40), &s).is_ok());
    }

    /// The image that showed why a result counts with its dictionary: 3 by 5 pixels shown a point square (which
    /// `min_pixels` keeps out, so the rule is set past its range here). Resampled to 3 by 4 its data is a
    /// tenth smaller, and with the predictor's /DecodeParms the stream would be larger than the one it replaces.
    #[test]
    fn a_small_image_does_not_grow_by_its_dictionary() {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut px = Vec::new();
        for i in 0..15 {
            for c in 0..3 {
                px.push(
                    (40 + (i % 3) * (150 - 30 * c) / 3 + (i / 3) * (50 + 20 * c) / 5 + (rng(&mut s) % 9) as usize)
                        as u8,
                );
            }
        }
        let mut dict = Dict::default();
        dict.set(b"Subtype", Obj::Name(b"Image".to_vec()));
        dict.set(b"Width", Obj::Int(3));
        dict.set(b"Height", Obj::Int(5));
        dict.set(b"ColorSpace", Obj::Name(b"DeviceRGB".to_vec()));
        dict.set(b"BitsPerComponent", Obj::Int(8));
        dict.set(b"Filter", Obj::Name(b"FlateDecode".to_vec()));
        let stream = Stream { dict, data: flate::deflate_zlib(&px, &flate::Options::quick()) };
        let mut opt = Options { lossy: Lossy { photo_ppi: 1, art_ppi: 1, ..Lossy::level(3) }, ..Options::default() };
        opt.rules = LossyRules { min_pixels: 1, photo_floor: 72, art_floor: 150, resample_margin: 100, ..opt.rules };
        let shown = Shown { width: 1.0 / 72.0, height: 1.0 / 72.0, scan: false };
        let img = eligible(&stream, shown, &BTreeMap::new(), &opt).expect("an image the step works on");
        let c = candidate(&img, &opt).expect("resampled, and a tenth smaller");
        assert!(
            c.data.len() < stream.data.len() && c.dict.has(b"DecodeParms"),
            "{} of {}",
            c.data.len(),
            stream.data.len()
        );
        assert_eq!(smaller(c, &stream).err(), Some("not smaller with its dictionary"));
    }

    #[test]
    fn the_rules_default_to_the_values_that_were_fixed() {
        let was = [65_536, 150, 300, 125, 10, 50, 95, 10, 3, 35, 26, 16, 70, 20_000_000, 1_000_000];
        let d = LossyRules::default();
        assert_eq!(LossyRules::RULES.len(), was.len());
        for (&(name, default, lowest, highest), was) in LossyRules::RULES.iter().zip(was) {
            assert_eq!((default, d.get(name)), (was, Some(was)), "{name}");
            assert!(lowest <= default && default <= highest, "{name}");
        }
        // Where the ranges end: no saving under the default (a lossy output could pass the lossless one), the
        // floors at 72 and 150 pixels per inch, no image of under 10,000 pixels.
        let mut r = d;
        assert!(!r.set("min_saving", 9) && !r.set("photo_floor", 71) && !r.set("art_floor", 149));
        assert!(!r.set("min_pixels", 9_999) && !r.set("no_such_rule", 5) && r == d && r.get("no_such_rule").is_none());
        assert!(r.set("photo_floor", 72) && r.set("art_floor", 150) && r.set("min_pixels", 10_000) && r != d);
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn decibels_become_the_error_limits_in_whole_numbers() {
        // The two limits that were fixed: 35 dB over the image, 26 dB in the worst block.
        assert_eq!((mse_limit(35), mse_limit(26)), (2056, 16334));
        // 6,502,500 over ten to the power of a tenth of the decibels, to the nearest.
        assert_eq!((mse_limit(10), mse_limit(20), mse_limit(30), mse_limit(40)), (650_250, 65_025, 6503, 650));
        assert_eq!((mse_limit(1), mse_limit(99)), (5_165_121, 0));
        assert!(
            (1..99).all(|db| mse_limit(db) >= mse_limit(db + 1)) && (1..60).all(|db| mse_limit(db) > mse_limit(db + 1))
        );
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn errors_are_counted_whole_and_by_block() {
        let (w, h) = (40, 33);
        let a = vec![100u8; w * h];
        assert_eq!(errors(&a, &a, w, h, 16), (0, 0));
        // One sample off by 16 in the last block (24 wide, 17 high): 256 over 40 x 33, and over 24 x 17.
        let mut b = a.clone();
        b[w * h - 1] = 116;
        assert_eq!(errors(&a, &b, w, h, 16), (25600 / (40 * 33), 25600 / (24 * 17)));
        // Every sample off by 5: 25 everywhere.
        let c = vec![105u8; w * h];
        assert_eq!(errors(&a, &c, w, h, 16), (2500, 2500));
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn a_busy_photo_is_coded_at_a_higher_quality() {
        // A ramp under noise of 41 levels: far from its samples at a low quality, close at a high one.
        let (w, h) = (160, 120);
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let px = (0..w * h).map(|i| (60 + (i % w) / 2 + (rng(&mut s) % 41) as usize) as u8).collect();
        let y = Plane { w, h, px };
        let opt = Options::default();
        let at = |q: u8| jpeg::encode(std::slice::from_ref(&y), q, Deadline::NONE).unwrap();
        assert!(close(&at(75), &y, &opt).is_err() && close(&at(95), &y, &opt).is_ok());
        let (coded, size) = photo_jpeg(vec![y.clone()], None, 75, 1 << 30, || 1 << 30, &opt).unwrap();
        assert_eq!(size, (w, h));
        assert_eq!(close(&coded, &y, &opt), Ok(()));
        // Two steps up from 50 are not enough: the photo is left alone. So is one that is not worth it.
        let low = photo_jpeg(vec![y.clone()], None, 50, 1 << 30, || 1 << 30, &opt);
        assert!(low.unwrap_err().starts_with("quality"));
        let big = photo_jpeg(vec![y.clone()], None, 75, 1 << 30, || 10, &opt);
        assert_eq!(big.unwrap_err(), "not smaller by `min_saving` as a JPEG");
        // The rules of the ladder: a second try 45 higher gets from 50 to 95, steps of 20 from 55 to 95; one try
        // is the first quality only, and a step past `quality_max` stops there.
        let with = |rules: &[(&str, u64)], q: u8| {
            let mut opt = Options::default();
            assert!(rules.iter().all(|(name, v)| opt.rules.set(name, *v)));
            photo_jpeg(vec![y.clone()], None, q, 1 << 30, || 1 << 30, &opt).map(|(coded, _)| coded)
        };
        let at95 = with(&[], 95).unwrap();
        assert!(with(&[("quality_step", 45), ("quality_tries", 2)], 50).unwrap() == at95);
        assert!(with(&[("quality_step", 20)], 55).unwrap() == at95);
        assert!(with(&[("quality_step", 99)], 50).unwrap() == at95);
        assert!(with(&[("quality_tries", 1)], 75).unwrap_err().starts_with("quality"));
        assert!(with(&[("quality_step", 99), ("quality_max", 60)], 50).unwrap_err().starts_with("quality"));
        // A looser floor takes the first quality; a block of another side still lines up.
        assert!(with(&[("min_psnr", 20), ("min_block_psnr", 10)], 50).is_ok());
        assert!(with(&[("psnr_block", 8)], 95).is_ok() && with(&[("psnr_block", 64)], 95).is_ok());
    }

    #[test]
    fn worth_is_a_tenth() {
        assert!(worth(90, 100, 10) && !worth(91, 100, 10) && worth(0, 0, 10) && !worth(1, 1, 10));
    }
}
