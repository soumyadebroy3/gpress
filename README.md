# gpress

Lossless PDF compression as a library, written in Rust, with C and WebAssembly bindings and a command-line
tool. Two outputs, both verified before they are returned:

- **Normal PDF** (`optimize`): a smaller PDF that any PDF reader opens, with every object's value preserved.
  On the public test set (907 files from the pdf.js, pypdf, OCRmyPDF and pikepdf test suites, 130 MB):
  16.0% smaller on the files it can safely touch, against 9.3% for qpdf on the same files.
- **gpress archive** (`compress` / `expand`, format version 8, `.gpz`): any file, a PDF above all, restored
  byte for byte. On the test set: 56.5% of the original size, against 76.3% for the best of xz, Brotli and
  Zstandard.

These figures were measured on the earlier builds that still had preflate-rs (bench/RESULTS.md sections 2-3
and 6-7, commits d85cd7a and ca8fa92). This build stores every Flate stream that zlib did not write as it is
(preflate-rs went with its LGPL dependency; see "Existing library" below), so archives of files with such
streams come out larger here (on ten files of the sample, 55.8% of the input became 75.1% at effort 0), and
the archive figure for the whole set will move when the set is measured again on this build; the normal-PDF
figure does not depend on preflate-rs.

Every number, the test set and how every output was checked by tools we didn't write (qpdf, pikepdf,
PDFium, Poppler, pdf.js, libjpeg, libtiff, jbig2dec) are in [bench/RESULTS.md](bench/RESULTS.md). The engine
is our own code; the only outside code in a default build is RustCrypto's AES, MD5 and SHA-2 for encrypted
PDFs. Private while in development; the repository, the crate, the C library and the npm package are all
`gpress`.

## Using it

### Rust

```toml
[dependencies]
gpress = { git = "https://github.com/soumyadebroy3/gpress" }
```

```rust
use gpress::{Options, Outcome};

let opt = Options::new().effort(80).threads(4);

// A smaller PDF that any reader opens.
let r = gpress::optimize(&pdf_bytes, &opt)?;
match r.outcome {
    Outcome::Smaller(report) => std::fs::write("out.pdf", &r.data)?,
    Outcome::Untouched(why) => println!("left as it was: {why}"),
}

// The gpress archive, and the original back from it.
let archive = gpress::compress(&pdf_bytes, &opt)?;
let original = gpress::expand(&archive.data, &opt)?;
assert_eq!(original, pdf_bytes);
```

Errors are a plain `Error` enum with a stable numeric code (1 input too large, 2 damaged archive,
3 unsupported archive version, 4 internal, 5 invalid options, 6 time budget, 7 declared length over the
limit, 8 needs the existing library, 9 declared decoded total over the limit) and a message without
internal paths. The library never panics across
its boundary: every call runs inside a panic guard, and a bug comes back as `Error::Internal`.
`Optimized.outcome` says what happened (`Smaller` with a `Report`: objects, streams re-encoded and kept,
object streams, JPEGs given new tables, fax images written as JBIG2, how the file is encrypted; or
`Untouched` with the reason), and `Compressed` carries the archive's statistics (streams rebuilt, through
Lepton, Type 1 fonts opened, fax images, streams stored decrypted, decoded bytes, or `stored` when nothing
could be made smaller).

### C (and everything that binds to C)

`crates/gpress-c` builds `libgpress_c` (shared and static) with the header
[`include/gpress.h`](crates/gpress-c/include/gpress.h):

```c
gpress_options opt; gpress_options_default(&opt); opt.effort = 70;
gpress_result r = gpress_optimize(pdf, pdf_len, &opt);
if (r.code == GPRESS_OK) fwrite(r.output.data, 1, r.output.len, out);
else fprintf(stderr, "%s\n", r.message);
gpress_result_free(&r);
```

