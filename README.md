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
| Safety gate | `src/pdf/rewrite.rs` (`gate`) | Encrypted files the empty password doesn't open, signed, certified, PDF/A, PDF/X, files with earlier revisions, files we can't read strictly (including a literal string with a raw carriage return, which qpdf reads as a line feed and pdf.js, PDFium and Poppler keep), objects beyond /Size, objects present but not indexed: returned untouched. |
| Encrypted PDFs | `src/pdf/crypt.rs`, `src/pack/sealed.rs` | The standard security handler with the empty user password (see below). RC4 and the key derivations are ours; AES, MD5 and SHA-2 come from RustCrypto. |
| peyaji format | `src/pack/`, `src/cm/` | Any PDF, packed with our context-mixing model (bit-history states, mixers, SSE) with PDF-aware contexts: syntax, predicted object numbers and xref offsets, drawing instructions, image samples coded as residuals. Streams are recognised by their bytes (zlib, JPEG), not only by their dictionaries. Format version 4: a long repeat is coded as one 16-bit-precision decision instead of eight model decisions (huge flat images, duplicates); palette and packed-pixel images are modelled by their exact neighbours; an image's row length is confirmed from its data length; a JPEG behind Flate goes through Lepton when that beats the Flate bytes. Restored byte for byte; every pack is restored and checked (SHA-256) before it is returned. A damaged .pyj file is refused with an error: lengths are bounded, decoding stops when the data runs out, and the checksum is checked, so a wrong file is never returned. The model's settings are constants of the format version. |
| SHA-256 | `src/pack/sha256.rs` | For the restore check. |

## Encrypted PDFs

Only files that open with the empty user password (anyone can read them) and use the standard security handler:
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
  alone, the pack without decryption is made too and the smaller kept. Unencrypted files encode exactly as
  before (format version 4). The decoder refuses crypt parts out of place (a second key, settings not right
  after the syntax before a stream, an empty payload) and more than two parts in a row that restore nothing.
- Left untouched (normal PDF) and packed as before (peyaji): files that need a password, other security
  handlers, signed files, streams with their own /Crypt filter, and anything readers may take differently:
  a /CF entry named /Identity (qpdf and pdf.js use it, PDFium and Poppler don't), /CFM /None or no /CFM (qpdf
  decrypts as AES, PDFium as RC4), /StmF and /StrF missing or naming different filters, a /V 4 or 5 key length
  given nowhere (Poppler takes 40 bits), a /V 4 key that isn't 128 bits, /V 2 without /Length, /R 2 with more
  than 40 bits, /O or /U of unusual length, a /P outside 32 bits, and literal strings with a raw carriage
  return (decrypted differently by readers that keep it and readers that read a line feed). Everything else the
  gate refuses (PDF/A, earlier revisions, ...) still applies.

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
  `@napi-rs/canvas`), installed outside this folder.
- `verify_encryption.py`: an output keeps the original's encryption exactly (qpdf's own reading of the revision,
  permissions, methods and file key, the /ID and the /Encrypt dictionary, compared with pikepdf and qpdf).
- `check_the_checkers.py`: the checks must catch one small change (a character, a metadata value, the page
  size, one bit of one pixel) and pass an unchanged copy.
- `pack_bench.py`: the peyaji format against xz, Brotli, Zstandard and inflate+xz; every file restored and
  compared byte for byte.

Results: `bench/RESULTS.md`.
