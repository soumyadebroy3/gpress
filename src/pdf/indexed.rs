//! Images of few colours as Indexed images: every pixel becomes an index into a table of the colours the image
//! uses, over the image's own colour space, so each pixel keeps exactly the colour it had.
//!
//! An image is written this way only when nothing about it is uncertain: 8 bits per component; the colour
//! space /DeviceGray or /DeviceRGB, named in the image itself; general filters only (the caller's business);
//! direct /Width and /Height that the samples fill exactly; no /Decode, colour-key /Mask, /ImageMask,
//! /Interpolate or /Alternates; not the mask of another image (a soft mask must stay DeviceGray) and no soft
//! mask with a /Matte colour, which is given in the image's colour space (`allowed`, which needs the object
//! table); and few enough colours for an index to take fewer bits than the samples of a pixel: at most 256
//! colours of DeviceRGB, 16 of DeviceGray. The indexes take 8, 4 or 2 bits for at most 256, 16 or 4 colours,
//! rows padded to a byte (never 1 bit: PDFium 5.13 draws a 1-bit image that it scales another way than the
//! 8-bit image it was made of). The table holds the colours in ascending order of their bytes, so it does not
//! depend on where in the image a colour comes first.
//!
//! CalGray, CalRGB and ICCBased images are left alone: a reader may take an image's samples and a table's
//! colours through such a colour space by different routes that round differently (Poppler 24.02 draws an
//! Indexed image over an ICCBased space up to 9 levels of 255 away from the image it was made of, PDFium 5.13
//! one over CalRGB). For the same reason no image of a file that names a default colour space (/DefaultGray,
//! /DefaultRGB) is written this way (the caller's business too).

use super::object::{Dict, Obj};
use std::collections::{BTreeMap, HashSet};

/// The entries an Indexed image has in another form than the image it was made of.
fn own_entry(k: &[u8]) -> bool {
    matches!(k, b"Length" | b"Filter" | b"DecodeParms" | b"ColorSpace" | b"BitsPerComponent")
}

/// Bits of an index into a table of `n` colours. Never 1: PDFium draws a 1-bit image that it scales another
/// way than an 8-bit one.
fn index_bits(n: usize) -> usize {
    match n {
        0..=4 => 2,
        5..=16 => 4,
        _ => 8,
    }
}

/// An image dictionary that may become Indexed as far as it says itself (see the module notes): the components
/// of its colour space (1 or 3), its width and its height.
pub fn plain_image(d: &Dict) -> Option<(usize, usize, usize)> {
    if d.name(b"Subtype") != Some(b"Image") || d.get(b"BitsPerComponent") != Some(&Obj::Int(8)) {
        return None;
    }
    let off = |k: &[u8]| matches!(d.get(k), None | Some(Obj::Bool(false)));
    // A /Mask that is a reference may be a stencil mask, which is fine: `allowed` looks it up.
    let colour_key = !matches!(d.get(b"Mask"), None | Some(Obj::Ref(..)));
    if d.has(b"Decode") || d.has(b"Alternates") || colour_key || !off(b"ImageMask") || !off(b"Interpolate") {
        return None;
    }
    let m = match d.name(b"ColorSpace")? {
        b"DeviceGray" => 1,
        b"DeviceRGB" => 3,
        _ => return None,
    };
    let size = |k: &[u8]| d.get(k)?.as_int().and_then(|v| usize::try_from(v).ok()).filter(|&v| v > 0);
    Some((m, size(b"Width")?, size(b"Height")?))
}

/// What only the object table can say about image `n` with dictionary `d`: it is not named as the /SMask or
/// /Mask of another object (`masks`: every object that is), its /Mask, if any, is a stream (a stencil mask, not
/// a colour key), and its /SMask, if any, is a stream without a /Matte colour.
pub fn allowed(n: u32, d: &Dict, objs: &BTreeMap<u32, (u16, Obj)>, masks: &HashSet<u32>) -> bool {
    // None: not there. Some(None): there, and not a reference to a stream.
    let stream = |k: &[u8]| {
        d.get(k).map(|v| match v {
            Obj::Ref(r, _) => match objs.get(r) {
                Some((_, Obj::Stream(s))) => Some(&s.dict),
                _ => None,
            },
            _ => None,
        })
    };
    !masks.contains(&n)
        && stream(b"Mask").is_none_or(|m| m.is_some())
        && stream(b"SMask").is_none_or(|m| m.is_some_and(|m| !m.has(b"Matte")))
}

