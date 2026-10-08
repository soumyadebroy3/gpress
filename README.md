# PDF engine

Our own PDF compression engine, written in Rust. Phase 1 of the PDF compressor ("prove it"): a command-line
tool, measured on public test PDFs against the best existing tools. Nothing here is part of the site yet.

## What is ours

| Part | File | What it does |
| --- | --- | --- |
| Flate decoder | `src/flate/inflate.rs`, `zlib.rs` | Accepts exactly what zlib accepts; anything damaged is an error, never a silent repair. |
| Flate encoder | `src/flate/deflate.rs`, `matcher.rs`, `parse.rs`, `block.rs`, `huffman.rs` | Binary-tree match finder (smallest distance for every length), shortest-path parsing priced first by symbol statistics and then by the exact code lengths, farther matches with cheaper distance codes, block boundaries by dynamic programming plus merge/split polishing, optimised block headers. The parse prices match lengths without branches, so it runs on vector instructions (AVX2 when the processor has it, chosen at run time; the same result either way). |
| zlib rebuild | `src/flate/zlibexact.rs` | zlib's deflate re-created from its description, bit for bit, to rebuild streams zlib wrote. |
| PDF reader | `src/pdf/parse.rs`, `xref.rs`, `filters.rs` | Strict parser: objects, xref tables and streams, object streams, LZW, ASCIIHex, ASCII85, RunLength, predictors. |
| PDF writer | `src/pdf/rewrite.rs`, `images.rs` | Keeps every reachable object's value; re-encodes streams with our Flate; picks a better PNG predictor (or none) for images, counting the bytes its parameters take; puts Flate in front of a JPEG only when that saves at least 1% (never in front of JPEG 2000 or JBIG2, whose filter chain keeps its shape for readers such as pdf.js); packs objects into compressed object streams grouped by type; writes a compressed xref stream; reads its own output back and compares before returning it. An image with fewer samples than its size needs is kept exactly as it is (readers differ on such images). |
| Safety gate | `src/pdf/rewrite.rs` (`gate`) | Encrypted, signed, certified, PDF/A, PDF/X, files with earlier revisions, files we can't read strictly, objects beyond /Size, objects present but not indexed: returned untouched. |
| peyaji format | `src/pack/`, `src/cm/` | Any PDF, packed with our context-mixing model (bit-history states, mixers, SSE) with PDF-aware contexts: syntax, predicted object numbers and xref offsets, drawing instructions, image samples coded as residuals. Streams are recognised by their bytes (zlib, JPEG), not only by their dictionaries. Format version 4: a long repeat is coded as one 16-bit-precision decision instead of eight model decisions (huge flat images, duplicates); palette and packed-pixel images are modelled by their exact neighbours; an image's row length is confirmed from its data length; a JPEG behind Flate goes through Lepton when that beats the Flate bytes. Restored byte for byte; every pack is restored and checked before it is returned (with a second thread, the restore decodes the bytes while the encoder writes them: the same code on the same bytes, in about half the time). A damaged .pyj file is refused with an error: lengths are bounded, decoding stops when the data runs out, and the checksum is checked, so a wrong file is never returned. The model's settings are constants of the format version. |
| SHA-256 | `src/pack/sha256.rs` | For the restore check. |
| Threads and effort | `src/par.rs`, `Options::effort` in `src/flate/deflate.rs` and `src/pdf/rewrite.rs`, `PackOptions` in `src/pack/mod.rs` | Standard-library threads only: the segments of a stream, the streams of a PDF and the streams `pack` looks at are worked on in parallel and put together in their original order, so the output is the same for any thread count (one thread uses no thread machinery at all, as in a browser). A thread the system refuses (thread or process limits) is no loss: the calling thread does the work, and `pack` checks the restore after encoding instead of alongside. Identical streams are encoded once (a remembered result is checked against its input before it is used); encoded outputs are remembered for one stream at a time, so memory follows the file's size. `--effort` 0 to 100: 100 is the strongest setting; below, Flate works in steps of 20 (measured: effort 0 is 3 to 4 times faster than 100 and about 0.5% larger on typical streams), the streams that take the most room get the strongest step that fits a work budget proportional to the file's size, and image predictors are ranked on sampled rows. The quick settings have a budget too (16 decoded bytes per byte of the file at effort 0, twice as much every 10 points, at least 4 MiB): a stream beyond it is kept as it is, so the time follows the file's size even when streams decode to a hundred times their size. In `pack`, already-compressed data (evenly spread bytes, and evenly spread differences between neighbouring bytes) is coded plainly below 50. Decided from sizes only, never from time. |

## Existing libraries (optional, `--features existing`)

The engine keeps whichever result is smaller after the checks, so existing libraries can only help:

