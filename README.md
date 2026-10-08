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
| PDF writer | `src/pdf/rewrite.rs`, `images.rs` | Keeps every reachable object's value; re-encodes streams with our Flate; picks a better PNG predictor (or none) for images, counting the bytes its parameters take; gives JPEGs optimal Huffman tables; puts Flate in front of a JPEG only when that saves at least 1% (never in front of JPEG 2000 or JBIG2, whose filter chain keeps its shape for readers such as pdf.js); packs objects into compressed object streams grouped by type; writes a compressed xref stream; reads its own output back and compares before returning it (a changed JPEG must read back with the same coded symbols). An image with fewer samples than its size needs is kept exactly as it is (readers differ on such images). |
| Safety gate | `src/pdf/rewrite.rs` (`gate`) | Encrypted files the empty password doesn't open, signed, certified, PDF/A, PDF/X, files with earlier revisions, files we can't read strictly (including a literal string with a raw carriage return, which qpdf reads as a line feed and pdf.js, PDFium and Poppler keep), objects beyond /Size, objects present but not indexed, object numbers far apart (more unused numbers below the highest one than objects plus 4096: the cross-reference rows would be out of proportion to the file): returned untouched. Nothing a file claims (an object stream's /N, the trailer's /Size, an image's /Width, predictor parameters) sizes memory or work on its own; every count is checked against the data it describes first. |
| JPEG tables | `src/jpeg/` | Baseline and extended sequential JPEG (8-bit) written again with optimal Huffman tables, the same coefficients (like `jpegtran -optimize -copy all`): every scan is read to its coded symbols, which are written back with the best code of at most 16 bits that leaves the all-ones code word free (package-merge); of three orders of the symbols within a code length, the one with the fewest stuffed 0xFF bytes is kept. Every other byte stays (ICC and Adobe markers, comments, restart markers, bytes after the end); one table segment goes before each scan. The new file is read back and compared symbol by symbol before it is used. Progressive, arithmetic-coded, lossless, hierarchical, 12-bit, fractionally sampled (libjpeg can't decode those) and anything unusual or damaged is left as it is, and so are layouts readers could take differently once the tables move (fill bytes before a marker; every segment's contents must fill it exactly, as pdf.js reads tables by their contents), data bound to the file's bytes (multi-picture offsets, APP11 boxes such as content credentials), and files of more than 4096 segments. |
| CCITT fax codec | `src/fax/ccitt.rs`, `tables.rs` | Decoder and encoder for T.4 (1-D, 2-D) and T.6 as CCITTFaxDecode uses them. The decoder records every choice of the original encoder (fill bits, EOLs, tag bits, end codes, padding, bytes after the end, and Adobe's way of choosing between pass and horizontal mode) and keeps a row only if writing it again gives the same bits. Checked against libtiff both ways and against libtiff and Poppler on all 183 fax streams of the test files. |
| JBIG2 | `src/fax/jbig2.rs` | MQ coder; generic-region encoder and decoder (T.88, template 0, nominal adaptive pixels, typical prediction); the embedded stream PDF reads. jbig2dec decodes our output exactly. |
| Fax images, normal PDF | `src/fax/pdf.rs` | A CCITT image becomes JBIG2 when that is at least 5% smaller and nothing is uncertain: one CCITT filter with direct parameters, a DeviceGray image or a mask, the width and height the data holds, at most 2^30 pixels (width rounded up to 32; PDFium's JBIG2 decoder, which pdf.js also uses, shows nothing above 2^31), no byte alignment, EOLs where every reader expects them (no EndOfLine on Group 4 data; an EOL before every row once EndOfLine is set or the data starts with one, as Poppler and xpdf then look for them), every row written back bit for bit, end codes or 0 bits at the end, coding work of at most 2048 pixels per byte of data, and our JBIG2 decoder giving the same pixels. JBIG2Decode's output is the same bits as CCITTFaxDecode's (BlackIs1 handled), so Decode, ImageMask and masks keep their meaning. The self-check compares the two by decoded pixels. |
| Encrypted PDFs | `src/pdf/crypt.rs`, `src/pack/sealed.rs` | The standard security handler with the empty user password (see below). RC4 and the key derivations are ours; AES, MD5 and SHA-2 come from RustCrypto. |
| peyaji format | `src/pack/`, `src/cm/` | Any PDF, packed with our context-mixing model (bit-history states, mixers, SSE) with PDF-aware contexts: syntax, predicted object numbers and xref offsets, drawing instructions, image samples coded as residuals. Streams are recognised by their bytes (zlib, JPEG), not only by their dictionaries. Format version 6 (the first with Type 1 fonts, fax images and decrypted streams together; nothing earlier shipped, so earlier versions are refused): a long repeat is coded as one 16-bit-precision decision instead of eight model decisions (huge flat images, duplicates); palette and packed-pixel images are modelled by their exact neighbours; an image's row length is confirmed from its data length; a JPEG behind Flate goes through Lepton when that beats the Flate bytes. Restored byte for byte; every pack is restored and checked before it is returned (with a second thread, the restore decodes the bytes while the encoder writes them: the same code on the same bytes, in about half the time). A damaged .pyj file is refused with an error, never restored wrong: what a part restores is bounded by what is left of the declared length, and decoded data, corrections and Lepton data by 1032 bytes per byte of the file (what Flate can expand to), so the decoder's work and memory follow the length the header claims (`UnpackOptions::max_len` refuses a file declaring more than the caller can afford); decoding stops when the data runs out; a preflate part carries a checksum that stops random damage before the existing library sees it (a crafted file can carry a matching checksum, and preflate-rs has no bound of its own, so a build with the existing libraries is still open there); and the file's checksum is checked at the end. The model's settings, the part kinds and how each is coded are constants of the format version. |
| Type 1 fonts | `src/pack/type1.rs`, `src/cm/model.rs` | Type 1 font programs (FontFile with /Length1 /Length2 /Length3, typical of TeX) lose both layers of encryption before coding: the eexec layer (binary, PFB, and hex when its lines are regular) and the charstring layer inside it (lenIV honoured). Both ciphers are reversible byte for byte and are put back on restore; a font that would not come back exactly is coded as it is. On restore, a layout that doesn't fit inside the font's length is refused before anything is decoded for it (all its lengths are worked out with checked arithmetic). The charstrings get their own model kind, which follows their tokens (numbers, commands, operands per command). |
| Fax images, peyaji format | `src/fax/bilevel.rs`, `part.rs` | A CCITT stream is stored as its image, coded by a bilevel context-mixing model (templates up to four rows high, vertical runs, two mixers, two probability maps, 16-bit precision, typical prediction, a fast path for white areas), plus the encoder's choices; on restore our CCITT encoder writes the original bytes back. EncodedByteAlign data whose EOLs end on a byte boundary is read too. Streams under 64 bytes stay plain. The model's work is bounded by the file's size: up to any point of the file, the fax streams may take 2^24 plus 512 pixels per byte up to there. Streams past it are coded plainly, and a restore that would go past it is refused, so a restore's time is bounded by what it has restored, whatever length a damaged file claims. |
| SHA-256 | `src/pack/sha256.rs` | For the restore check. |
| Threads and effort | `src/par.rs`, `Options::effort` in `src/flate/deflate.rs` and `src/pdf/rewrite.rs`, `PackOptions` in `src/pack/mod.rs` | Standard-library threads only: the segments of a stream, the streams of a PDF and the streams `pack` looks at are worked on in parallel and put together in their original order, so the output is the same for any thread count (one thread uses no thread machinery at all, as in a browser). A thread the system refuses (thread or process limits) is no loss: the calling thread does the work, and `pack` checks the restore after encoding instead of alongside. Identical streams are encoded once (a remembered result is checked against its input before it is used); encoded outputs are remembered for one stream at a time, so memory follows the file's size. `--effort` 0 to 100: 100 is the strongest setting; below, Flate works in steps of 20 (measured: effort 0 is 3 to 4 times faster than 100 and about 0.5% larger on typical streams), the streams that take the most room get the strongest step that fits a work budget proportional to the file's size, and image predictors are ranked on sampled rows. The quick settings have a budget too (16 decoded bytes per byte of the file at effort 0, twice as much every 10 points, at least 4 MiB): a stream beyond it is kept as it is, so the time follows the file's size even when streams decode to a hundred times their size. Effort 0 is the fastest setting, not the smallest: on a file whose images decode to many times its size it can be larger than effort 20 or 30, where those streams are re-encoded again (measured: 2400dpi 164,073 bytes at 0 to 20 and 132,565 from 30; issue12963 517,015 at 0 to 10 and 450,534 from 20). The JPEG tables and the JBIG2 conversion run at every effort (about 0.07 s per MB of JPEG); only the second Flate pass over a JPEG's original bytes (the poor-tables rule) waits for effort 100. In `pack`, already-compressed data (evenly spread bytes, and evenly spread differences between neighbouring bytes) is coded plainly below 50. Decided from sizes only, never from time. |

## Encrypted PDFs

Both outputs handle files that open with the empty user password (anyone can read them) and use the standard
security handler:
/V 1, 2, 4, 5 and /R 2 to 6 (RC4, AES-128, AES-256), one crypt filter (/CFM /V2, /AESV2 or /AESV3, or /Identity)
named by both /StmF and /StrF, and /EncryptMetadata false. The key comes from the empty password only
(algorithms 2, 2.A and 2.B), and is confirmed by /U (algorithms 6 and 11) and, for AES-256, by /Perms (algorithm
13).

- Normal PDF: strings and streams are decrypted while reading, optimised as usual, and written with the same
  encryption: the same /Encrypt dictionary (and the objects it refers to), /ID, file key and permissions. Each
  string and stream is encrypted with its own object's key; object streams are encrypted as streams, the
  cross-reference stream is not; AES initialization vectors are derived from the file key and the object, so
  the output is the same on every run. Encrypted XMP metadata is compressed (no tool can read it from the file
  anyway); metadata left unencrypted stays as it is. The self-check reads the output back, checks that it opens
  with the same key and dictionary, and compares the decrypted objects.
- Permissions (/P): the file keeps exactly the same encryption, permission bits and content, so the engine
  proceeds whatever /P says.
- peyaji format: each encrypted stream that encrypts back to exactly its stored bytes is stored decrypted (so it
  gets the zlib rebuild, preflate, Lepton and the model) and encrypted again on restore. The pack carries the
  file key once and, per stream, its method, object number and generation (usually "those of the `N G obj` just
  before") and its AES initialization vector. When the pack isn't clearly smaller than the encrypted streams
  alone, the pack without decryption is made too and the smaller kept. Unencrypted files carry no crypt
  parts. The decoder refuses crypt parts out of place (a second key, settings not right
  after the syntax before a stream, an empty payload) and more than two parts in a row that restore nothing.
- Left untouched (normal PDF): files that need a password, other security handlers, signed files, streams
  with their own /Crypt filter, and anything readers may take differently: a /CF entry named /Identity (qpdf
  and pdf.js use it, PDFium and Poppler don't), /CFM /None or no /CFM (qpdf decrypts as AES, PDFium as RC4),
  /StmF and /StrF missing or naming different filters, a /V 4 or 5 key length given nowhere (Poppler takes 40
  bits), a /V 4 key that isn't 128 bits, /V 2 without /Length, /R 2 with more than 40 bits, /O or /U of unusual
  length, a /P outside 32 bits, and literal strings with a raw carriage return (decrypted differently by
  readers that keep it and readers that read a line feed). Everything else the gate refuses (PDF/A, earlier
  revisions, ...) still applies.
- peyaji format: files whose handler isn't opened as above are packed exactly as before. In the others, a
  stream with its own /Crypt filter, or one that doesn't encrypt back to its stored bytes, is kept as stored;
  strings are never changed, so signatures and raw carriage returns don't matter there.

## Existing libraries (optional, `--features existing`)

The engine keeps whichever result is smaller after the checks, so existing libraries can only help:

- [preflate-rs](https://crates.io/crates/preflate-rs) (Apache-2.0, Microsoft): rebuilds Flate streams made by encoders other than zlib (Adobe, cairo, Quartz) in the peyaji format.
- [lepton_jpeg](https://crates.io/crates/lepton_jpeg) (Apache-2.0, Microsoft): recompresses JPEG streams in the peyaji format and restores them bit for bit.

Apache-2.0 requires shipping its licence and notices with the product.

## Options

The public choices are plain fields, and nothing is read from the environment except `PDFE_DEBUG`, which only
prints timings. Three kinds of constants stay in the code: the ones that define the file formats (a decoder
must agree with its encoder on those: the model's settings, the part tags, the fax work budget); the ones
readers set (JBIG2 images of at most 2^30 pixels, no EOL rules readers disagree on); and measured thresholds
(JBIG2 only when 5% smaller, at most 2048 pixels of fax work per byte, at most 4096 JPEG segments, plain coding
below effort 50, fax streams of at least 64 bytes, Flate in front of a JPEG only when it saves 1%, the second
Flate pass only for JPEGs with poor tables, Lepton only when 10% smaller than the Flate bytes, the quick
budget's 16 bytes per byte and 4 MiB, 4096 spare object numbers).

- Normal PDF, `pdf::rewrite::Options`: `effort` 0 to 100 (`Options::effort(n)` sets the Flate settings to
  match; 0 is the fastest, 100 the smallest), `threads` (0: what the machine offers, at most 8; never changes
  the output), `max_stream` (largest decoded stream re-encoded, 64 MiB), `objstm_size` (objects per object
  stream, 200), `respect_conformance` (leave PDF/A, PDF/X, signed, certified and usage-rights files untouched;
  the safe default), `flate_over_jpeg` (also try our Flate in front of a JPEG), `jbig2` (CCITT fax images
  become JBIG2), `jpeg_tables` (JPEGs get optimal Huffman tables), `rewrite_encrypted` (files the empty
  password opens are rewritten with their encryption). The Flate settings themselves (`flate::Options`:
  iterations, exact rounds, chain length, segment size, splitting, alternatives, the multi-start limit, the
  nice length, polishing) are plain fields too.
- peyaji format, `pack::PackOptions`: `effort` (below 50, data that looks compressed already is coded plainly;
  Type 1 fonts are opened first and never coded plainly), `threads`, `decrypt` (store the streams of a file
  the empty password opens decrypted), `max_stream` (largest decoded stream stored decoded, 256 MiB),
  `existing` (use preflate and Lepton when the build includes them; the default is what the build has). The
  decoder needs none of these: every choice is stored in the file. The one exception is the build itself: a
  file with parts made by preflate or Lepton restores only with a build that has them (`unpack` says so).
- Restoring, `pack::UnpackOptions`: `max_len`, the largest file restored (16 GiB, the format's limit). The
  decoder's work and memory follow the length the header declares, which is the one bound a crafted file
  can't escape, so a service that takes files from anyone sets this to what it can afford; a file declaring
  more is refused before anything is decoded.

## Build and test

```
cargo test --release
cargo test --release --features existing
cargo clippy --all-targets -- -D warnings
cargo build --release --features existing
target/release/pdfe optimize in.pdf out.pdf        # normal PDF, maximum effort (--effort 0..100, --quick = 0; --no-jbig2,
                                                   # --no-jpeg-tables, --keep-encrypted, --ignore-conformance, --no-flate-over-jpeg)
target/release/pdfe pack in.pdf out.pyj            # peyaji format (--effort 0..100, --no-decrypt)
target/release/pdfe unpack out.pyj back.pdf        # exact original (--max-len N refuses a file declaring more)
target/release/pdfe jpeg in.jpg out.jpg            # one JPEG with optimal Huffman tables (measuring)
```

`--threads N` sets the number of threads (default: what the machine offers, at most 8); it never changes the
output. `PDFE_DEBUG=1` prints the time of every stream and Flate segment. `tests/interaction.rs` runs every
feature on one encrypted file (a JPEG, a fax image, a Type 1 font and a zlib stream) in both outputs.

## Benchmarks (`bench/`)

- `extract_streams.py`, `flate_bench.py`: every Flate stream of the test PDFs, our encoder against zlib-9,
  libdeflate-12 and Zopfli; every output checked with zlib.
- `pdf_bench.py`, `verify_pdf.py`: whole PDFs against qpdf; every output checked independently with qpdf
  --check, pikepdf (object graph followed by value, streams decoded; a JPEG whose bytes changed must have the
  same coefficients, `jpegtran -copy all` writing the same bytes for both, and decode to the same pixels with
  qpdf's libjpeg, Pillow and pdf.js's own JPEG decoder, `verify_jpeg_pdfjs.mjs`; the other image streams qpdf
  can't decode, CCITT, JBIG2 and JPEG 2000, are compared by their decoded pixels with pikepdf's decoders,
  libtiff, jbig2dec and OpenJPEG, then Poppler's when pikepdf can't decode one or disagrees, noted in the
  output: libtiff wants an EOL before every Group 3 row, which PDF doesn't require) and PDFium (pixels and
  text of every page, up to 30 pages). A changed JPEG needs `jpegtran` (PATH or `JPEGTRAN`) and pdf.js
  (`PDFJS_MODULES`); without them it is reported, never passed. jbig2dec and jbig2enc (`apt install jbig2dec
  jbig2`) are used only by the checks and benchmarks, never by the engine.
- `verify_render.py`, `verify_pdfjs.mjs`: two more renderers on every output, Poppler and pdf.js (pixels and
  text of every page, up to 30 pages; no new Poppler errors). pdf.js comes from npm (`pdfjs-dist`,
  `@napi-rs/canvas`), installed outside this folder.
- `verify_encryption.py`: an output keeps the original's encryption exactly (qpdf's own reading of the revision,
  permissions, methods and file key, the /ID and the /Encrypt dictionary, compared with pikepdf and qpdf).
- `check_the_checkers.py`: the checks must catch one small change (a character, a metadata value, the page
  size, one bit of one pixel, one JPEG coefficient, a coefficient one larger that libjpeg's pixels don't show,
  a JPEG layout only pdf.js reads differently) and pass an unchanged copy and a JPEG with other Huffman tables.
  With `--fax FILE`: a fax image coded again by libtiff (same pixels) must pass, one flipped pixel must be
  caught.
- `jpeg_bench.py`: every JPEG in the test PDFs, ours against `jpegtran -optimize` (libjpeg-turbo) and mozjpeg;
  every JPEG we change is checked with libjpeg-turbo (pixels, and coefficients through jpegtran) and Pillow.
  A tool's output counts even when jpegtran warns about the input (exit status 2); the report also gives the
  totals without those JPEGs.
- `fax_bench.py`: files with fax images, both outputs before and after, and every fax stream against xz -9e
  and jbig2enc's lossless generic coding.
- `pack_bench.py`: the peyaji format against xz, Brotli, Zstandard and inflate+xz; every file restored and
  compared byte for byte.
- `examples/type1_fonts.rs`: Type 1 font programs (one file each) opened as the peyaji format opens them,
  written out for other compressors, and coded by our model as they are and opened
  (`cargo run --release --example type1_fonts -- INDEX OUTDIR`).

Results: `bench/RESULTS.md`.
