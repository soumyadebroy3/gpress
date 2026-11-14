//! The rewrite's rules (`RewriteRules`), driven by the table itself: a rule set to its default is a rule left
//! alone; every rule's lowest and highest value give a file that is no larger and holds the same content, the
//! same bytes on one thread and on four; and every rule that decides something a caller can see has a value
//! here that changes the output.

use gpress::pdf::object::Obj;
use gpress::pdf::rules::RewriteRules;
use gpress::pdf::xref;
use gpress::{optimize, Options, Outcome};

// The fax scan of the mixed test document (making it needs both features).
#[cfg(all(feature = "fax", feature = "encryption"))]
mod common;

#[cfg(feature = "jpeg")]
const JPEG: &[u8] = include_bytes!("data/jpeg/base444.jpg");

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut b = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    b.extend_from_slice(data);
    b.extend_from_slice(b"\nendstream");
    b
}

/// A classic PDF from the bodies of objects 1, 2, ... (without "obj"/"endobj").
fn pdf(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut d = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offs.push(d.len());
        d.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        d.extend_from_slice(body);
        d.extend_from_slice(b"\nendobj\n");
    }
    let x = d.len();
    d.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offs {
        d.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    d.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    d
}

/// zlib data from the fast encoder at `level`: at its weakest level every effort has something to gain.
fn flate(data: &[u8], level: u8) -> Vec<u8> {
    gpress::flate::deflate_zlib(data, &gpress::flate::Options { fast: level, ..gpress::flate::Options::quick() })
}

fn rng(seed: u32) -> impl FnMut() -> u32 {
    let mut s = seed;
    move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    }
}

/// `len` bytes of text: words drawn from `vocabulary` made-up ones, ten to a line.
fn words(len: usize, vocabulary: u32, seed: u32) -> Vec<u8> {
    let mut next = rng(seed);
    let mut out = Vec::with_capacity(len + 16);
    let mut n = 0;
    while out.len() < len {
        let mut letters = rng(next() % vocabulary + 1);
        for _ in 0..3 + letters() % 6 {
            out.push(b'a' + (letters() % 26) as u8);
        }
        n += 1;
        out.push(if n % 10 == 0 { b'\n' } else { b' ' });
    }
    out.truncate(len);
    out
}

const PAGES: [&[u8]; 2] = [b"<< /Type /Catalog /Pages 2 0 R >>", b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"];

/// Two pages of text, with strings and with small objects whose kinds are not in the order of their numbers.
fn text_doc() -> Vec<u8> {
    pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /Lang (en-GB) /PageLabels << /Nums [0 << /P (Page ) >>] >> >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> /ExtGState << /G1 8 0 R >> >> /Contents 4 0 R >>".to_vec(),
        stream("/Filter /FlateDecode", &flate(&words(24_000, 300, 1), 1)),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 7 0 R >>".to_vec(),
        stream("/Filter /FlateDecode", &flate(&words(24_000, 300, 2), 1)),
        b"<< /Type /ExtGState /LW 2 >>".to_vec(),
    ])
}

/// One page whose content is objects 4 and 5.
fn page_of(streams: [Vec<u8>; 2]) -> Vec<u8> {
    let [a, b] = streams;
    pdf(&[
        PAGES[0].to_vec(),
        PAGES[1].to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 5 0 R] >>".to_vec(),
        a,
        b,
    ])
}

/// What each of the two streams of `two_streams_doc` decodes to.
const HALF: usize = 40_000;

/// Two streams of text that decode to `HALF` bytes each; the second takes more room in the file.
fn two_streams_doc() -> Vec<u8> {
    page_of([
        stream("/Filter /FlateDecode", &flate(&words(HALF, 16, 3), 1)),
        stream("/Filter /FlateDecode", &flate(&words(HALF, 300, 4), 1)),
    ])
}

/// A stream of text stored as effort 0 encodes it, so encoding it again that way saves nothing.
fn settled_doc() -> Vec<u8> {
    let text = words(30_000, 300, 5);
    page_of([
        stream("/Filter /FlateDecode", &gpress::flate::deflate_zlib(&text, &gpress::flate::Options::effort(0))),
        stream("", b"q Q\n"),
    ])
}

