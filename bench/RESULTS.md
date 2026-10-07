# Phase 1 results

Test set: 907 public test PDFs, 130.47 MB (the pdf.js test suite, pypdf's sample files, and the OCRmyPDF and
pikepdf test resources). Many of them are deliberately broken or unusual. Measured on a 2-core cloud machine
in October 2026. Every number below comes from the scripts in this folder.

## 1. Flate, the compression inside normal PDFs

Every distinct Flate stream in the test files (7,280 streams, 197.6 MB uncompressed), each compressed on its
own. Every output was decoded with zlib and compared with the input.

| Encoder | Bytes | Compared with ours |
| --- | ---: | ---: |
| **Ours (maximum effort)** | **41,051,353** | |
| Zopfli (15 iterations), Google's maximum-compression Deflate encoder | 41,326,819 | 0.67% larger |
| libdeflate level 12 | 41,626,580 | 1.40% larger |
| zlib level 9 | 43,902,977 | 6.95% larger |
| As stored in the test files | 44,761,670 | 9.04% larger |

- Against Zopfli by stream type: fonts 0.81% smaller, images 0.56%, page content 0.45%, cross-reference
  data 2.46%.
- Stream by stream: smaller than Zopfli on 2,535, the same on 2,392, larger on 2,353. Taking the better of the
  two for every stream would give 41.04 MB, only about 15 KB less than ours alone.
- CPU time: 1,005 s for ours, 1,338 s for Zopfli.

## 2. Normal PDF (opens in every PDF reader)

`pdfe optimize` at maximum effort on all 907 files, compared with qpdf (object streams, Flate recompressed at
level 9).

| | Files | Before | After | Saved |
| --- | ---: | ---: | ---: | ---: |
| **Ours, files we made smaller** | 697 | 85.67 MB | **71.93 MB** | **16.0%** |
| qpdf on the same 697 files | 697 | 85.67 MB | 77.70 MB | 9.3% |
| Ours, all 907 files (untouched ones count as they are) | 907 | 130.47 MB | 116.73 MB | 10.5% |

Time for all 907 files: 2,525 s. Half the files take about 0.05 s or less, 90% under 2.2 s; the slowest file
(5.3 MB) took 545 s.

JPEG and JPEG 2000 images can't be made smaller in a normal PDF without changing them. When it saves at least
1%, the engine puts a Flate layer in front of a JPEG (a filter chain the PDF standard allows; PDFium, Poppler
and pdf.js all render these identically). JPEG 2000 and JBIG2 images are never given one: pdf.js reads a JPEG
2000 image's colour and depth from the image data only when JPXDecode is the first filter, and the render
check below caught 7 files that looked different in pdf.js before this rule (it costs 36 KB on the test set).

### Checked by tools we didn't write

Every one of the 697 outputs, compared with its original:

