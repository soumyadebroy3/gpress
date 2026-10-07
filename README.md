# PDF engine

Our own PDF compression engine, written in Rust. Phase 1 of the PDF compressor ("prove it"): a command-line
tool, measured on public test PDFs against the best existing tools. Nothing here is part of the site yet.

## What is ours

| Part | File | What it does |
| --- | --- | --- |
| Flate decoder | `src/flate/inflate.rs`, `zlib.rs` | Accepts exactly what zlib accepts; anything damaged is an error, never a silent repair. |
| Flate encoder | `src/flate/deflate.rs`, `matcher.rs`, `parse.rs`, `block.rs`, `huffman.rs` | Binary-tree match finder (smallest distance for every length), shortest-path parsing priced first by symbol statistics and then by the exact code lengths, farther matches with cheaper distance codes, block boundaries by dynamic programming plus merge/split polishing, optimised block headers. |
| zlib rebuild | `src/flate/zlibexact.rs` | zlib's deflate re-created from its description, bit for bit, to rebuild streams zlib wrote. |
| PDF reader | `src/pdf/parse.rs`, `xref.rs`, `filters.rs` | Strict parser: objects, xref tables and streams, object streams, LZW, ASCIIHex, ASCII85, RunLength, predictors. |
| PDF writer | `src/pdf/rewrite.rs`, `images.rs` | Keeps every reachable object's value; re-encodes streams with our Flate; picks a better PNG predictor (or none) for images; gives JPEGs optimal Huffman tables; puts Flate in front of a JPEG only when that saves at least 1% (never in front of JPEG 2000 or JBIG2, whose filter chain keeps its shape for readers such as pdf.js); packs objects into compressed object streams grouped by type; writes a compressed xref stream; reads its own output back and compares before returning it (a changed JPEG must read back with the same coded symbols). |
| JPEG tables | `src/jpeg/` | Baseline and extended sequential JPEG (8-bit) written again with optimal Huffman tables, the same coefficients (like `jpegtran -optimize -copy all`): every scan is read to its coded symbols, which are written back with the best code of at most 16 bits that leaves the all-ones code word free (package-merge); of three orders of the symbols within a code length, the one with the fewest stuffed 0xFF bytes is kept. Every other byte stays (ICC and Adobe markers, comments, restart markers, bytes after the end); one table segment goes before each scan. The new file is read back and compared symbol by symbol before it is used. Progressive, arithmetic-coded, lossless, hierarchical, 12-bit, fractionally sampled (libjpeg can't decode those) and anything unusual or damaged is left as it is, and so are layouts readers could take differently once the tables move (fill bytes before a marker; every segment's contents must fill it exactly, as pdf.js reads tables by their contents), data bound to the file's bytes (multi-picture offsets, APP11 boxes such as content credentials), and files of more than 4096 segments. |
| Safety gate | `src/pdf/rewrite.rs` (`gate`) | Encrypted, signed, certified, PDF/A, PDF/X, files with earlier revisions, files we can't read strictly, objects beyond /Size, objects present but not indexed: returned untouched. |
| peyaji format | `src/pack/`, `src/cm/` | Any PDF, packed with our context-mixing model (bit-history states, mixers, SSE) with PDF-aware contexts: syntax, predicted object numbers and xref offsets, drawing instructions, image samples coded as residuals. Streams are recognised by their bytes (zlib, JPEG), not only by their dictionaries. Format version 4: a long repeat is coded as one 16-bit-precision decision instead of eight model decisions (huge flat images, duplicates); palette and packed-pixel images are modelled by their exact neighbours; an image's row length is confirmed from its data length; a JPEG behind Flate goes through Lepton when that beats the Flate bytes. Restored byte for byte; every pack is restored and checked (SHA-256) before it is returned. A damaged .pyj file is refused with an error: lengths are bounded, decoding stops when the data runs out, and the checksum is checked, so a wrong file is never returned. The model's settings are constants of the format version. |
| SHA-256 | `src/pack/sha256.rs` | For the restore check. |

## Existing libraries (optional, `--features existing`)

The engine keeps whichever result is smaller after the checks, so existing libraries can only help:

- [preflate-rs](https://crates.io/crates/preflate-rs) (Apache-2.0, Microsoft): rebuilds Flate streams made by encoders other than zlib (Adobe, cairo, Quartz) in the peyaji format.
- [lepton_jpeg](https://crates.io/crates/lepton_jpeg) (Apache-2.0, Microsoft): recompresses JPEG streams in the peyaji format and restores them bit for bit.

Apache-2.0 requires shipping its licence and notices with the product.

## Build and test

```
cargo test --release
cargo build --release --features existing
target/release/pdfe optimize in.pdf out.pdf        # normal PDF, maximum effort (--quick for fast)
target/release/pdfe pack in.pdf out.pyj            # peyaji format
target/release/pdfe unpack out.pyj back.pdf        # exact original
target/release/pdfe jpeg in.jpg out.jpg            # one JPEG with optimal Huffman tables (measuring)
```

## Benchmarks (`bench/`)

- `extract_streams.py`, `flate_bench.py`: every Flate stream of the test PDFs, our encoder against zlib-9,
  libdeflate-12 and Zopfli; every output checked with zlib.
- `pdf_bench.py`, `verify_pdf.py`: whole PDFs against qpdf; every output checked independently with qpdf
  --check, pikepdf (object graph followed by value, streams decoded; a JPEG whose bytes changed must have the
  same coefficients, `jpegtran -copy all` writing the same bytes for both, and decode to the same pixels with
  qpdf's libjpeg, Pillow and pdf.js's own JPEG decoder, `verify_jpeg_pdfjs.mjs`) and PDFium (pixels and text
  of every page, up to 30 pages). A changed JPEG needs `jpegtran` (PATH or `JPEGTRAN`) and pdf.js
  (`PDFJS_MODULES`); without them it is reported, never passed.
- `verify_render.py`, `verify_pdfjs.mjs`: two more renderers on every output, Poppler and pdf.js (pixels and
  text of every page, up to 30 pages; no new Poppler errors). pdf.js comes from npm (`pdfjs-dist`,
  `@napi-rs/canvas`), installed outside this folder; the engine itself has no dependencies.
- `check_the_checkers.py`: the checks must catch one small change (a character, a metadata value, the page
  size, one bit of one pixel, one JPEG coefficient, a coefficient one larger that libjpeg's pixels don't show,
  a JPEG layout only pdf.js reads differently) and pass an unchanged copy and a JPEG with other Huffman tables.
- `jpeg_bench.py`: every JPEG in the test PDFs, ours against `jpegtran -optimize` (libjpeg-turbo) and mozjpeg;
  every JPEG we change is checked with libjpeg-turbo (pixels, and coefficients through jpegtran) and Pillow.
  A tool's output counts even when jpegtran warns about the input (exit status 2); the report also gives the
  totals without those JPEGs.
- `pack_bench.py`: the peyaji format against xz, Brotli, Zstandard and inflate+xz; every file restored and
  compared byte for byte.

Results: `bench/RESULTS.md`.