/// One page that draws a grey image of 8 bits a sample (object 5).
fn image_doc(width: usize, px: &[u8]) -> Vec<u8> {
    let image = format!(
        "/Type /XObject /Subtype /Image /Width {width} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode",
        px.len() / width
    );
    pdf(&[
        PAGES[0].to_vec(),
        PAGES[1].to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 240] /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        stream("", b"q 600 0 0 240 0 0 cm /Im1 Do Q\n"),
        stream(&image, &flate(px, 1)),
    ])
}

/// A row of the images below, by the predictor that suits it.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    /// Goes up from sample to sample by one of two steps, which differ from the steps of the row above: Sub
    /// leaves two values, and the row above is no help.
    Sub,
    /// The row above plus 100 or 101: Up leaves two values, and Sub leaves noise.
    Up,
    Noise,
}

/// A grey image of 600 x 240 (144 KB, more than the predictor search samples below effort 40) of the rows that
/// `kind` names. Their residuals are too large for the least-sum choice to find the predictor that suits them.
fn rows(kind: impl Fn(usize) -> Row) -> Vec<u8> {
    const W: usize = 600;
    let mut next = rng(7);
    let mut px = vec![0u8; W * 240];
    for r in 0..240 {
        let step = 90 + (r % 4 * 9) as u8;
        for i in 0..W {
            let bit = (next() >> 7 & 1) as u8;
            px[r * W + i] = match kind(r) {
                Row::Sub if i == 0 => next() as u8,
                Row::Sub => px[r * W + i - 1].wrapping_add(step + bit),
                Row::Up => px[(r - 1) * W + i].wrapping_add(100 + bit),
                Row::Noise => next() as u8,
            };
        }
    }
    px
}

/// Rows for Sub at the top and at every 30th row, rows for Up elsewhere: over the whole image, and over bands
/// of rows spread across it, Up wins; over the first rows only, or over single rows at every 30th, Sub does.
fn mixed_rows() -> Vec<u8> {
    rows(|r| match r {
        _ if r < 72 || r % 30 == 0 => Row::Sub,
        _ if r == 72 || r % 30 == 1 => Row::Noise,
        _ => Row::Up,
    })
}

/// Of every 30 rows, 13 for Sub and 16 for Up: Up wins over the whole image and over the bands of 27 rows
/// that effort 20 samples, Sub over the bands of 13 that the quick settings sample.
fn banded_rows() -> Vec<u8> {
    rows(|r| match r % 30 {
        0..13 => Row::Sub,
        13 => Row::Noise,
        _ => Row::Up,
    })
}

/// 40 KB of four letters at random: every place has many earlier places that match it, and more rounds of
/// parsing find a cheaper choice among them, so the efforts from 40 on differ from the quick settings and from
/// each other.
fn four_letters_doc() -> Vec<u8> {
    let mut next = rng(13);
    let text: Vec<u8> = (0..40_000).map(|_| b'a' + (next() >> 7 & 3) as u8).collect();
    page_of([stream("/Filter /FlateDecode", &flate(&text, 1)), stream("", b"q Q\n")])
}

/// A grey image of 128 x 48 with text in the first samples of every row and, in the last `walk`, samples that
/// go up by 100 or 101: the text is smaller as it is, the rest is smaller with Sub, and the more there is of
/// the rest the closer the two come.
fn text_and_walk(walk: usize) -> Vec<u8> {
    const W: usize = 128;
    let text = words(W * 48, 40, 8);
    let mut next = rng(10);
    let mut px = vec![0u8; W * 48];
    for k in 0..px.len() {
        px[k] = if k % W < W - walk { text[k] } else { px[k - 1].wrapping_add(100 + (next() >> 7 & 1) as u8) };
    }
    px
}

/// A grey image of 200 x 100 where every sample is the PNG Average prediction plus 100 or 101: only the Average
/// predictor leaves two values, and again the least-sum choice does not find it.
fn averaged() -> Vec<u8> {
    const W: usize = 200;
    let mut next = rng(9);
    let mut px = vec![0u8; W * 100];
    for k in 0..px.len() {
        let left = if k % W == 0 { 0 } else { px[k - 1] as u16 };
        let up = if k < W { 0 } else { px[k - W] as u16 };
        px[k] = (((left + up) / 2) as u8).wrapping_add(100 + (next() >> 7 & 1) as u8);
    }
    px
}