- [preflate-rs](https://crates.io/crates/preflate-rs) (Apache-2.0, Microsoft): rebuilds Flate streams made by encoders other than zlib (Adobe, cairo, Quartz) in the peyaji format.
- [lepton_jpeg](https://crates.io/crates/lepton_jpeg) (Apache-2.0, Microsoft): recompresses JPEG streams in the peyaji format and restores them bit for bit.

Apache-2.0 requires shipping its licence and notices with the product.

## Build and test

```
cargo test --release
cargo build --release --features existing
target/release/pdfe optimize in.pdf out.pdf        # normal PDF, maximum effort (--effort 0..100, --quick = 0)
target/release/pdfe pack in.pdf out.pyj            # peyaji format (--effort 0..100)
target/release/pdfe unpack out.pyj back.pdf        # exact original
```

`--threads N` sets the number of threads (default: what the machine offers, at most 8); it never changes the
output. `PDFE_DEBUG=1` prints the time of every stream and Flate segment.

## Benchmarks (`bench/`)

- `extract_streams.py`, `flate_bench.py`: every Flate stream of the test PDFs, our encoder against zlib-9,
  libdeflate-12 and Zopfli; every output checked with zlib.
- `benchmark.py`: the standard measurement, in one command. `time` and `space` measure every file one
  process at a time (wall and CPU time, peak memory, bytes in and out, exactness): ours at each effort and
  thread count, normal PDF and archive, against qpdf (normal PDF) and xz -9e, Brotli 11, Zstandard 19 with
  long matching and expand + xz (archive), each as its own process. A normal-PDF output must open (page count
  and every page's content decoded with pikepdf), an untouched file must be the input byte for byte, and a
  sample of outputs goes through `verify_pdf.py`; every archive is restored with a separate `unpack` and
  compared byte for byte. Rows go to `results/<run>/*.jsonl` as each file finishes, so a stopped run
  continues where it was. `report` makes the tables (text, Markdown, and `time.tsv`, `memory.tsv`,
  `size.tsv`, `files.tsv`); `compare BASE NEW` fails when a setting got more than 10% slower in total, a file
  more than 2x slower, any output larger, or the peak memory more than 10% higher; `selftest` plants each of
  those and checks they are caught. Time is measured as both wall time (what a user waits) and the process's
  CPU time; the check uses CPU time (`--metric wall` to change), which moves far less than wall time when
  the machine is shared, though not nothing: a neighbour that fills the cache slows memory-bound code (the
  archive's model) in CPU time too. The report prints the machine's load next to every setting and `compare`
  says when the two runs saw a different load; numbers that are to be compared come from a quiet machine.
  The competitors need measuring only once: `--with RUN` lets `report` and `compare` take the settings a run
  lacks from a stored one (`results/all907-speed-ca8fa92` has every competitor on all 907 files), so
  measuring a new build is `time RUN --set all --who ours` and nothing more. `measure.c` is the small parent
  process that runs each command (Linux gives a process the peak memory of the one that forked it, so a
  Python parent would hide everything under 20 MB). File lists are in `sets/` (`all`, and `sample120`: the
  20 slowest, the 20 largest and 80 random files, seed 11), relative to the corpus root (`CORPUS`); the
  engine comes from `PDFE` (or `GPRESS`). This replaces the earlier `pdf_bench.py` and `pack_bench.py`; the
  "qpdf + xz" baseline they had is gone for good (qpdf drops objects and revisions, so it was never
  comparable).
- `verify_pdf.py`: the independent check of one output: qpdf --check, pikepdf (object graph followed by
  value, streams decoded) and PDFium (pixels and text of every page, up to 30 pages).
- `verify_render.py`, `verify_pdfjs.mjs`: two more renderers on every output, Poppler and pdf.js (pixels and
  text of every page, up to 30 pages; no new Poppler errors). pdf.js comes from npm (`pdfjs-dist`,
  `@napi-rs/canvas`), installed outside this folder; the engine itself has no dependencies.
- `check_the_checkers.py`: the checks must catch one small change (a character, a metadata value, the page
  size, one bit of one pixel) and pass an unchanged copy.
- `security.py`: part 3 of the same measurement, hostile input, pass or fail (`run` does everything;
  `benchmark.py security ...` reaches it too). Twenty packs damaged every way (bit flips, cuts, garbage,
  header fields, checksum) must be refused or restored exactly, never a wrong file; hostile PDFs (nesting
  100,000 deep, loops in the xref chain and the object streams, Flate, LZW and RunLength bombs, a
  100,000 x 100,000 image, a million objects, bad UTF-16) must come back untouched, smaller or with a clean
  error inside a time and memory bound that grows with the input; mutation fuzzing of optimize, pack and
  unpack for ten minutes each under a hard 512 MiB address space; `cargo audit`, the licence of every
  dependency and every `unsafe` block with its reason; the same inputs through the C library
  (`gpress_harness.c`) when `crates/gpress-c` is in the tree. `selfcheck` plants a wrong file, a crash, a
  hang, a memory blow-up, a bad licence and a bare `unsafe` and checks that each is caught. Every failing
  input is saved under `results/security/failures/` with the engine's stderr; `measure.c` runs each command
  from a parent under 1 MB so the peak memory is the command's own.

Results: `bench/RESULTS.md`.