Every function is thread-safe, never aborts the process, never reads outside the buffers it is given, and
every buffer it hands out is freed with `gpress_result_free` (or `gpress_free` / `gpress_free_message`, each
given the field's address), which are safe on null and on a result already freed: a free clears what it
freed, so a double free or a part freed before the whole result is harmless by construction. An empty output
is a null pointer with length 0. The options struct has the same fields as the Rust
`Options` (fill it with `gpress_options_default` first; a size left at 0 means its default).
[`examples/roundtrip.c`](crates/gpress-c/examples/roundtrip.c) is a complete program, and
`bench/gpress_harness.c` sends the security suite's hostile inputs through this API.

### JavaScript and TypeScript (browser and Node)

`crates/gpress-wasm` is a plain WebAssembly module (no bindgen layer) with a typed wrapper in
[`js/gpress.ts`](crates/gpress-wasm/js/gpress.ts):

```ts
import { Gpress } from "gpress";
const g = await Gpress.load(fetch("/gpress.wasm"));
const smaller = g.optimize(bytes, { effort: 70, jbig2: false });   // { data, outcome, reason? }
const archive = g.compress(bytes);
const original = g.expand(archive.data, { maxLenMiB: 512 });       // throws GpressError on a damaged archive
```

The module gives the same bytes as the native library, is single-threaded and has no clock (so no time
budget: run it in a Web Worker to keep a page responsive and to end it when it has taken too long). It is
built with `panic = "abort"` (the only choice on `wasm32-unknown-unknown`), so the panic guard that turns
a bug into `Error::Internal` natively catches nothing there: a bug, like a failed memory growth, traps the
module, and the wrapper turns the trap into a `GpressError` with code 4 and marks the instance dead (load
a fresh one). Sizes are 32-bit inside it: the wrapper refuses inputs of 2 GiB and more. Pages that load it
need `'wasm-unsafe-eval'` in their Content Security Policy's `script-src`.

### Command line

```
cargo build --release [--features existing]
target/release/gpress optimize in.pdf out.pdf [--effort N | --quick] [--threads N] [--time-budget MS] ...
target/release/gpress pack in.pdf out.gpz
target/release/gpress unpack out.gpz back.pdf [--max-len BYTES] [--max-decoded BYTES]
```

Every option of the library is a flag (`gpress` without arguments lists them); the tool also carries the
engine's measuring commands (`deflate`, `inflate`, `jpeg`, `fax-decode`, `expand`, ...) for the benchmarks.

## Options

Everything a caller can tune is a field of `Options` (the C struct and the TypeScript options mirror it),
with `Options::check` saying whether the engine accepts a set; nothing is read from the environment except
`PDFE_DEBUG`, which only prints timings and figures to standard error. For a normal PDF that includes one
summary line per file, which `bench/p5.py` reads:

    plan: S streams, J jobs (I identical to an earlier one, H sharing their input); store: budget B, peak P bytes, K kept, X hits, Y computed, about Z s saved; segments planned once: N

the streams, the jobs that encode them (a stream identical to an earlier one is no job of its own; H jobs
have the encoder input of another under another dictionary), what the store held at most and answered (with
the encoding time its answers saved, roughly), and the segments written from an earlier segment's plans.

| Option | Default | What it does |
| --- | --- | --- |
| `effort` | 100 | 0 (fastest) to 100 (smallest), both outputs. Normal PDF: 0 is the fast path, our fast Flate encoder (`src/flate/fast.rs`) with its near-optimal level for page content, fonts and the rest (one pass of it for a stream that decodes to more than 4 MiB) and a lazy level for images, image predictors chosen from an estimate instead of trial encodes, and only the cheap steps (object streams, the cross-reference stream, JPEG Huffman tables, JBIG2 for fax images); on the 120-file sample it takes 16.1 s of CPU against 25.3 s for qpdf's strongest lossless setting and 195.5 s for the strong encoder that effort 0 used before (one thread; files of 1 to 10 MB: median 0.20 s, p90 1.14 s, slowest 3.9 s). 1 to 19 bridge to the strong encoder: images get closer parses, from 10 the streams that take the most room get the strong encoder's quick settings within a budget that doubles every point (and so do the object streams and the cross-reference stream when they fit), except a Flate stream that effort 0's settings can't shrink, which is kept as it is. 20 to 100 are the strong encoder: the Flate settings go in steps of 20, the streams that take the most room get the strongest step that fits a work budget proportional to the file's size, and image predictors are ranked on sampled rows (all as before the fast path). Below 100 the decoded bytes have a budget too (16 per byte at effort 0, twice as much every 10 points, at least 4 MiB), so the time follows the file's size even when its streams decode to a hundred times their size. On the sample (CPU s, output as a share of the input on the 90 files it rewrites): 0: 16.1, 87.02%; 5: 23.0, 86.70%; 10: 40.7, 86.11%; 15: 111, 85.74%; 19: 183, 85.59%; 20: 244, 85.43%; 50: 334, 85.17%; 100: 983, 85.01% (qpdf's strongest lossless setting: 87.23% counting only the outputs bench/verify_pdf.py finds the same apart from the trailer's /ID). In the archive, data that looks compressed already is coded plainly below 50. Decided from sizes only, never from time. |
| `threads` | 0 | 0: every core the machine offers, at most 8 (each thread holds a stream of its own, so memory grows with them, until a budget for what they hold at once exists); at most 64. The output is identical for any count (the pieces of work are the same and are put together in order); a thread the system refuses is no loss. No more stream jobs or encoders run at once than there are threads (`SECURITY.md`, "Memory with threads"). |
| `store_budget` | 2 MiB | Normal PDF: bytes of encoded outputs one file keeps, so that an input another stream also encodes (the same data under another dictionary, say) is encoded once. The oldest go first, and each kept output counts 256 bytes besides its own; 0 keeps none, at most 1 GiB. A kept output is used only after it decodes to the same input. Never changes the output; nothing is kept from one call to the next (`SECURITY.md` says why). The `PDFE_DEBUG` summary gives its peak and hits. |
| `max_input` | 1 GiB | Larger inputs are refused at once. |
| `max_stream` | 64 MiB | Normal PDF: a stream that decodes to more is kept as it is. |
| `max_stream_archive` | 256 MiB | Archive: a stream that decodes to more is stored as it is. |
| `max_decoded` | 256 MiB | Archive, both ways. Compress: the most decoded bytes the parts of one archive may claim in all; in file order, a stream whose decoded form (plus any Lepton data for it) does not fit in what is left is stored as it is, decided from sizes alone. The archive's header declares the total. Expand: an archive declaring more than this is refused before anything is decoded (`Error::DecodedTooLarge`), and while decoding every part is held to what the header declared, so a damaged body can claim no more. The test set's largest file needs 232 MiB (`enormous.pdf`, 310 KB). |
| `max_len` | 1 GiB | Expand: an archive declaring more is refused before anything is decoded or reserved (`Error::DeclaredTooLarge`). The format's own limit is 16 GiB. |
| `objects_per_stream` | 200 | Normal PDF: most objects per compressed object stream. |
| `respect_conformance` | on | Normal PDF: leave PDF/A, PDF/X, signed, certified and usage-rights files untouched. Off, every object's value is still kept, but those guarantees are given up. |
| `flate_over_jpeg` | on | Normal PDF: also try our Flate in front of a JPEG (never in front of JPEG 2000 or JBIG2, whose filter chain keeps its shape for readers such as pdf.js). |
| `jpeg_tables` | on | Normal PDF: JPEGs get optimal Huffman tables (the same coefficients). |
| `jbig2` | on | Normal PDF: CCITT fax images become JBIG2 when that is at least 5% smaller (the same pixels). |
| `rewrite_encrypted` | on | Normal PDF: files the empty user password opens are rewritten with their encryption kept exactly. Off, every encrypted file is left untouched. |
| `decrypt` | on | Archive: the streams of such files are stored decrypted (so they get the zlib rebuild, Lepton and the model) and encrypted again on expand. |
| `use_existing_libraries` | on | Use Lepton for JPEG streams in the archive when the build has it (`--features existing`). An archive with parts Lepton made expands only with such a build (the error says so). |
| `time_budget` | none | Give up after this much wall time: `Error::TimeBudget` and no output, the input unchanged. Checked between streams, Flate segments and archive parts (every 64 KiB of model work) in all three operations, so it can be overshot by a little (on expand by one part's zlib re-creation). A call that finishes in time gives the same bytes as one without a budget. |

Memory while expanding follows these three: at most about `max_len` for the output, `max_len + max_decoded`
for the model's history (one byte per byte it codes, and while an image part is coded a ring of its residuals:
about a row and a pixel, never more than the part), and the model's tables (up to 164 MiB for files over
4 MiB). A decoded stream is not kept beside the history: its zlib stream is made again from there, with one
copy of the part only for PNG or TIFF predictor rows or a JPEG, and up to three while a Type 1 font is closed
again (each at most `max_decoded` and 2 GiB). Compress holds the input and its archive. Its encoder holds the
same history and tables and no copy of the file: a stream is inflated again as it is coded, a Deflate block at
a time (a Type 1 font whole; for PNG predictor rows also their row types, a byte a row, and about 64 KiB of
rows, or one row if longer). The planner keeps no decoded Flate data, only each stream's zlib settings and
Lepton data, a fax stream's image and an encrypted stream's decrypted bytes. Every pack ends with a restore
check, which is an expand as above: after the encoder with one thread, beside it with two (two histories and
two sets of tables at once). The security suite runs every pack and unpack with `max_decoded` at 64 MiB and
holds them to 512 MiB.