/// One page that draws a JPEG (object 5), with `comment` bytes of one letter in a comment segment.
#[cfg(feature = "jpeg")]
fn jpeg_doc(comment: usize) -> Vec<u8> {
    let mut jpeg = JPEG[..2].to_vec();
    if comment > 0 {
        jpeg.extend_from_slice(&[0xFF, 0xFE]);
        jpeg.extend_from_slice(&(comment as u16 + 2).to_be_bytes());
        jpeg.extend(std::iter::repeat_n(b'A', comment));
    }
    jpeg.extend_from_slice(&JPEG[2..]);
    pdf(&[
        PAGES[0].to_vec(),
        PAGES[1].to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 75 53] /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        stream("", b"q 75 0 0 53 0 0 cm /Im1 Do Q\n"),
        stream("/Type /XObject /Subtype /Image /Width 75 /Height 53 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode", &jpeg),
    ])
}

/// The fax image of the mixed test document, as an image dictionary and its data.
#[cfg(all(feature = "fax", feature = "encryption"))]
fn fax() -> Vec<u8> {
    use common::{fax_data, scan, FAX_H, FAX_W};
    let dict = format!("/Type /XObject /Subtype /Image /Width {FAX_W} /Height {FAX_H} /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns {FAX_W} >>");
    stream(&dict, &fax_data(&scan(FAX_W, FAX_H)))
}

/// One page that draws a fax image (object 5).
#[cfg(all(feature = "fax", feature = "encryption"))]
fn fax_doc() -> Vec<u8> {
    pdf(&[
        PAGES[0].to_vec(),
        PAGES[1].to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 400] /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        stream("", b"q 600 0 0 400 0 0 cm /Im1 Do Q\n"),
        fax(),
    ])
}

/// A little of everything the rules reach: text, strings, small objects, a grey image of 8 bits a sample and
/// one of 4, and (where the build has them) a JPEG and a fax image.
fn small_doc() -> Vec<u8> {
    let mut next = rng(11);
    let smooth: Vec<u8> = (0..100 * 40).map(|k| (k % 100 + k / 100 * 3) as u8 + (next() % 3) as u8).collect();
    let nibbles: Vec<u8> = (0..50 * 40).map(|k| ((k % 50 / 4) as u8) << 4 | (next() % 3) as u8).collect();
    let image = |bits: u32| {
        format!("/Type /XObject /Subtype /Image /Width 100 /Height 40 /ColorSpace /DeviceGray /BitsPerComponent {bits} /Filter /FlateDecode")
    };
    #[allow(unused_mut)]
    let mut images = vec![stream(&image(8), &flate(&smooth, 1)), stream(&image(4), &flate(&nibbles, 1))];
    #[cfg(feature = "jpeg")]
    images.push(stream(
        "/Type /XObject /Subtype /Image /Width 75 /Height 53 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
        JPEG,
    ));
    #[cfg(all(feature = "fax", feature = "encryption"))]
    images.push(fax());
    let names: String = (0..images.len()).map(|k| format!("/Im{k} {} 0 R ", 7 + k)).collect();
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R /Lang (en-GB) /PageLabels << /Nums [0 << /P (\\(Page\\) ) >>] >> >>".to_vec(),
        PAGES[1].to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> /ExtGState << /G1 6 0 R >> /XObject << {names}>> >> /Contents 4 0 R >>").into_bytes(),
        stream("/Filter /FlateDecode", &flate(&words(3000, 50, 6), 1)),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /ExtGState /LW 2 >>".to_vec(),
    ];
    objs.extend(images);
    pdf(&objs)
}

fn with_rules(effort: u8, rules: &[(&str, u64)]) -> Options {
    let mut opt = Options::default().effort(effort).threads(1);
    for &(name, value) in rules {
        assert!(opt.rewrite_rules.set(name, value), "{name}={value} is not a rule and a value in its range");
    }
    opt
}

/// The rewrite of `input`, held to what must be true whatever the rules say: no error, and a file that is no
/// larger and holds the same content (the input itself when nothing was gained).
fn checked(input: &[u8], opt: &Options, what: &str) -> Vec<u8> {
    let r = optimize(input, opt).unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(r.data.len() <= input.len(), "{what}: {} bytes from {}", r.data.len(), input.len());
    match &r.outcome {
        Outcome::Untouched(_) => assert!(r.data == input, "{what}: untouched, but not the input"),
        Outcome::Smaller(_) => same_content(input, &r.data, what),
    }
    r.data
}

