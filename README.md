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
| PDF writer | `src/pdf/rewrite.rs`, `images.rs` | Keeps every reachable object's value; re-encodes streams with our Flate; picks a better PNG predictor (or none) for images; puts Flate in front of a JPEG only when that saves at least 1% (never in front of JPEG 2000 or JBIG2, whose filter chain keeps its shape for readers such as pdf.js); packs objects into compressed object streams grouped by type; writes a compressed xref stream; reads its own output back and compares before returning it. |
| Safety gate | `src/pdf/rewrite.rs` (`gate`) | Encrypted, signed, certified, PDF/A, PDF/X, files with earlier revisions, files we can't read strictly, objects beyond /Size, objects present but not indexed: returned untouched. |
| peyaji format | `src/pack/`, `src/cm/` | Any PDF, packed with our context-mixing model (bit-history states, mixers, SSE) with PDF-aware contexts: syntax, predicted object numbers and xref offsets, drawing instructions, image samples coded as residuals. Streams are recognised by their bytes (zlib, JPEG), not only by their dictionaries. Format version 4: a long repeat is coded as one 16-bit-precision decision instead of eight model decisions (huge flat images, duplicates); palette and packed-pixel images are modelled by their exact neighbours; an image's row length is confirmed from its data length; a JPEG behind Flate goes through Lepton when that beats the Flate bytes. Restored byte for byte; every pack is restored and checked (SHA-256) before it is returned. A damaged .pyj file is refused with an error: lengths are bounded, decoding stops when the data runs out, and the checksum is checked, so a wrong file is never returned. The model's settings are constants of the format version. |
| SHA-256 | `src/pack/sha256.rs` | For the restore check. |
| CCITT fax codec | `src/fax/ccitt.rs`, `tables.rs` | Decoder and encoder for T.4 (1-D, 2-D) and T.6 as CCITTFaxDecode uses them. The decoder records every choice of the original encoder (fill bits, EOLs, tag bits, end codes, padding, bytes after the end, and Adobe's way of choosing between pass and horizontal mode) and keeps a row only if writing it again gives the same bits. Checked against libtiff both ways and against libtiff and Poppler on all 183 fax streams of the test files. |
| JBIG2 | `src/fax/jbig2.rs` | MQ coder; generic-region encoder and decoder (T.88, template 0, nominal adaptive pixels, typical prediction); the embedded stream PDF reads. jbig2dec decodes our output exactly. |
| Fax images, normal PDF | `src/fax/pdf.rs` | A CCITT image becomes JBIG2 when that is at least 5% smaller and nothing is uncertain: one CCITT filter with direct parameters, a DeviceGray image or a mask, the width and height the data holds, at most 2^30 pixels (width rounded up to 32; PDFium's JBIG2 decoder, which pdf.js also uses, shows nothing above 2^31), no byte alignment, EOLs where every reader expects them (no EndOfLine on Group 4 data; an EOL before every row once EndOfLine is set or the data starts with one, as Poppler and xpdf then look for them), every row written back bit for bit, end codes or 0 bits at the end, coding work of at most 2048 pixels per byte of data, and our JBIG2 decoder giving the same pixels. JBIG2Decode's output is the same bits as CCITTFaxDecode's (BlackIs1 handled), so Decode, ImageMask and masks keep their meaning. The self-check compares the two by decoded pixels. |
| Fax images, peyaji format | `src/fax/bilevel.rs`, `part.rs` | Format version 5: a CCITT stream is stored as its image, coded by a bilevel context-mixing model (templates up to four rows high, vertical runs, two mixers, two probability maps, 16-bit precision, typical prediction, a fast path for white areas), plus the encoder's choices; on restore our CCITT encoder writes the original bytes back. EncodedByteAlign data whose EOLs end on a byte boundary is read too. Streams under 64 bytes stay plain. The model's work is bounded by the file's size: up to any point of the file, the fax streams may take 2^24 plus 512 pixels per byte up to there. Streams past it are coded plainly, and a restore that would go past it is refused, so a restore's time is bounded by what it has restored, whatever length a damaged file claims. |

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
```

## Benchmarks (`bench/`)

- `extract_streams.py`, `flate_bench.py`: every Flate stream of the test PDFs, our encoder against zlib-9,
  libdeflate-12 and Zopfli; every output checked with zlib.
- `pdf_bench.py`, `verify_pdf.py`: whole PDFs against qpdf; every output checked independently with qpdf
  --check, pikepdf (object graph followed by value, streams decoded) and PDFium (pixels and text of every
  page, up to 30 pages).
- `verify_render.py`, `verify_pdfjs.mjs`: two more renderers on every output, Poppler and pdf.js (pixels and
  text of every page, up to 30 pages; no new Poppler errors). pdf.js comes from npm (`pdfjs-dist`,
  `@napi-rs/canvas`), installed outside this folder; the engine itself has no dependencies.
- `check_the_checkers.py`: the checks must catch one small change (a character, a metadata value, the page
  size, one bit of one pixel) and pass an unchanged copy. With `--fax FILE`: a fax image coded again by
  libtiff (same pixels) must pass, one flipped pixel must be caught.
- `verify_pdf.py` compares image streams qpdf can't decode (CCITT, JBIG2, JPEG, JPEG 2000) by their decoded
  pixels: pikepdf's decoders (libtiff, jbig2dec, libjpeg, OpenJPEG), then Poppler's when pikepdf can't
  decode one or disagrees (noted in its output; libtiff wants an EOL before every Group 3 row, which PDF
  doesn't require). jbig2dec and jbig2enc (`apt install jbig2dec jbig2`) are used only by the checks and
  benchmarks, never by the engine.
- `fax_bench.py`: files with fax images, both outputs before and after, and every fax stream against xz -9e
  and jbig2enc's lossless generic coding.
- `pack_bench.py`: the peyaji format against xz, Brotli, Zstandard and inflate+xz; every file restored and
  compared byte for byte.

Results: `bench/RESULTS.md`.
