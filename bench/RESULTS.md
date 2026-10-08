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

## Security (hostile input)

Every number here comes from `bench/security.py` (run `security`, build speed-ca8fa92, 2026-10-08 05:00:43 UTC), engine at effort 0 with 1 thread, each command its own process measured by bench/measure.c (parent under 1 MB); address-space limit by prlimit. Time bounds apply to the child's CPU time. The machine was shared (load average 6.6 on 2 CPUs at the start), so wall times include waiting for a CPU. The bounds: a damaged archive is refused in under 5 s with a peak under 512 MiB; a hostile PDF returns (untouched, or smaller and proven by the engine's own self-check) or gives a clean error in under 30 s plus 5 s per MiB of input, with a peak under 512 MiB or 16 bytes per input byte, whichever is more; a run past 60 s of wall time (2x its bound when that is more) is a hang. The fuzzer runs with a hard 512 MiB address space.

### Damaged archives

1506 damaged versions of 20 packs (bit flips at 3 bits of 17 positions, cuts at every 1/16 of the length, trailing garbage, a garbage body, a future version and the older versions 0 and 2 with this body, a declared length of 16 GiB, over the format's maximum, and zero, the checksum flipped and zeroed). unpack must error or restore the exact original.

| Result | Cases | Slowest refusal | Peak memory |
| --- | ---: | ---: | ---: |
| Passed (1476 refused, 30 restored exactly) | 1506 | 0.90 s (2.25 s wall) | 167 MiB |
| Failed (wrong file, crash, hang or over bound) | 0 | | |

| Damage | Passed | Failed |
| --- | ---: | ---: |
| checksum_flipped | 20 | 0 |
| checksum_zeroed | 20 | 0 |
| cut | 300 | 0 |
| declared_16gb | 20 | 0 |
| declared_over_max | 20 | 0 |
| declared_zero | 20 | 0 |
| flip | 966 | 0 |
| future_version | 20 | 0 |
| garbage_body | 20 | 0 |
| trailing_garbage | 20 | 0 |
| version_0_body | 20 | 0 |
| version_0_declared_16gb | 20 | 0 |
| version_2_body | 20 | 0 |
| version_2_declared_16gb | 20 | 0 |

### Hostile PDFs

28 runs over the files below, each through optimize and pack (pack's archive is then unpacked and compared). Verdicts: `untouched` or `smaller` (optimize), `packed-exact`, `clean-error`, or the failure.

| File | What it is | Bytes | optimize | pack | Bound |
| --- | --- | ---: | --- | --- | --- |
| `nested_array.pdf` | array nested 100,000 deep | 200,344 | untouched (0.00 s, 4 MiB) | packed-exact (0.05 s, 17 MiB) | 31 s, 512 MiB |
| `nested_dict.pdf` | dictionary nested 100,000 deep | 900,344 | untouched (0.00 s, 5 MiB) | packed-exact (0.11 s, 36 MiB) | 34 s, 512 MiB |
| `length_overflow.pdf` | /Length 9,999,999,999 on a 3-byte stream | 426 | untouched (0.00 s, 3 MiB) | packed-exact (0.03 s, 14 MiB) | 30 s, 512 MiB |
| `huge_image.pdf` | image 100,000 x 100,000 with 64 bytes of data | 570 | smaller (0.01 s, 5 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `flate_bomb.pdf` | Flate stream of 1019 KiB that decodes to 1024 MiB | 1,044,090 | untouched (0.37 s, 70 MiB) | **over-bound:mem 664MiB>512 (5.83 s, 663 MiB)** | 35 s, 512 MiB |
| `flate_bomb_nested.pdf` | two-stage Flate (564 B) decoding to 256 MiB | 1,017 | untouched (0.36 s, 68 MiB) | packed-exact (0.05 s, 15 MiB) | 30 s, 512 MiB |
| `lzw_bomb.pdf` | LZW stream of 5409 B that keeps repeating one byte | 5,847 | smaller (0.33 s, 31 MiB) | packed-exact (0.05 s, 14 MiB) | 30 s, 512 MiB |
| `runlength_bomb.pdf` | RunLength stream of 2 MiB expanding 128x | 2,097,603 | smaller (0.13 s, 29 MiB) | packed-exact (0.23 s, 91 MiB) | 40 s, 512 MiB |
| `bad_utf16_name.pdf` | invalid UTF-16 strings and #-escaped names | 492 | smaller (0.01 s, 4 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `prev_loop.pdf` | /Prev pointing in a loop (classic tables) | 263 | untouched (0.00 s, 3 MiB) | packed-exact (0.03 s, 13 MiB) | 30 s, 512 MiB |
| `xref_stream_loop.pdf` | xref stream whose /Prev is itself and /XRefStm points back | 223 | untouched (0.00 s, 3 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `objstm_selfref.pdf` | xref stream saying an object lives inside object stream 2: itself | 165 | untouched (0.00 s, 4 MiB) | packed-exact (0.03 s, 14 MiB) | 30 s, 512 MiB |
| `objstm_loop.pdf` | object stream that contains itself and /Extends itself | 401 | untouched (0.00 s, 3 MiB) | packed-exact (0.03 s, 14 MiB) | 30 s, 512 MiB |
| `million_objects.pdf` | 1,000,000 tiny objects reachable from the catalog | 52,778,080 | smaller (20.50 s, 518 MiB) | packed-exact (98.37 s, 470 MiB) | 282 s, 805 MiB |

Passed 27 of 28 runs.

### Mutation fuzzing

Mutated corpus files (38 seeds under 1.5 MB whose clean pack peaks under 256 MiB; 1 to 8 random edits each: bit flips, byte changes, cuts, inserts, copied chunks, runs) through each entry point, one process at a time, with a hard 512 MiB address space and 60 s. A crash, an allocation failure, a hang, an uncaught panic or (unpack) a wrong file is a failure, and the input is saved under `failures/fuzz/` with the engine's stderr.

| Entry point | Seeds | Seconds | Iterations | Crashes | Allocation failures | Hangs | Wrong files | Panics |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| optimize | 38 | 600 | 6228 | 0 | 0 | 0 | 0 | 0 |
| pack | 38 | 600 | 870 | 0 | 0 | 0 | 0 | 0 |
| unpack | 12 | 600 | 1667 | 0 | 0 | 0 | 0 | 0 |

Seeds left out because their clean pack at effort 0 does not fit the limit with room to spare (a hit on the limit would then be legitimate work, not the mutation's doing); each is a memory finding in itself:

- `ocrmypdf/tests__resources__enormous.pdf` (0.31 MB): the clean pack ended as oom-abort (370 MiB resident at that point, 3.42 s CPU)
- `ocrmypdf/tests__resources__multipage.pdf` (1.12 MB): the clean pack ended as oom-abort (289 MiB resident at that point, 0.80 s CPU)

### Supply chain

- `cargo audit`: 0 advisories.
- Dependencies of the measured build (`--features existing`): 41; the engine's own code has none, these come with the optional existing libraries. Licences seen: (MIT OR Apache-2.0) AND Unicode-3.0, 0BSD OR MIT OR Apache-2.0, Apache-2.0, BSD-2-Clause, LGPL-3.0-or-later, MIT, MIT OR Apache-2.0, MIT OR Zlib OR Apache-2.0, MIT/Apache-2.0, Unlicense OR MIT, Zlib, Zlib OR Apache-2.0 OR MIT.
- **FAIL: `cabac 0.15.0` is `LGPL-3.0-or-later`** (pulled in through preflate-rs). The rule allows only 0BSD, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, MIT, MPL-2.0, Unicode-3.0, Unicode-DFS-2016, Zlib; GPL, AGPL and LGPL are not allowed.
- `unsafe` in our own code: 2 blocks, each with a reason next to it.
  - `src/flate/parse.rs:82` (reason given) `return unsafe { parse_avx2(data, a, b, m, c, alt, nice, sc, out) };`
  - `src/flate/parse.rs:90` (reason given) `unsafe fn parse_avx2(data: &[u8], a: usize, b: usize, m: &Matches, c: &Costs, alt: bool, nice: usize, sc: &mut ParseScra`

### The suite tests itself

Planted failures (a wrong file returned with exit 0, a crash, a hang, a 600 MiB blow-up, an allocation failure, a GPL licence, an unsafe block without a reason) must each be caught, and a clean run must pass: all as expected.

- wrong-file: caught (ok, WRONG-FILE)
- crash: caught (ok, crash:SIGSEGV)
- hang: caught (ok, hang)
- memory-over-bound: caught (ok, over-bound:mem 608MiB>512)
- oom-abort: caught (ok, oom-abort)
- licence-gpl: caught (ok)
- licence-dual-ok: caught (ok)
- unsafe-without-reason: caught (ok, 2 found)
- clean-run-not-flagged: passed (ok, restored)

### C library

Skipped: no crates/gpress-c in this tree. The harness (`bench/gpress_harness.c`) builds against `crates/gpress-c/include/gpress.h` and runs the same damaged archives and hostile PDFs through `gpress_optimize`, `gpress_compress` and `gpress_expand`; a panic that escapes the library's guard aborts it and counts as a crash, one the guard catches comes back as exit 4 and counts as a bug.

### Failures to fix

Each input is saved under `bench/results/security/failures/` next to the engine's stderr (`*.stderr.txt`); the hostile PDFs are regenerated by `security.py pdfs`. The engine is not changed by this suite.

- pdfs: `flate_bomb.pdf` through pack: over-bound:mem 664MiB>512, 5.83 s CPU, 663 MiB against 35 s / 512 MiB (`failures/pdfs/flate_bomb.pdf`)
- supply: `cabac 0.15.0` is LGPL-3.0-or-later (via preflate-rs)

### Notes on the failures

Written after looking at each failure of the run on build speed-ca8fa92 (effort 0, 1 thread). The engine is not
changed here; these are the exact causes, for whoever schedules the fixes, and two observations that passed
the bounds but belong in the optimisation plan.

- **`flate_bomb.pdf` through pack: 663 MiB for a 1 MiB file (FAIL).** `optimize` is safe (0.4 s, 70 MiB: the
  stream is over the quick budget of 16 decoded bytes per input byte, so it is kept as it is). `pack` is
  not: in `src/pack/mod.rs` `plan()` tries our own rebuild first (`rebuild_params`, which inflates with a
  256 MiB cap, `zlib::unwrap(raw, 1 << 28)`, and gives up), then `preflated()` hands the raw stream to
  `preflate_rs::preflate_whole_deflate_stream` with no cap at all. preflate-rs stops on its own after about
  125 MiB of plain text (`decoded_bytes: 131030720` with `PDFE_DEBUG=1`), the plan then holds that plain
  text, `recreate_whole_deflate_stream` makes the check copy, and the model codes the 125 MiB (`bypassed:
  131946038`): the peak is about 5 bytes per decoded byte, 663 MiB, for an input of 1 MiB. The pack path
  needs the same budget as optimize (decoded bytes per input byte, else store the stream raw) and
  preflate-rs needs a plain-text limit (the task "Bound preflate-rs on damaged archives" is the same
  library's other side). With that fix the stream is stored raw, which is what optimize already does, so
  compression does not change on normal files.
- **`cabac 0.15.0` is LGPL-3.0-or-later (FAIL).** It is a dependency of preflate-rs (the arithmetic coder it
  uses for the corrections) and is linked statically into every build with `--features existing`, including
  the measured one. The owner's rule allows no LGPL. The default build (no features) has no dependencies at
  all. Either the `existing` feature goes, preflate-rs is replaced by our own correction coder, or the crate
  is relicensed upstream; until then the feature cannot ship.
- **The million-object file is slow, not unbounded (passed).** `optimize`: 20 s of CPU and 518 MiB for
  52.8 MB (one million objects go into 5,001 object streams; 10 bytes of memory per input byte, about
  2.5 MB/s). `pack`: 98 s of CPU and 470 MiB (every one of the 52.8 MB of syntax goes through the
  context-mixing model at about 0.55 MB/s), and the restore check takes as long again. Both are inside the
  bound, which grows with the input (30 s + 5 s per MiB; 512 MiB or 16 bytes per input byte), because the
  work is proportional to the input. The throughput itself is a finding for the optimisation plan: syntax
  bytes are the slowest thing the model codes.
- **A declared length sizes the model's tables before a byte is checked (passed).** The damaged archives
  that declare 16 GiB are refused in under a second, but with a peak of 165 to 167 MiB where every other
  refusal stays under 25 MiB: `unpack` builds the model with `table_bits(len)` from the declared length
  (`src/pack/mod.rs`: `primed(table_bits(total_len))` in `run`, `Model::new(table_bits(len))` in
  `fallback_decode`; 2^20 buckets of 16 bytes for each of the 10 contexts), so a 30-byte file costs 160 MiB
  of tables. Inside the 512 MiB bound, and the
  plan's "size the fixed tables to the input" would close it.
- **Fuzz seeds left out.** Two small scans of the corpus need far more than 512 MiB even unmutated at
  effort 0: `ocrmypdf/tests__resources__enormous.pdf` (311 KB, decodes to 243 MB of image; the pack peaks at
  950 MiB without a limit) and `ocrmypdf/tests__resources__multipage.pdf` (1.1 MB, decodes to 142 MB;
  728 MiB). An earlier, unfinished run of this suite counted their mutants as crashes (SIGABRT under the
  512 MiB limit); they were allocation failures of legitimate work, so the seeds are now screened first and
  listed in the table. The memory itself, about 3 bytes per decoded byte held for the whole run, is a
  finding for the space benchmark and the plan.