/// `checked`, and the same bytes on four threads.
fn rewritten(input: &[u8], opt: &Options, what: &str) -> Vec<u8> {
    let one = checked(input, opt, what);
    let four = optimize(input, &opt.clone().threads(4)).unwrap_or_else(|e| panic!("{what}, four threads: {e}"));
    assert!(four.data == one, "{what}: four threads give other bytes");
    one
}

/// Every object of `input` is in `output` under its number: the same value, and for a stream without an image
/// codec the same bytes once its filters and predictor are undone.
fn same_content(input: &[u8], output: &[u8], what: &str) {
    let (before, after) = (xref::open(input).unwrap(), xref::open(output).unwrap_or_else(|e| panic!("{what}: {e:?}")));
    let mut n = 1;
    while let Ok((_, old)) = before.load(n) {
        let new = after.load(n).unwrap_or_else(|e| panic!("{what}: object {n}: {e:?}")).1;
        match (old, new) {
            (Obj::Stream(a), Obj::Stream(b)) => {
                let decoded = |s: &gpress::pdf::object::Stream| xref::decode_fully(&s.dict, &s.data, 1 << 24);
                if let (Some(a), Some(b)) = (decoded(&a), decoded(&b)) {
                    assert!(a == b, "{what}: stream {n} decodes to other bytes");
                }
            }
            (old, new) => assert_eq!(old, new, "{what}: object {n}"),
        }
        n += 1;
    }
    assert!(n > 5, "{what}: only {} objects compared", n - 1);
}

#[test]
fn a_rule_set_to_its_default_is_a_rule_left_alone() {
    let mut opt = Options::default();
    for &(name, default, _, _) in RewriteRules::RULES {
        assert!(opt.rewrite_rules.set(name, default), "{name}");
        assert_eq!(opt.rewrite_rules.get(name), Some(default), "{name}");
    }
    assert_eq!(opt.rewrite_rules, Options::default().rewrite_rules);
    // And so is the output, where the fast path, the bridge, the budgets and the full search decide.
    let doc = small_doc();
    for effort in [0, 15, 40, 100] {
        let (set, unset) = (opt.clone().effort(effort).threads(1), Options::default().effort(effort).threads(1));
        assert!(optimize(&doc, &set).unwrap().data == optimize(&doc, &unset).unwrap().data, "effort {effort}");
    }
}

#[test]
fn the_lowest_and_the_highest_value_of_every_rule_run_clean() {
    let doc = small_doc();
    for effort in [0, 15, 40, 100] {
        let base = rewritten(&doc, &with_rules(effort, &[]), &format!("effort {effort}"));
        assert!(base.len() < doc.len(), "effort {effort}: the small document is not made smaller");
        for &(name, default, lowest, highest) in RewriteRules::RULES {
            for value in [lowest, highest] {
                if value != default {
                    rewritten(&doc, &with_rules(effort, &[(name, value)]), &format!("{name}={value}, effort {effort}"));
                }
            }
        }
    }
}

/// The documents of the tests below.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Doc {
    Text,
    TwoStreams,
    Settled,
    FourLetters,
    MixedRows,
    BandedRows,
    Averaged,
    Jpeg,
    JpegWithComment,
    Fax,
}

/// None: a document this build cannot make or has no use for (a part it leaves out).
fn build(doc: Doc) -> Option<Vec<u8>> {
    Some(match doc {
        Doc::Text => text_doc(),
        Doc::TwoStreams => two_streams_doc(),
        Doc::Settled => settled_doc(),
        Doc::FourLetters => four_letters_doc(),
        Doc::MixedRows => image_doc(600, &mixed_rows()),
        Doc::BandedRows => image_doc(600, &banded_rows()),
        Doc::Averaged => image_doc(200, &averaged()),
        #[cfg(feature = "jpeg")]
        Doc::Jpeg => jpeg_doc(0),
        #[cfg(feature = "jpeg")]
        Doc::JpegWithComment => jpeg_doc(4000),
        #[cfg(all(feature = "fax", feature = "encryption"))]
        Doc::Fax => fax_doc(),
        #[allow(unreachable_patterns)]
        _ => return None,
    })
}