/// The image of dictionary `d` and samples `px` (its general filters decoded, a predictor undone) as an Indexed
/// image: its dictionary, without filters, and its indexes. None when it is not one to write this way (see the
/// module notes; `allowed` is the caller's to ask).
pub fn convert(d: &Dict, px: &[u8]) -> Option<(Dict, Vec<u8>)> {
    let (m, w, h) = plain_image(d)?;
    if (w as u64).checked_mul(h as u64)?.checked_mul(m as u64)? != px.len() as u64 {
        return None;
    }
    let most = if m == 1 { 16 } else { 256 };
    // The colours in ascending order. A run of one colour is looked up once.
    let mut colours: Vec<&[u8]> = Vec::new();
    let mut last: &[u8] = &[];
    for c in px.chunks_exact(m) {
        if c == last {
            continue;
        }
        last = c;
        if let Err(at) = colours.binary_search(&c) {
            if colours.len() == most {
                return None;
            }
            colours.insert(at, c);
        }
    }
    let bits = index_bits(colours.len());
    let row = (w * bits).div_ceil(8);
    let mut out = vec![0u8; row * h];
    let mut last: &[u8] = &[];
    let mut index = 0u8;
    for (line, packed) in px.chunks_exact(w * m).zip(out.chunks_exact_mut(row)) {
        for (x, c) in line.chunks_exact(m).enumerate() {
            if c != last {
                last = c;
                index = colours.binary_search(&c).ok()? as u8;
            }
            packed[x * bits / 8] |= index << (8 - bits - x * bits % 8);
        }
    }
    let mut d2 = d.clone();
    d2.remove(b"Filter");
    d2.remove(b"DecodeParms");
    let base = d.get(b"ColorSpace")?.clone();
    let space =
        vec![Obj::Name(b"Indexed".to_vec()), base, Obj::Int(colours.len() as i64 - 1), Obj::Str(colours.concat())];
    d2.set(b"ColorSpace", Obj::Array(space));
    d2.set(b"BitsPerComponent", Obj::Int(bits as i64));
    Some((d2, out))
}

