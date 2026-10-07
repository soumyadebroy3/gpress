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
| PDF writer | `src/pdf/rewrite.rs`, `images.rs` | Keeps every reachable object's value; re-encodes streams with our Flate; picks a better PNG predictor (or none) for images; packs objects into compressed object streams grouped by type; writes a compressed xref stream; reads its own output back and compares before returning it. |
| Safety gate | `src/pdf/rewrite.rs` (`gate`) | Encrypted, signed, certified, PDF/A, PDF/X, files with earlier revisions, files we can't read strictly, objects beyond /Size, objects present but not indexed: returned untouched. |
| peyaji format | `src/pack/`, `src/cm/` | Any PDF, packed with our context-mixing model (bit-history states, mixers, SSE) with PDF-aware contexts: syntax, predicted object numbers and xref offsets, drawing instructions, image samples coded as residuals. Restored byte for byte; every pack is restored and checked (SHA-256) before it is returned. |
| SHA-256 | `src/pack/sha256.rs` | For the restore check. |

## Existing libraries (optional, `--features existing`)

The engine keeps whichever result is smaller after the checks, so existing libraries can only help:

- [preflate-rs](https://crates.io/crates/preflate-rs) (Apache-2.0, Microsoft): rebuilds Flate streams made by encoders other than zlib (Adobe, cairo, Quartz) in the peyaji format.

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
- `pack_bench.py`: the peyaji format against xz, Brotli, Zstandard and inflate+xz; every file restored and
  compared byte for byte.

Results: `bench/RESULTS.md`.