/// A value of a rule that changes what a caller gets: at `effort`, `doc` comes out in other bytes than with
/// the rule at its default. `with`: other rules, set in both rewrites, that bring this rule's decision into
/// play.
struct Case {
    rule: String,
    value: u64,
    effort: u8,
    doc: Doc,
    with: &'static [(&'static str, u64)],
}

fn case(rule: &str, value: u64, effort: u8, doc: Doc, with: &'static [(&'static str, u64)]) -> Case {
    Case { rule: rule.to_string(), value, effort, doc, with }
}

/// Rules that decide how the work is done and never what comes out: how many predictor candidates are ranked
/// at the same time, and where a new stream is compared with the old one.
const NEVER_SEEN: [&str; 2] = ["rank_together", "job_check_ratio"];

/// Rules with a test of their own: two below, and the JPEG's second pass in src/pdf/rewrite.rs (it changes a
/// file only when a JPEG's original bytes suit Flate better than its new ones, which no small JPEG made here
/// does, so its decision is tested where it is made).
const TESTED_APART: [&str; 4] =
    ["xref_predictor", "predictor_rank_level", "jpeg_second_pass_flate_gain", "jpeg_second_pass_table_gain"];

#[test]
fn every_rule_a_caller_can_see_has_a_value_that_changes_the_output() {
    use Doc::*;
    const ANY: u64 = 1 << 40;
    // Off the bridge, where the plan gives a stream the fast encoder's level of the rule.
    const NO_BRIDGE: &[(&str, u64)] = &[("bridge_start", 0)];
    const ALL_BIG: &[(&str, u64)] = &[("bridge_start", 0), ("big_stream", 0)];
    let mut cases = vec![
        // What is kept.
        case("min_file_saving", ANY, 0, Text, &[]),
        case("min_stream_saving", ANY, 0, Text, &[]),
        case("flate_over_image_min_saving", 100, 40, JpegWithComment, &[]),
        case("jpeg_min_saving", ANY, 40, Jpeg, &[]),
        case("jpeg_code_orders", 1, 40, Jpeg, &[]),
        case("jbig2_min_saving", 100, 40, Fax, &[]),
        // The predictor of an image. Up is the best for the rows of `mixed_rows` taken together, which the
        // default sample shows and a single band or single rows do not; a choice by fewest bits finds the
        // best type for every row; only Average suits `averaged`, and only the fast path's estimate tries it.
        case("max_predictor_image", 0, 40, MixedRows, &[]),
        case("min_predictor_image", ANY, 40, MixedRows, &[]),
        case("predictor_candidates", 63, 15, Averaged, &[]),
        case("fast_predictor_candidates", 1, 0, MixedRows, &[]),
        case("fast_predictor_min_bits", 16, 0, MixedRows, &[]),
        case("row_predictor_rule", 1, 40, MixedRows, &[]),
        case("predictor_sample_bands", 1, 15, MixedRows, &[]),
        case("fast_predictor_sample", 0, 0, MixedRows, &[]),
        case("predictor_sample_0", 0, 15, MixedRows, &[]),
        case("predictor_sample_20", 0, 20, MixedRows, &[]),
        case("predictor_sample_40", 0, 40, MixedRows, &[]),
        case("predictor_sample_60", 0, 60, MixedRows, &[]),
        case("predictor_sample_80", 0, 80, MixedRows, &[]),
        case("trial_predictors_from", 0, 5, Averaged, &[]),
        // The layout of the output.
        case("object_streams", 0, 0, Text, &[]),
        case("object_stream_order", 1, 0, Text, &[]),
        case("string_form", 1, 0, Text, &[]),
        // The effort: the full search, the bridge from 10 to 20 and the fast path's levels.
        case("full_from", 0, 15, Text, &[]),
        case("bridge_from", 20, 15, Text, &[]),
        case("bridge_start", 0, 15, Text, &[]),
        case("bridge_growth", 100, 19, Text, &[]),
        case("bridge_test", 0, 19, Settled, &[]),
        case("big_stream", 0, 0, Text, &[]),
        // The budgets from 20 on. With no floor a budget of nothing keeps both streams as they are; a budget
        // of one and a half streams goes to the first in the order.
        case("quick_budget_factor", 0, 40, TwoStreams, &[("quick_budget_floor", 0)]),
        case("quick_budget_doubling", 100, 99, TwoStreams, &[("quick_budget_floor", 0), ("quick_budget_factor", 1)]),
        case("quick_budget_floor", 0, 40, TwoStreams, &[("quick_budget_factor", 0)]),
        case(
            "budget_order",
            1,
            40,
            TwoStreams,
            &[("quick_budget_factor", 0), ("quick_budget_floor", HALF as u64 * 3 / 2)],
        ),
        case("work_budget_start", 0, 60, FourLetters, &[]),
        case("work_budget_doubling", 100, 60, FourLetters, &[]),
        // Effort 20 and the quick settings give the same Flate data; what a stream loses with the quick
        // settings is the larger predictor sample.
        case("work_cost_20", 100_000, 20, BandedRows, &[]),
        case("work_cost_40", 100_000, 40, FourLetters, &[]),
        case("work_cost_60", 100_000, 60, FourLetters, &[]),
        case("work_cost_80", 100_000, 80, FourLetters, &[]),
        case("step_down", 0, 60, FourLetters, &[("work_cost_60", 100_000)]),
    ];
    for e in 0..20 {
        cases.push(case(&format!("image_level_{e}"), 1, e, MixedRows, NO_BRIDGE));
        cases.push(case(&format!("big_level_{e}"), 1, e, Text, ALL_BIG));
    }
    let mut docs: Vec<(Doc, Option<Vec<u8>>)> = Vec::new();
    for c in &cases {
        if !docs.iter().any(|d| d.0 == c.doc) {
            docs.push((c.doc, build(c.doc)));
        }
        let Some(doc) = docs.iter().find(|d| d.0 == c.doc).and_then(|d| d.1.as_ref()) else { continue };
        let what = format!("{}={} at effort {} on {:?}", c.rule, c.value, c.effort, c.doc);
        let mut opt = with_rules(c.effort, c.with);
        let base = checked(doc, &opt, &what);
        assert!(opt.rewrite_rules.set(&c.rule, c.value), "{what}: not a rule and a value in its range");
        assert!(checked(doc, &opt, &what) != base, "{what}: the output is the default's");
    }
    // The rules that only decide how the work is done.
    let image = build(MixedRows).unwrap();
    for effort in [15, 40] {
        let base = checked(&image, &with_rules(effort, &[]), "the default");
        for name in NEVER_SEEN {
            let &(_, _, lowest, highest) = RewriteRules::RULES.iter().find(|r| r.0 == name).unwrap();
            for value in [lowest, highest] {
                let what = format!("{name}={value} at effort {effort}");
                assert!(checked(&image, &with_rules(effort, &[(name, value)]), &what) == base, "{what}");
            }
        }
    }
    // No rule is left out.
    for &(name, ..) in RewriteRules::RULES {
        let seen = cases.iter().any(|c| c.rule == name) as usize;
        let listed = NEVER_SEEN.contains(&name) as usize + TESTED_APART.contains(&name) as usize;
        assert_eq!(seen + listed, 1, "{name}: a value that changes the output, or a line in one of the lists");
    }
}

#[test]
fn the_cross_reference_predictor_is_used_when_smaller_unless_a_rule_says() {
    let doc = text_doc();
    let out = |choice: u64| checked(&doc, &with_rules(0, &[("xref_predictor", choice)]), "xref_predictor");
    let (smaller, never, always) = (out(0), out(1), out(2));
    assert!(never != always);
    assert!(smaller == if never.len() <= always.len() { never } else { always });
}

#[test]
fn the_ranking_level_decides_between_predictors_that_are_close() {
    // Off the bridge, so the image keeps the fast encoder and its ranking level. As the part of each row that
    // Sub suits grows, Sub overtakes no predictor at all; where exactly depends on the level they are ranked
    // with, so at some width the weakest level picks the other one.
    let differs = (0..32).any(|walk| {
        let doc = image_doc(128, &text_and_walk(walk));
        let out = |level: u64| {
            let what = format!("predictor_rank_level={level}, {walk} samples");
            checked(&doc, &with_rules(15, &[("bridge_start", 0), ("predictor_rank_level", level)]), &what)
        };
        out(1) != out(7)
    });
    assert!(differs, "no width at which the ranking level changes the predictor");
}