/// For the self-check: the stream of dictionary `b` and content `indexes` is the image of dictionary `a` and
/// content `px` as `convert` writes it. `a` is an image that may be written so (`plain_image`; `allowed` is
/// the caller's to ask); `b` has `a`'s entries, but for the encoding's, an Indexed colour space over `a`'s own
/// with a table of the right size and the bits per index that go with it; and every index looks up the colour
/// its pixel had.
pub fn same_image(a: &Dict, px: &[u8], b: &Dict, indexes: &[u8]) -> bool {
    let Some((m, w, h)) = plain_image(a) else { return false };
    if !a.0.iter().filter(|(k, _)| !own_entry(k)).eq(b.0.iter().filter(|(k, _)| !own_entry(k))) {
        return false;
    }
    let Some(Obj::Array(space)) = b.get(b"ColorSpace") else { return false };
    let [Obj::Name(family), base, Obj::Int(hival), Obj::Str(table)] = space.as_slice() else { return false };
    if family != b"Indexed" || a.get(b"ColorSpace") != Some(base) || !(0..=255).contains(hival) {
        return false;
    }
    let (n, bits) = (*hival as usize + 1, index_bits(*hival as usize + 1));
    if b.get(b"BitsPerComponent") != Some(&Obj::Int(bits as i64)) || table.len() != n * m {
        return false;
    }
    let row = (w as u64 * bits as u64).div_ceil(8);
    if (w as u64).checked_mul(h as u64).and_then(|p| p.checked_mul(m as u64)) != Some(px.len() as u64)
        || row.checked_mul(h as u64) != Some(indexes.len() as u64)
    {
        return false;
    }
    px.chunks_exact(w * m).zip(indexes.chunks_exact(row as usize)).all(|(line, packed)| {
        line.chunks_exact(m).enumerate().all(|(x, c)| {
            let i = (packed[x * bits / 8] >> (8 - bits - x * bits % 8)) as usize & ((1 << bits) - 1);
            table.get(i * m..(i + 1) * m) == Some(c)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(text: &str) -> Dict {
        match super::super::parse::Lexer::new(text.as_bytes(), 0).parse_obj() {
            Ok(Obj::Dict(d)) => d,
            x => panic!("{text}: {x:?}"),
        }
    }

    fn image(extra: &str, w: usize, h: usize) -> Dict {
        dict(&format!("<< /Subtype /Image /Width {w} /Height {h} /BitsPerComponent 8 {extra} >>"))
    }

    fn stream(text: &str) -> Obj {
        Obj::Stream(Box::new(super::super::object::Stream { dict: dict(text), data: Vec::new() }))
    }

    /// A `w` x `h` image of `n` colours of `m` components (all of them once it has `n` pixels; `n` is not a
    /// multiple of 7), in an order that is not the sorted one.
    fn samples(n: usize, m: usize, w: usize, h: usize) -> Vec<u8> {
        (0..w * h).flat_map(|i| (0..m).map(move |c| (i * 7 % n * 37 + c * 11) as u8)).collect()
    }

    #[test]
    fn the_indexes_look_up_every_pixel_at_each_depth() {
        for (n, m, bits) in [(1, 3, 2), (2, 3, 2), (3, 3, 2), (4, 1, 2), (5, 3, 4), (16, 1, 4), (17, 3, 8), (256, 3, 8)]
        {
            // Widths whose rows end inside a byte, too.
            for w in [1, 5, 8, 13] {
                let cs = if m == 1 { "/DeviceGray" } else { "/DeviceRGB" };
                let a = image(&format!("/ColorSpace {cs} /Filter /FlateDecode /Intent /Perceptual"), w, 40);
                let px = samples(n, m, w, 40);
                let used = px.chunks(m).collect::<std::collections::BTreeSet<_>>();
                let (b, idx) = convert(&a, &px).unwrap_or_else(|| panic!("{n} colours of {m}, width {w}"));
                assert_eq!(b.get(b"BitsPerComponent"), Some(&Obj::Int(index_bits(used.len()) as i64)));
                assert_eq!(idx.len(), (w * index_bits(used.len())).div_ceil(8) * 40);
                if w * 40 >= n {
                    assert_eq!((used.len(), index_bits(used.len())), (n, bits));
                }
                assert!(!b.has(b"Filter") && b.name(b"Intent") == Some(b"Perceptual"));
                // The table: the colours used, ascending.
                let Some(Obj::Array(space)) = b.get(b"ColorSpace") else { panic!("{b:?}") };
                let base = a.get(b"ColorSpace").unwrap().clone();
                assert_eq!(space[..3], [Obj::Name(b"Indexed".to_vec()), base, Obj::Int(used.len() as i64 - 1)]);
                assert_eq!(space[3], Obj::Str(used.iter().flat_map(|c| c.iter().copied()).collect()));
                assert!(same_image(&a, &px, &b, &idx), "{n} colours of {m}, width {w}");
                // A pixel of another colour, a table entry changed, a row short: not the same image.
                let mut other = px.clone();
                other[px.len() / 2] ^= 1;
                assert!(!same_image(&a, &other, &b, &idx));
                let mut wrong = b.clone();
                let Some(Obj::Array(space)) = wrong.get_mut(b"ColorSpace") else { unreachable!() };
                let Obj::Str(table) = &mut space[3] else { unreachable!() };
                table[0] ^= 0x80;
                assert!(!same_image(&a, &px, &wrong, &idx));
                assert!(!same_image(&a, &px, &b, &idx[..idx.len() - 1]));
            }
        }
    }

    #[test]
    fn more_colours_than_an_index_pays_for_are_left() {
        let rgb = image("/ColorSpace /DeviceRGB", 20, 20);
        assert!(convert(&rgb, &samples(256, 3, 20, 20)).is_some());
        let mut px = samples(256, 3, 20, 20);
        px[1] = 255; // a 257th colour
        assert!(convert(&rgb, &px).is_none());
        // One component: an index of 8 bits is no smaller than the sample.
        let grey = image("/ColorSpace /DeviceGray", 20, 20);
        assert!(convert(&grey, &samples(16, 1, 20, 20)).is_some());
        assert!(convert(&grey, &samples(17, 1, 20, 20)).is_none());
        // Samples that are not exactly the image's.
        assert!(convert(&rgb, &samples(4, 3, 20, 19)).is_none());
        assert!(convert(&rgb, &samples(4, 3, 20, 21)).is_none());
    }

    #[test]
    fn only_plain_images_in_device_grey_or_rgb() {
        let px = samples(4, 3, 10, 10);
        for (extra, ok) in [
            ("/ColorSpace /DeviceRGB", true),
            ("/ColorSpace /DeviceRGB /Interpolate false /ImageMask false /Mask 7 0 R /SMask 6 0 R", true),
            ("/ColorSpace /DeviceRGB /Decode [0 1 0 1 0 1]", false),
            ("/ColorSpace /DeviceRGB /Mask [0 0 0 0 0 0]", false),
            ("/ColorSpace /DeviceRGB /Interpolate true", false),
            ("/ColorSpace /DeviceRGB /Alternates []", false),
            ("/ColorSpace /DeviceRGB /ImageMask true", false),
            ("/ColorSpace /DeviceCMYK", false),
            // A reader may round a table's colours another way than an image's samples in these.
            ("/ColorSpace [/CalRGB << /WhitePoint [0.95 1 1.09] >>]", false),
            ("/ColorSpace [/ICCBased 9 0 R]", false),
            ("/ColorSpace [/DeviceRGB]", false),
            ("/ColorSpace 8 0 R", false),
            ("/ColorSpace [/Lab << /WhitePoint [0.95 1 1.09] >>]", false),
            ("/ColorSpace [/Indexed /DeviceRGB 3 <000000111111222222333333>]", false),
            ("/ColorSpace [/Separation /Spot /DeviceRGB 5 0 R]", false),
            ("/ColorSpace /RGB", false),
            ("", false),
        ] {
            assert_eq!(convert(&image(extra, 10, 10), &px).is_some(), ok, "{extra}");
        }
        let sixteen = dict("<< /Subtype /Image /Width 10 /Height 10 /BitsPerComponent 16 /ColorSpace /DeviceRGB >>");
        assert!(convert(&sixteen, &px).is_none());
        let form = dict("<< /Subtype /Form /Width 10 /Height 10 /BitsPerComponent 8 /ColorSpace /DeviceRGB >>");
        assert!(convert(&form, &px).is_none());
    }

    #[test]
    fn masks_and_matted_images_are_not_allowed() {
        let objs: BTreeMap<u32, (u16, Obj)> = [
            (5, stream("<< /Subtype /Image >>")),
            (6, stream("<< /Subtype /Image /Matte [0 0 0] >>")),
            (7, Obj::Array(vec![Obj::Int(0); 6])),
        ]
        .into_iter()
        .map(|(n, o)| (n, (0, o)))
        .collect();
        let masks: HashSet<u32> = [4].into();
        for (extra, n, ok) in [
            ("", 3, true),
            ("", 4, false), // the mask of another image
            ("/Mask 5 0 R /SMask 5 0 R", 3, true),
            ("/SMask 6 0 R", 3, false), // a matte colour in the image's colour space
            ("/Mask 7 0 R", 3, false),  // a colour key behind a reference
            ("/Mask 9 0 R", 3, false),  // nothing there
            ("/SMask 9 0 R", 3, false),
            ("/SMask /None", 3, false),
        ] {
            assert_eq!(allowed(n, &image(extra, 10, 10), &objs, &masks), ok, "{extra} as object {n}");
        }
    }

    #[test]
    fn another_dictionary_or_colour_space_is_not_the_same_image() {
        let a = image("/ColorSpace /DeviceRGB /Name /Im1", 9, 9);
        let px = samples(5, 3, 9, 9);
        let (b, idx) = convert(&a, &px).unwrap();
        assert!(same_image(&a, &px, &b, &idx));
        let with = |f: &dyn Fn(&mut Dict)| {
            let mut d = b.clone();
            f(&mut d);
            d
        };
        let space = |f: &dyn Fn(&mut Vec<Obj>)| {
            with(&|d| {
                let Some(Obj::Array(s)) = d.get_mut(b"ColorSpace") else { unreachable!() };
                f(s)
            })
        };
        for (what, wrong) in [
            ("an entry changed", with(&|d| d.set(b"Name", Obj::Name(b"Im2".to_vec())))),
            ("an entry gone", with(&|d| drop(d.remove(b"Name")))),
            ("an entry added", with(&|d| d.set(b"Decode", Obj::Array(vec![Obj::Int(15), Obj::Int(0)])))),
            ("another width", with(&|d| d.set(b"Width", Obj::Int(8)))),
            ("another depth", with(&|d| d.set(b"BitsPerComponent", Obj::Int(8)))),
            ("another base", space(&|s| s[1] = Obj::Name(b"DeviceGray".to_vec()))),
            ("another family", space(&|s| s[0] = Obj::Name(b"Separation".to_vec()))),
            ("a longer table than hival says", space(&|s| s[2] = Obj::Int(3))),
            ("a fifth entry", space(&|s| s.push(Obj::Null))),
            ("not Indexed", with(&|d| d.set(b"ColorSpace", Obj::Name(b"DeviceRGB".to_vec())))),
        ] {
            assert!(!same_image(&a, &px, &wrong, &idx), "{what}");
        }
        // The image itself must be one that may be written so.
        let decoded = image("/ColorSpace /DeviceRGB /Name /Im1 /Decode [1 0 1 0 1 0]", 9, 9);
        let mut b2 = b.clone();
        b2.set(b"Decode", decoded.get(b"Decode").unwrap().clone());
        assert!(!same_image(&decoded, &px, &b2, &idx));
    }
}