| Check | What it compares | Result |
| --- | --- | --- |
| qpdf --check | structure; the output must check at least as clean as the input | 690 clean (91 of them had warnings before), 6 keep the original's warnings, 1 keeps the original's errors |
| pikepdf (qpdf's object model) | every object reachable from the trailer, by value; every stream decoded (Flate, LZW, ASCII85, ASCIIHex, RunLength, predictors undone) | 697 identical (27,909 objects) |
| PDFium (Chrome's PDF engine) | pixels and text of every page, up to 30 pages per file | 697 identical (950 pages) |
| Poppler (many Linux viewers) | pixels and text of every page, up to 30 pages; no new errors | 696 identical (948 pages; on 3 of them Poppler fails the same way for original and output). The other one: Poppler draws the original itself differently on every run |
| pdf.js (Firefox's PDF viewer) | pixels and text of every page, up to 30 pages | 697 identical (950 pages) |

The checks themselves are tested (`check_the_checkers.py`, run on tracemonkey.pdf and pikepdf's rle.pdf): an
unchanged copy passes all five; one changed character of the page content, one changed value in the document
information, a page 1 point wider, and one bit of one pixel of a RunLength image are each caught (the
document information only by the object check, since it doesn't change the picture).

Two checker problems were found and fixed in the final run. Four files were first flagged: three because
qpdf decodes RunLength only at its "specialized" level and the checker had asked for "generalized" (so it
could not read the original images at all), and one because rendering a 175 × 233 inch page ran the checker
out of memory (giant pages are now rendered at a capped size, the same for both files). With the checker
fixed, all four are identical.

### Left untouched (210 files)

The safety gate returns these files exactly as they were:

| Reason | Files |
| --- | ---: |
| Object numbers beyond the file's declared /Size (qpdf warns on all 61; readers disagree on such files) | 61 |
| Not readable by our strict parser (damaged objects, cross-reference errors) | 52 |
| Earlier revisions kept in the file (incremental updates) | 51 |
| PDF/A (archival; any change needs a new conformance check) | 22 |
| Encrypted | 9 |
| PDF/X (print production) | 8 |
| No saving possible | 4 |
| Usage rights or certification | 2 |
| Digitally signed | 1 |

## 3. peyaji format (exact restore with our decoder)

`pdfe pack` on all 907 files, built with the existing libraries (`--features existing`). Every pack is
restored and compared inside the engine before it is returned, again by the bench script with a separate
`pdfe unpack`, and a third time with the final decoder: 907 of 907 restored byte for byte.

| | Size, as % of the original 130.47 MB |
| --- | ---: |
| **Ours (peyaji format)** | **56.5%** |
| xz -9e | 76.8% |
| Brotli 11 | 76.5% |
| Zstandard 19, long matching | 77.0% |
| Best of those three, file by file | 76.3% |
| Expand + xz: the same preparation as ours (streams opened and rebuilt exactly, JPEGs through Lepton), then xz -9e instead of our model, plus what a restore needs | 60.7% |

- Ours is the smallest of all of these on 858 of the 907 files.
- Most of the gain over general compressors comes from opening the compressed streams and rebuilding them
  exactly. On that same opened data our model is 6.9% smaller than xz (smaller on 877 files).
- Where xz still wins (30 files, 433 KB in all): huge, very repetitive images (a 243 MB blank image, a 142 MB
  set of scans), one CMYK photo, and JPEG data that Lepton has already compressed.
- An earlier "qpdf + xz" comparison (qpdf writes every stream decompressed, then xz) came out at 55.2%, but it
  isn't comparable: qpdf drops objects nothing refers to, earlier revisions, and the parts of damaged files it
  can't read (one 8.1 MB test file came out as 38 KB). It is left out.
- Time: 2,421 s for all 907 files, packing plus the built-in restore check.
- Streams are now recognised by their bytes, not only by their dictionaries: a 421 KB test file whose font
  stream names its filter by reference went from 414 KB to 333 KB.

Damaged .pyj files: 90 damaged versions of a pack (bit flips, cuts, garbage after the header) are all refused
with an error, never a wrong file and never a crash; the slowest refusal took 7.4 s. `tests/pack_format.rs`
checks this on every build.

## 4. Rebuilding the original Flate bytes

To restore a PDF exactly, a stream we decompress must be compressed again to the very same bytes. Our
re-creation of zlib's deflate, bit for bit, was tried on 3,392 Flate streams sampled from the test files:

| | Streams rebuilt | Share of the bytes |
| --- | ---: | ---: |
| Ours (zlib re-created, every setting) | 2,309 | 68.5% |
| Stock zlib, trying its usual settings | 2,285 | 61.5% |

Ours rebuilt every stream stock zlib did, plus 24 more. The rest come from other encoders (cairo 1.18, Adobe
Distiller and PDF Library, older Apple Quartz); for those the peyaji format uses preflate-rs when it is built in.

## 5. JPEG in the peyaji format

With lepton_jpeg built in, JPEG streams are recompressed and restored bit for bit:

| File | Original | peyaji | xz -9e |
| --- | ---: | ---: | ---: |
| stamps.pdf | 107,372 | 81,822 | 100,404 |
| bug854315.pdf | 107,532 | 34,757 | 41,160 |
| issue1350.pdf | 118,396 | 52,420 | 65,976 |
| issue12798_page1_reduced.pdf | 128,707 | 21,808 | 29,816 |

## Known limits

- Normal PDF: JPEG images only get the optional Flate layer; JPEG 2000 and JBIG2 images are not improved.
- Speed: maximum effort is slow on large image-heavy files (545 s for the slowest test file); the quick
  setting is faster but still too slow for some of them. A time budget is needed before this runs in a browser.
- peyaji format: very repetitive huge images and data that is already compressed (Lepton output, JPEG 2000)
  are where xz still does better; the model needs a long-repeat path and a "store as is" path for those.
- Encrypted PDFs are left as they are in both modes (they would need decrypting and re-encrypting exactly).
- Untouched files: see the table above; they are returned unchanged on purpose.