Three kinds of constants stay in the code: the ones that define the file formats (a decoder must agree with
its encoder on those: the model's settings, the part tags, the fax work budget); the ones readers set (JBIG2
images of at most 2^30 pixels, the EOL rules readers disagree on); and measured thresholds (JBIG2 only when
5% smaller, at most 2048 pixels of fax work per byte, at most 4096 JPEG segments, plain coding below effort
50, fax streams of at least 64 bytes, Flate in front of a JPEG only when it saves 1%, the second Flate pass
only for JPEGs with poor tables, Lepton only when 10% smaller than the Flate bytes, the quick budget's 16
bytes per byte and 4 MiB, 16 cross-reference rows per byte of the file plus 65536). The engine's own types
(`pdf::rewrite::Options`, `pack::PackOptions`, `pack::UnpackOptions`, `flate::Options` with its iterations,
exact rounds, chain length, segment size, splitting, alternatives, multi-start limit, nice length and
polishing) are public too, for measuring and for callers that want a knob the library's `Options` doesn't
carry.

## What is ours

| Part | File | What it does |
| --- | --- | --- |
| The public API | `src/api.rs`, `src/clock.rs` | `Options`, `optimize`, `compress`, `expand`, `Error` with stable codes, the panic guard, the deadline behind `time_budget`. |
| Flate decoder | `src/flate/inflate.rs`, `zlib.rs` | Accepts exactly what zlib accepts; anything damaged is an error, never a silent repair. |
| Flate encoder | `src/flate/deflate.rs`, `matcher.rs`, `parse.rs`, `block.rs`, `huffman.rs` | Binary-tree match finder (smallest distance for every length), shortest-path parsing priced first by symbol statistics and then by the exact code lengths, farther matches with cheaper distance codes, block boundaries by dynamic programming plus merge/split polishing, optimised block headers. The parse prices match lengths without branches, so it runs on vector instructions (AVX2 when the processor has it, chosen at run time; the same result either way). Smaller output than Zopfli, in less time. |
| Fast Flate encoder | `src/flate/fast.rs` | Effort 0 to 19. Twelve levels: hash chains (4-byte hashes, from level 3 also 3-byte ones) with greedy, one-step or two-step lazy matching for levels 1 to 7; binary trees and a cheapest-path parse under costs from a first pass (two passes from level 10) for levels 8 to 12. Blocks end where the symbol statistics change, counted as the symbols are made (no search); each block gets the shared Huffman code builder, a one-pass header and the cheapest of dynamic, fixed and stored. Inputs are cut at fixed 1 MiB offsets, each segment primed with the 32 KiB before it, worked on in parallel and joined bit for bit, so the output is the same for any thread count. On the sample's 3373 Flate streams (152.5 MB, one core), against zlib -9: level 1 +6.5% at 140 MB/s, level 7 -1.2% at 58 MB/s, level 12 -4.2% at 9 MB/s (0.9% above the strong encoder's quick settings, which run at 1.4 MB/s). |
| zlib rebuild | `src/flate/zlibexact.rs` | zlib's deflate re-created from its description, bit for bit, to rebuild streams zlib wrote. Streams other encoders wrote are stored as they are. |
| PDF reader | `src/pdf/parse.rs`, `xref.rs`, `filters.rs` | Strict parser: objects, xref tables and streams, object streams, LZW, ASCIIHex, ASCII85, RunLength, predictors. |
| PDF writer | `src/pdf/rewrite.rs`, `images.rs` | Keeps every reachable object's value; re-encodes streams with our Flate; picks a better PNG predictor (or none) for images, counting the bytes its parameters take; gives JPEGs optimal Huffman tables; puts Flate in front of a JPEG only when that saves at least 1% (never in front of JPEG 2000 or JBIG2); packs objects into compressed object streams grouped by type; writes a compressed xref stream; reads its own output back and compares before returning it (a changed JPEG must read back with the same coded symbols). An image with fewer samples than its size needs is kept exactly as it is (readers differ on such images). |
| Safety gate | `src/pdf/rewrite.rs` (`gate`) | Encrypted files the empty password doesn't open, signed, certified, PDF/A, PDF/X, files with earlier revisions, files we can't read strictly (including a literal string with a raw carriage return, which qpdf reads as a line feed and pdf.js, PDFium and Poppler keep), objects beyond /Size, objects present but not indexed, object numbers far apart (beyond the cross-reference rows a file of that size may have: 16 per byte plus 65536): returned untouched. Nothing a file claims (an object stream's /N, the trailer's /Size, an image's /Width, predictor parameters) sizes memory or work on its own; every count is checked against the data it describes, or the file's size, first. |
| JPEG tables | `src/jpeg/` | Baseline and extended sequential JPEG (8-bit) written again with optimal Huffman tables, the same coefficients (like `jpegtran -optimize -copy all`): every scan is read to its coded symbols, which are written back with the best code of at most 16 bits that leaves the all-ones code word free (package-merge); of three orders of the symbols within a code length, the one with the fewest stuffed 0xFF bytes is kept. Every other byte stays (ICC and Adobe markers, comments, restart markers, bytes after the end); one table segment goes before each scan. The new file is read back and compared symbol by symbol before it is used. Progressive, arithmetic-coded, lossless, hierarchical, 12-bit, fractionally sampled and anything unusual or damaged is left as it is, and so are layouts readers could take differently once the tables move (fill bytes before a marker; every segment's contents must fill it exactly), data bound to the file's bytes (multi-picture offsets, APP11 boxes such as content credentials), and files of more than 4096 segments. |
| CCITT fax codec | `src/fax/ccitt.rs`, `tables.rs` | Decoder and encoder for T.4 (1-D, 2-D) and T.6 as CCITTFaxDecode uses them. The decoder records every choice of the original encoder (fill bits, EOLs, tag bits, end codes, padding, bytes after the end, and Adobe's way of choosing between pass and horizontal mode) and keeps a row only if writing it again gives the same bits. Checked against libtiff both ways and against libtiff and Poppler on all 183 fax streams of the test files. |
| JBIG2 | `src/fax/jbig2.rs` | MQ coder; generic-region encoder and decoder (T.88, template 0, nominal adaptive pixels, typical prediction); the embedded stream PDF reads. jbig2dec decodes our output exactly. |
| Fax images, normal PDF | `src/fax/pdf.rs` | A CCITT image becomes JBIG2 when that is at least 5% smaller and nothing is uncertain: one CCITT filter with direct parameters, a DeviceGray image or a mask, the width and height the data holds, at most 2^30 pixels (width rounded up to 32; PDFium's JBIG2 decoder, which pdf.js also uses, shows nothing above 2^31), no byte alignment, EOLs where every reader expects them, every row written back bit for bit, end codes or 0 bits at the end, coding work of at most 2048 pixels per byte of data, and our JBIG2 decoder giving the same pixels. JBIG2Decode's output is the same bits as CCITTFaxDecode's (BlackIs1 handled), so Decode, ImageMask and masks keep their meaning. |
| Encrypted PDFs | `src/pdf/crypt.rs`, `src/pack/sealed.rs` | The standard security handler with the empty user password (see below). RC4 and the key derivations are ours; AES, MD5 and SHA-2 come from RustCrypto. |
| gpress archive | `src/pack/`, `src/cm/` | Any PDF, packed with our context-mixing model (bit-history states, mixers, SSE) with PDF-aware contexts: syntax, predicted object numbers and xref offsets, drawing instructions, image samples coded as residuals. Streams are recognised by their bytes (zlib, JPEG), not only by their dictionaries. Format version 8: a long repeat is coded as one 16-bit-precision decision instead of eight model decisions (huge flat images, duplicates); palette and packed-pixel images are modelled by their exact neighbours; an image's row length is confirmed from its data length; a JPEG behind Flate goes through Lepton when that beats the Flate bytes. Restored byte for byte; every archive is restored and checked before it is returned (with a second thread, the restore decodes the bytes while the encoder writes them). A damaged archive is refused with an error, never restored wrong: the header declares the restored length and the decoded bytes the parts claim in all (`max_len` and `max_decoded` refuse a file declaring more than the caller can afford, before anything is decoded); what a part restores is bounded by what is left of the declared length and what it claims by what is left of the declared decoded total, so the decoder's work and memory follow the two numbers the caller allowed, not what a damaged body says; decoding stops when the data runs out; and the file's checksum is checked at the end. The model's settings, the part kinds and how each is coded are constants of the format version; nothing earlier shipped, so versions before 8 are refused. |
| Type 1 fonts | `src/pack/type1.rs`, `src/cm/model.rs` | Type 1 font programs (FontFile with /Length1 /Length2 /Length3, typical of TeX) lose both layers of encryption before coding: the eexec layer (binary, PFB, and hex when its lines are regular) and the charstring layer inside it (lenIV honoured). Both ciphers are reversible byte for byte and are put back on restore; a font that would not come back exactly is coded as it is. On restore, a layout that doesn't fit inside the font's length is refused before anything is decoded for it. The charstrings get their own model kind, which follows their tokens. |
| Fax images, archive | `src/fax/bilevel.rs`, `part.rs` | A CCITT stream is stored as its image, coded by a bilevel context-mixing model (templates up to four rows high, vertical runs, two mixers, two probability maps, 16-bit precision, typical prediction, a fast path for white areas), plus the encoder's choices; on restore our CCITT encoder writes the original bytes back. Streams under 64 bytes stay plain. The model's work is bounded by the file's size (2^24 plus 512 pixels per byte up to any point of the file); streams past it are coded plainly, and a restore that would go past it is refused. |
| SHA-256 | `src/pack/sha256.rs` | For the restore check. |
| Threads and effort | `src/par.rs`, `flate::Options::effort`, `pdf::rewrite::Options`, `pack::PackOptions` | Standard-library threads only: the segments of a stream, the streams of a PDF and the streams the archive looks at are worked on in parallel and put together in their original order, so the output is the same for any thread count (one thread uses no thread machinery at all, as in a browser). Identical streams are encoded once, and so is an input that streams under other dictionaries share: encoded outputs are remembered for the whole file within `store_budget` (a remembered result is checked against its input before it is used), so memory stays bounded. With the strong encoder, in each group of eight segments of a stream, a segment like an earlier one (the same length, history length and bytes) is written from that one's plans, not planned again. Effort as in the table above (the JPEG tables and the JBIG2 conversion run at every effort). |

## Encrypted PDFs

Both outputs handle files that open with the empty user password (anyone can read them) and use the standard
security handler: /V 1, 2, 4, 5 and /R 2 to 6 (RC4, AES-128, AES-256), one crypt filter (/CFM /V2, /AESV2 or
/AESV3, or /Identity) named by both /StmF and /StrF, and /EncryptMetadata false. The key comes from the empty
password only (algorithms 2, 2.A and 2.B), and is confirmed by /U (algorithms 6 and 11) and, for AES-256, by
/Perms (algorithm 13).

- Normal PDF: strings and streams are decrypted while reading, optimised as usual, and written with the same
  encryption: the same /Encrypt dictionary (and the objects it refers to), /ID, file key and permissions. Each
  string and stream is encrypted with its own object's key; object streams are encrypted as streams, the
  cross-reference stream is not; AES initialization vectors are derived from the file key and the object, so
  the output is the same on every run. Encrypted XMP metadata is compressed (no tool can read it from the file
  anyway); metadata left unencrypted stays as it is. The self-check reads the output back, checks that it opens
  with the same key and dictionary, and compares the decrypted objects. The file keeps exactly the same
  encryption, permission bits and content, so the engine proceeds whatever /P says.
- Archive: each encrypted stream that encrypts back to exactly its stored bytes is stored decrypted (so it
  gets the zlib rebuild, Lepton and the model) and encrypted again on restore. The archive carries the file key
  once and, per stream, its method, object number and generation and its AES initialization vector. When the
  archive isn't clearly smaller than the encrypted streams alone, the archive without decryption is made too
  and the smaller kept. The decoder refuses crypt parts out of place and more than two parts in a row that
  restore nothing.
- Left untouched (normal PDF): files that need a password, other security handlers, signed files, streams
  with their own /Crypt filter, and anything readers may take differently: a /CF entry named /Identity (qpdf
  and pdf.js use it, PDFium and Poppler don't), /CFM /None or no /CFM (qpdf decrypts as AES, PDFium as RC4),
  /StmF and /StrF missing or naming different filters, a /V 4 or 5 key length given nowhere (Poppler takes 40
  bits), a /V 4 key that isn't 128 bits, /V 2 without /Length, /R 2 with more than 40 bits, /O or /U of unusual
  length, a /P outside 32 bits, and literal strings with a raw carriage return. Everything else the gate
  refuses (PDF/A, earlier revisions, ...) still applies. In the archive, files whose handler isn't opened as
  above are packed exactly as any other; a stream with its own /Crypt filter, or one that doesn't encrypt
  back to its stored bytes, is kept as stored; strings are never changed.

## Existing library (optional, `--features existing`)

The engine keeps whichever result is smaller after the checks, so an existing library can only help:

- [lepton_jpeg](https://crates.io/crates/lepton_jpeg) (Apache-2.0, Microsoft): recompresses JPEG streams in
  the archive and restores them bit for bit. Apache-2.0 requires shipping its licence and notices with the
  product.

Flate streams that zlib didn't write (Adobe, cairo, Quartz) are stored as they are. preflate-rs did that job
until its dependency `cabac` turned out to be LGPL, which the licence rule doesn't allow, so it is gone; our
own re-creation coder for those streams is a later roadmap item.

## The licence rule

Our own code plus libraries under MIT, Apache-2.0, BSD, ISC, Zlib, Unicode or MPL-2.0 only; never GPL, AGPL
or LGPL. `deny.toml` holds the rule and `cargo deny check` (installed with `cargo install cargo-deny
--locked`) checks it for every feature and target, in CI and in `bench/security.py supply`, together with the
RustSec advisories, version bans and the source of every crate; `cargo deny` skips crates marked `publish =
false`, so `bench/check_dep_sources.py` (CI and `supply` too) refuses any dependency that is not from
crates.io. A default build depends on `aes`, `md-5` and `sha2` (RustCrypto, MIT or Apache-2.0) and nothing
else; `--features existing` adds `lepton_jpeg` and what it needs (MIT, Apache-2.0, Zlib, BSD-2-Clause,
Unicode-3.0).

## Build and test

```
cargo build --release --workspace                 # the library, the C library and the tool (and the wasm crate as a native rlib)
cargo build --release --features existing         # also lepton_jpeg (Apache-2.0)
cargo test --release --workspace
cargo test --release --workspace --features existing
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check                                  # and python3 -I bench/check_dep_sources.py
cargo build --release -p gpress-wasm --target wasm32-unknown-unknown   # needs: rustup target add wasm32-unknown-unknown
cd crates/gpress-wasm/js && npm ci && npm run build && npm test        # the npm package into js/dist/
cd fuzz && cargo +nightly fuzz run expand -- -max_total_time=600      # needs cargo-fuzz; also inflate, pdf_optimize
```

CI (`.github/workflows/ci.yml`) runs formatting, clippy with warnings as errors on both feature sets, both
test suites, the C example against the shared library, the licence and advisory gate, the WebAssembly package
and its tests, and a short fuzzing run of the three decoders. `tests/api.rs` proves that every option changes
what the engine does; `tests/interaction.rs` runs every feature on one encrypted file (a JPEG, a fax image, a
Type 1 font and a zlib stream) in both outputs; `tests/pack_format.rs` keeps an archive made by the first
build of the format version (`tests/data/format-v8.*`), which must still restore and pack to the same bytes.

## Layout

| Path | What |
| --- | --- |
| `src/api.rs`, `src/clock.rs` | The public API and the deadline behind `time_budget`. |
| `src/flate/` | Our Flate decoder and encoder, and the bit-exact zlib re-creation. |
| `src/pdf/` | PDF objects, strict parser, filters, images, encryption, the normal-PDF writer and its safety gate. |
| `src/jpeg/` | JPEG parsing and the optimal Huffman tables. |
| `src/fax/` | CCITT and JBIG2 codecs, the fax steps of both outputs, the bilevel model. |
| `src/cm/` | The context-mixing model and arithmetic coder. |
| `src/pack/` | The gpress archive (format version 8), Type 1 fonts, encrypted streams, SHA-256. |
| `src/par.rs` | The thread pool. |
| `src/bin/gpress.rs` | The command-line tool. |
| `crates/gpress-c/` | C API, header, example. |
| `crates/gpress-wasm/` | WebAssembly module and the TypeScript package. |
| `fuzz/` | Fuzz targets for the archive decoder, the Flate decoder and the PDF path. |
| `tests/` | Integration tests; `tests/data/` holds the zlib vectors, the encrypted and JPEG fixtures and the format fixture. |
| `bench/` | The benchmark suite, the security suite, the independent checks, the stored results, the corpus lock. |
| `docs/` | The optimisation plan and the task briefs (`docs/tasks/`) for the work it recommends. |
| `deny.toml`, `SECURITY.md` | The licence rule; the threat model and how the safety rules are checked. |

## Safety rules the code follows

- **No data loss.** Every transformation is verified exactly before it is used; anything uncertain leaves
  that stream or file untouched. A damaged archive is refused with an error, never restored wrongly.
- **Untrusted input.** Lengths are bounded by `Options`, decoding stops when data runs out, allocations are
  capped and grow with what is actually decoded, nothing a file claims sizes memory on its own, and no code
  path panics across the library boundary.
- **Deterministic.** Integer arithmetic only in the model; the format's constants are fixed in code and never
  read from the environment; output is identical whatever the thread count, the machine, or the time budget.
- **Licences.** Our code, plus permissive libraries only (above).

`SECURITY.md` has the threat model, the guarantees with where each is kept, and how they are checked.

## Benchmarks (`bench/`)

- `benchmark.py`: the standard measurement, in one command. `time` and `space` measure every file one
  process at a time (wall and CPU time, peak memory, bytes in and out, exactness): ours at each effort and
  thread count, normal PDF and archive, against qpdf (normal PDF) and xz -9e, Brotli 11, Zstandard 19 with
  long matching and expand + xz (archive), each as its own process. A normal-PDF output must open (page count
  and every page's content decoded with pikepdf), an untouched file must be the input byte for byte, and a
  sample of outputs goes through `verify_pdf.py`; every archive is restored with a separate `unpack` and
  compared byte for byte. Rows go to `results/<run>/*.jsonl` as each file finishes, so a stopped run
  continues where it was. `report` makes the tables; `compare BASE NEW` fails when a setting got more than
  10% slower in total, a file more than 2x slower, any output larger, or the peak memory more than 10%
  higher; `selftest` plants each of those and checks they are caught. Every run starts with a calibration job
  so `compare` can tell a slower build from a slower machine. The competitors need measuring only once:
  `--with RUN` takes the settings a run lacks from a stored one (`results/all907-speed-ca8fa92` has every
  competitor on all 907 files). `measure.c` is the small parent process that runs each command (Linux gives
  a process the peak memory of the one that forked it). File lists are in `sets/` (`all`, and `sample120`:
  the 20 slowest and the 20 largest files, 34 with 6 in both lists, then 86 random ones, in two sections
  `--set sample120:random` and `:slowest-and-largest` select); the engine comes from `GPRESS` and the
  corpus from `CORPUS` (default `bench/corpus`, where `fetch_corpus.sh` puts it).
- `p5.py`: what the threads and the store do for a normal PDF, file by file. `run NAME --engine BIN`
  optimizes each file of a list (`--set`, `--files`; `--limit N` takes the N smallest) at each effort and
  thread count, one process at a time with `PDFE_DEBUG` set, and keeps the child's wall and CPU time, peak
  memory, the SHA-256 of its output and the summary line in `results/NAME/p5-eE-tT.jsonl`. `report NAME`
  gives for each effort the files whose output is the same at every thread count, the work saved (identical
  streams, store hits, segments planned once), CPU time against one thread, wall time with the busy and idle
  shares (on a quiet machine only), the store's peak and the peak memory; `--base OTHER` adds the CPU time
  saved on the files where repeats were found.
- `fetch_corpus.sh`, `sets/corpus.lock`: the 907 test files from their four public sources, pinned by commit
  with the sha256 of every file; the script fetches a copy in about two minutes and `--check` proves a copy
  has exactly those bytes.
- `security.py`: hostile input, pass or fail. Twenty archives damaged every way must be refused or restored
  exactly; hostile PDFs (nesting 100,000 deep, loops in the xref chain and the object streams, Flate, LZW and
  RunLength bombs, a 100,000 x 100,000 image, a million objects, bad UTF-16) must come back untouched,
  smaller or with a clean error inside a time and memory bound that grows with the input; mutation fuzzing of
  optimize, pack and unpack under a hard 512 MiB address space; `cargo audit`, the licence of every
  dependency and every `unsafe` block with its reason; the same inputs through the C library
  (`gpress_harness.c`). `selfcheck` plants a wrong file, a crash, a hang, a memory blow-up, a bad licence and
  a bare `unsafe` and checks that each is caught.
- `verify_pdf.py`: the independent check of one output: qpdf --check, pikepdf (object graph followed by
  value, streams decoded; a JPEG whose bytes changed must have the same coefficients, `jpegtran -copy all`
  writing the same bytes for both, and decode to the same pixels with qpdf's libjpeg, Pillow and pdf.js's own
  JPEG decoder, `verify_jpeg_pdfjs.mjs`; CCITT, JBIG2 and JPEG 2000 streams are compared by their decoded
  pixels with pikepdf's decoders, libtiff, jbig2dec and OpenJPEG, then Poppler's when pikepdf can't decode one
  or disagrees) and PDFium (pixels and text of every page, up to 30 pages). jbig2dec and jbig2enc are used
  only by the checks and benchmarks, never by the engine.
- `verify_render.py`, `verify_pdfjs.mjs`: two more renderers on every output, Poppler and pdf.js (pixels and
  text of every page; no new Poppler errors).
- `verify_encryption.py`: an output keeps the original's encryption exactly (qpdf's own reading of the
  revision, permissions, methods and file key, the /ID and the /Encrypt dictionary).
- `check_the_checkers.py`: the checks must catch one small change (a character, a metadata value, the page
  size, one bit of one pixel, one JPEG coefficient, a JPEG layout only pdf.js reads differently) and pass an
  unchanged copy and a JPEG with other Huffman tables.
- `extract_streams.py`, `flate_bench.py`, `jpeg_bench.py`, `fax_bench.py`, `examples/type1_fonts.rs`: the
  codecs one at a time against zlib-9, libdeflate, Zopfli, jpegtran, mozjpeg, xz and jbig2enc.

Results: [bench/RESULTS.md](bench/RESULTS.md). The plan for making the engine faster and leaner without
changing a byte of its output: [docs/optimisation-plan.md](docs/optimisation-plan.md), with a brief per
branch under [docs/tasks/](docs/tasks/README.md).
