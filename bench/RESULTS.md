# Phase 1 results

Test set: 907 public test PDFs, 130.47 MB (the pdf.js test suite, pypdf's sample files, and the OCRmyPDF and
pikepdf test resources). Many of them are deliberately broken or unusual. Measured on a 2-core cloud machine
in October 2026. Every number below comes from the scripts in this folder.

Which build: sections 1 to 5 are the phase-1 engine (commit d85cd7a) at maximum effort. Sections 6 and 7
(time and space) and the Security section are the `speed` build (branch `speed`, commit ca8fa92:
`--effort 0..100`, `--threads N`, the work budget), measured with `bench/benchmark.py` (parts 1 and 2) and
`bench/security.py` (part 3, `benchmark.py security`); the rows are in `bench/results/`.

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

(The builds measured here, d85cd7a and ca8fa92, still had preflate-rs, which re-created Flate streams zlib
had not written. It is gone from the current build with its LGPL dependency, and such streams are now stored
as they are, so archives of files that have them come out larger than these rows: on ten files of the sample
measured on the current build, 55.8% of the input became 75.1% at effort 0 and 72.2% at effort 50; files
without such streams reproduce. Section 4 counts the share of such streams. The whole-set figure moves when
the set is measured again on the current build.)

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

## 6. Time (speed build, commit ca8fa92)

How it is measured (`bench/benchmark.py time`, one command for ours and the competitors): every file is one
process, started from a parent under 1 MB (`bench/measure.c`), and the row records the process's own wall
time (what a user waits), CPU time (user plus system), peak memory, bytes in and out, and the exactness
check: a normal-PDF output must open (page count and every page's content decoded with pikepdf) and every
twelfth file's output goes through `verify_pdf.py` (qpdf --check, the object graph by value, PDFium pixels
and text: 8 outputs per setting here, the other 2 of the 10 picked being untouched files), an untouched file
must be the input byte for byte, and every archive is restored with a separate `unpack` and compared byte
for byte. The competitors are the same as in sections 2 and 3: qpdf (object streams, Flate recompressed at
level 9), xz -9e, Brotli 11 (16 MiB window), Zstandard 19 with long matching, and expand + xz (our `expand`,
then xz, plus the bytes a restore needs), each its own process. In the tables, "Total wall s" and "CPU s"
are sums over the files; the per-file columns (Median, p90, p99, Slowest, MB/s, "< N s") are wall times.

The sample (`bench/sets/sample120.txt`, 120 files, 78.31 MB: 60% of the set's bytes in 13% of its files):
the 20 slowest files of the phase-1 run at maximum effort, the 20 largest, and 80 drawn at random with seed
11. It is a hard sample on purpose. Ours was measured on it at efforts 0, 50 and 100 with 1 and 2 threads;
the competitors on it and on all 907 files. The full run of ours on all 907 files is made on the merged
engine; `compare` checks it against these rows (the sample's files are a subset of the full set).

Two things about the machine. It is a shared 2-CPU cloud VM: the Load column is the 1-minute load average
while the setting was measured, and a load above 2 inflates the wall times (processes queue for a CPU) but
leaves CPU time almost alone, which is why the regression check uses CPU time. And a CPU second is not the
same amount of work on every host a container lands on: after a container restart between the archive runs
below, the same engine work (byte-identical archives, packed and unpacked) took 1.85x the CPU time. Since
then every run stores a calibration (the CPU time of `xz -9e` on 4 MiB of fixed text) in its `meta.json`,
the report prints it, and `compare` warns when the two runs' machines differ (`--scale` takes the ratio out).
The rows below come from the first host (`bench/results/sample120-speed-ca8fa92`, not calibrated); the same
measurements on the host after the restart are in `bench/results/sample120-speed-ca8fa92-host2`
(calibration 4.86 s), as far as they have got, for the regression check of the merged engine.

### Normal PDF (`optimize`), the 120-file sample

| Tool | Total wall s | CPU s | Load | Median s | p90 s | p99 s | Slowest s | Slowest file | MB/s | < 0.1 s | < 1 s | < 10 s | Exact |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ours effort 0, 1 thread | 204.6 | 156.0 | 3.6 | 0.051 | 3.99 | 11.9 | 61.0 | GeoTopo.pdf | 0.38 | 69 | 92 | 116 | 120/120 |
| ours effort 0, 2 threads | 228.0 | 153.6 | 4.2 | 0.023 | 4.22 | 14.2 | 82.7 | GeoTopo.pdf | 0.34 | 75 | 97 | 113 | 120/120 |
| ours effort 50, 1 thread | 297.7 | 274.4 | 2.3 | 0.017 | 6.58 | 33.6 | 105.5 | GeoTopo.pdf | 0.26 | 75 | 94 | 111 | 120/120 |
| ours effort 50, 2 threads | 274.5 | 282.6 | 2.9 | 0.020 | 5.44 | 25.1 | 103.5 | GeoTopo.pdf | 0.29 | 75 | 93 | 114 | 120/120 |
| ours effort 100, 1 thread | 1844.0 | 854.8 | 6.4 | 0.091 | 27.12 | 204.4 | 590.2 | GeoTopo.pdf | 0.04 | 62 | 82 | 96 | 120/120 |
| ours effort 100, 2 threads | 2481.8 | 901.8 | 4.2 | 0.062 | 33.68 | 290.6 | 992.9 | GeoTopo.pdf | 0.03 | 66 | 85 | 101 | 120/120 |
| qpdf | 30.7 | 15.0 | 5.4 | 0.018 | 0.61 | 2.8 | 4.4 | bug1978317.pdf | 2.55 | | | | 118 of 120 (2 encrypted files refused) |

- In CPU time, effort 0 costs 10 times qpdf, effort 50 18 times, effort 100 57 times. The slowest file
  (GeoTopo.pdf: 5.3 MB, 868 streams that decode to 53 MB) takes 55 s of CPU at effort 0, 103 s at effort 50
  and 321 s at effort 100 against qpdf's 1.3 s; its wall times in the table (61, 105 and 590 s) were taken
  at loads of 2.8 to 4.9 on 2 CPUs, so nearly half of the 590 s is waiting for a CPU.
- The second thread saves wall time only where the machine had a CPU free (it had not during most of these
  runs: the loads above are on 2 CPUs); it costs 1 to 5% more CPU time, and the output is byte-identical:
  120 of 120 files at every effort.
- Exactness: every output opened and every untouched file was the input byte for byte; the 8 outputs per
  setting that went through `verify_pdf.py` were identical by object graph, pixels and text.
- The median file is fast at every effort (0.02 to 0.09 s); the cost is in the tail: at effort 100, 24 of
  the 120 files take more than 10 s of wall time (18 in CPU time).

### Archive (`pack`, then a separate `unpack`), the 120-file sample

| Tool | Total wall s | CPU s | Load | Restore s | Median s | p90 s | p99 s | Slowest s | Slowest file | MB/s | < 0.1 s | < 1 s | < 10 s | Exact |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ours effort 0, 1 thread | 410.1 | 341.7 | 3.0 | 184.6 | 0.169 | 8.31 | 40.5 | 46.3 | GeoTopo.pdf | 0.19 | 55 | 83 | 109 | 120/120 |
| ours effort 0, 2 threads | 323.9 | 357.0 | 2.4 | 178.7 | 0.138 | 8.36 | 34.4 | 46.5 | GeoTopo.pdf | 0.24 | 56 | 87 | 110 | 120/120 |
| ours effort 50, 1 thread | 694.4 | 378.7 | 6.0 | 337.4 | 0.525 | 18.08 | 49.9 | 75.9 | issue3188.pdf | 0.11 | 12 | 72 | 100 | 120/120 |
| ours effort 50, 2 threads | 1062.5 | 414.7 | 6.1 | 469.3 | 0.431 | 25.57 | 110.0 | 122.0 | issue3188.pdf | 0.07 | 27 | 74 | 94 | 120/120 |
| brotli 11 | 309.9 | 147.7 | 5.1 | | 0.232 | 7.92 | 27.9 | 50.9 | issue3188.pdf | 0.25 | | | | |
| expand + xz | 405.8 | 196.3 | 4.7 | | 0.120 | 6.63 | 60.3 | 80.6 | bug1978317.pdf | 0.19 | | | | |
| xz -9e | 55.1 | 27.5 | 4.9 | | 0.074 | 1.26 | 3.9 | 5.6 | issue3188.pdf | 1.42 | | | | |
| zstd 19 long | 47.8 | 16.8 | 6.5 | | 0.026 | 1.11 | 4.9 | 5.3 | issue3188.pdf | 1.64 | | | | |

- Effort 100 is the same setting as effort 50 for the archive on this build: above 50 the effort changes
  nothing (below 50 it switches already-compressed data to plain coding), and the outputs are byte-identical
  (120 of 120 files, 1 and 2 threads). Its rows were measured after the container restart (CPU 698 s with 1
  thread, 766 s with 2, restore 369 s and 394 s, on the host that runs the same work 1.85x slower) and live in
  `sample120-speed-ca8fa92-host2`.
- In CPU time, the pack at effort 50 costs 14 times xz -9e and 2.6 times Brotli 11, and the restore about
  half of the pack (the pack already contains its own restore check). Effort 0 is 10% cheaper for 0.2%
  larger archives (section 7).
- Every archive of every setting restored byte for byte with the separate `unpack` (120 of 120).

### The competitors on all 907 files (130.47 MB)

| Tool | Files done | Total wall s | CPU s | Load | Median s | p90 s | p99 s | Slowest s | Slowest file | MB/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| qpdf | 891 | 28.1 | 25.3 | 1.8 | 0.006 | 0.04 | 0.49 | 2.4 | bug1978317.pdf | 4.59 |
| brotli 11 | 907 | 350.1 | 273.5 | 3.2 | 0.026 | 0.74 | 6.1 | 31.4 | issue3188.pdf | 0.37 |
| expand + xz | 907 | 315.5 | 261.9 | 3.0 | 0.036 | 0.32 | 5.2 | 50.0 | bug1978317.pdf | 0.41 |
| xz -9e | 907 | 63.8 | 57.0 | 2.5 | 0.023 | 0.12 | 0.98 | 3.0 | issue3188.pdf | 2.04 |
| zstd 19 long | 907 | 31.7 | 23.1 | 2.9 | 0.005 | 0.05 | 0.63 | 1.8 | issue3188.pdf | 4.12 |

qpdf refuses 16 files: 10 encrypted ones (it asks for a password; ours leaves them untouched) and 6 too
damaged to rebuild. For the record, the phase-1 engine at maximum effort took 2,525 s for the normal PDF of
all 907 files and 2,421 s for the archive (sections 2 and 3, on an earlier host); the full run of the merged
engine with this harness replaces those figures.

## 7. Space (speed build, commit ca8fa92)

Two meanings, both measured by the same rows: the memory while working (the process's peak resident set,
from `wait4`, every file its own process) and the space the output takes.

### Memory while working, the 120-file sample

| Tool | Median MB | Peak MB | Peak on (MB in) | Peak / input, files >= 1 MB: median | max | Fit: fixed MB | + bytes per input byte |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ours pdf, effort 0, 1 thread | 5.0 | 96.0 | issue9262_reduced.pdf (5.54) | 14.6 | 72.4 | 9.3 | 9.9 |
| ours pdf, effort 0, 2 threads | 6.2 | 148.2 | GeoTopo.pdf (5.32) | 19.4 | 89.3 | 11.7 | 12.3 |
| ours pdf, effort 50, 1 thread | 5.1 | 142.7 | issue12963.pdf (0.60) | 14.6 | 72.3 | 11.1 | 9.8 |
| ours pdf, effort 50, 2 threads | 6.2 | 160.7 | GeoTopo.pdf (5.32) | 19.7 | 88.8 | 13.9 | 12.4 |
| ours pdf, effort 100, 1 thread | 5.0 | 169.3 | issue12963.pdf (0.60) | 16.8 | 96.0 | 16.9 | 11.3 |
| ours pdf, effort 100, 2 threads | 5.9 | 305.5 | GeoTopo.pdf (5.32) | 23.5 | 165.9 | 23.0 | 16.4 |
| qpdf | 8.7 | 302.5 | bug1978317.pdf (1.54) | 6.6 | 206.6 | 10.7 | 3.3 |
| ours archive, effort 0, 1 thread | 16.2 | 950.2 | enormous.pdf (0.31) | 48.9 | 681.7 | 36.7 | 41.8 |
| ours archive, effort 0, 2 threads | 23.8 | 1086.2 | enormous.pdf (0.31) | 85.8 | 996.7 | 48.9 | 74.3 |
| ours archive, effort 50, 1 thread | 16.1 | 950.0 | enormous.pdf (0.31) | 51.2 | 681.6 | 36.6 | 42.1 |
| ours archive, effort 50, 2 threads | 25.9 | 1085.6 | enormous.pdf (0.31) | 95.6 | 995.5 | 48.7 | 75.9 |
| brotli 11 | 4.4 | 110.0 | issue3188.pdf (8.13) | 22.8 | 33.5 | 6.2 | 14.0 |
| expand + xz | 44.5 | 675.2 | pdfa multipage.pdf (1.12) | 46.7 | 631.9 | 51.4 | 33.1 |
| xz -9e | 58.0 | 136.4 | issue3188.pdf (8.13) | 41.2 | 71.3 | 34.8 | 17.8 |
| zstd 19 long | 3.9 | 100.2 | issue3188.pdf (8.13) | 23.1 | 35.7 | 4.9 | 17.1 |

- Normal PDF: the typical file needs 5 MB; the peak is 96 to 169 MB with 1 thread and up to 305 MB with 2
  (GeoTopo.pdf at effort 100: every thread holds its own decoded stream and matcher). The peak is not on the
  largest input: issue12963.pdf is 0.6 MB and reaches 143 to 169 MB, because its 312 streams decode to
  52 MB. qpdf's median is higher (8.7 MB) and its peak on bug1978317.pdf (1.5 MB) is 302 MB.
- Archive: the model's tables cost 13.7 MB before any work (next table), the typical file needs 16 MB with 1
  thread and 24 to 26 MB with 2, and the peak is 950 MB with 1 thread and 1,086 MB with 2, both on
  enormous.pdf: a 0.31 MB file whose one image decodes to 243 MB (the peak is four times that; the archive
  itself is 42 KB). The restore (`unpack`) peaks at 718 MB on the same file. The fit says 37 MB fixed plus 42
  bytes per input byte (1 thread): the decoded streams, not the model, are what grows. xz's own figures are
  58 MB typical and 136 MB peak; Brotli and Zstandard stay near 100 MB.
- The browser constraints apply here: a 32-bit WebAssembly address space holds 2 to 4 GB in all, so the
  archive of a file like enormous.pdf does not fit today; the optimisation plan takes this up.

Fixed cost on an empty one-page PDF (420 bytes; the smallest wall time of 5 runs, the peak memory):

| Tool | Wall ms | Peak MB | Bytes out |
| --- | ---: | ---: | ---: |
| ours pdf, effort 0 / 50, 1 thread | 14.0 / 5.3 | 4.6 / 4.5 | 420 (untouched: no saving possible) |
| ours pdf, effort 0 / 50, 2 threads | 5.1 / 6.9 | 5.4 / 5.6 | 420 |
| ours archive, effort 0 / 50, 1 thread | 65.8 / 50.2 | 13.7 / 13.6 | 92 |
| ours archive, effort 0 / 50, 2 threads | 63.2 / 47.6 | 14.1 / 14.1 | 92 |
| qpdf | 3.8 | 8.7 | 598 |
| xz -9e | 2.0 | 3.4 | 296 |
| brotli 11 | 2.6 | 3.5 | 214 |
| zstd 19 long | 1.5 | 2.7 | 246 |

The archive's 50 to 65 ms and 13.7 MB on an empty page are the model's tables being made and cleared; a
browser pays this before any work, and sizing the tables to the input is on the plan. Normal PDF starts in
5 ms and 4.6 MB (the one 14 ms reading was taken under load).

Peak memory (MB) against the input size on the 10 largest files of the sample (1 thread / 2 threads):

| File | MB in | ours pdf e0 | ours pdf e100 | qpdf | ours archive e50 | brotli | expand + xz | xz | zstd |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| issue3188.pdf | 8.13 | 20 / 19 | 19 / 19 | 11 | 291 / 496 | 110 | 244 | 136 | 100 |
| issue19517.pdf | 6.26 | 83 / 86 | 83 / 86 | 17 | 216 / 366 | 47 | 120 | 120 | 94 |
| issue12841_reduced.pdf | 5.85 | 89 / 106 | 94 / 120 | 25 | 174 / 280 | 69 | 73 | 117 | 96 |
| issue9262_reduced.pdf | 5.54 | 96 / 117 | 88 / 128 | 25 | 248 / 452 | 68 | 155 | 114 | 96 |
| GeoTopo.pdf | 5.32 | 89 / 148 | 158 / 305 | 28 | 351 / 646 | 93 | 524 | 112 | 95 |
| issue8795_reduced.pdf | 4.24 | 12 / 12 | 12 / 12 | 23 | 257 / 448 | 63 | 112 | 103 | 93 |
| PDFJS-9279-reduced.pdf | 3.09 | 42 / 59 | 60 / 90 | 16 | 111 / 201 | 52 | 93 | 93 | 57 |
| xfa_filled_imm1344e.pdf | 3.02 | 10 / 10 | 9 / 9 | 11 | 112 / 194 | 48 | 87 | 87 | 55 |
| unreadablemetadata.pdf | 2.82 | 9 / 9 | 8 / 9 | 9 | 122 / 223 | 50 | 124 | 91 | 57 |
| freeculture.pdf | 2.60 | 47 / 57 | 64 / 73 | 15 | 127 / 240 | 63 | 122 | 89 | 57 |

Memory follows what the streams decode to, not the file size: issue3188.pdf (8.1 MB, left untouched because
it keeps earlier revisions) needs 20 MB as a normal PDF, while GeoTopo.pdf (5.3 MB, decoding to 53 MB)
needs 158 MB at effort 100. The archive's second thread nearly doubles the peak on every large file: each
thread holds a decoded stream and its queued output.

### Output size, the 120-file sample

Normal PDF, the like-for-like table (the files ours made smaller, against qpdf on the same files; a qpdf
output larger than the input counts as the input):

| | Files | Before | Ours | Saved | qpdf | qpdf saved |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ours effort 0 | 89 | 52.70 MB | 45.41 MB | 13.8% | 48.30 MB | 8.4% |
| ours effort 50 | 89 | 52.70 MB | 45.13 MB | 14.4% | 48.30 MB | 8.4% |
| ours effort 100 | 89 | 52.70 MB | 45.05 MB | 14.5% | 48.30 MB | 8.4% |

The 31 other files are left untouched by the safety gate (7 keep earlier revisions, 10 are not readable by
the strict parser, 4 have objects beyond /Size, 3 PDF/A, 1 PDF/X, 2 encrypted, 2 with usage rights, 2 with
nothing to save). Counting those 31 as they are, ours gives 90.2% of the original bytes at effort 100 and
qpdf 74.7%, which is not a like-for-like figure: qpdf rewrites every file, dropping earlier revisions,
unreferenced objects and whatever it cannot read (issue3188.pdf, 8.1 MB, comes out at 89 KB). Effort 0 gives
up 0.8% of the saving of effort 100 (45.41 against 45.05 MB) for 5.5 times less CPU; effort 50 gives up
0.2% for 3.1 times less.

Archive, all 120 files (78.31 MB):

| Tool | Bytes out | % of original | Saved | Smallest on |
| --- | ---: | ---: | ---: | ---: |
| ours effort 0 | 41,563,443 | 53.1% | 46.9% | 112 of 120 |
| ours effort 50 (= effort 100) | 41,489,487 | 53.0% | 47.0% | 114 of 120 |
| expand + xz | 45,718,800 | 58.4% | 41.6% | |
| brotli 11 | 58,515,315 | 74.7% | 25.3% | |
| xz -9e | 58,598,904 | 74.8% | 25.2% | |
| zstd 19 long | 58,742,402 | 75.0% | 25.0% | |
| best of brotli, xz, zstd, file by file | 58,377,654 | 74.5% | 25.5% | |

On all 907 files the competitors give: expand + xz 60.6%, Brotli 76.5%, xz 76.8%, Zstandard 77.0%, the best
of the three file by file 76.3% (`bench/results/all907-speed-ca8fa92`), the same as section 3 measured.

### The regression check

`bench/benchmark.py compare BASE NEW` reads two runs and fails when a setting's total CPU time grew more
than 10%, a file got more than 2x slower (counting files that took at least 0.05 s), any output grew, the
peak memory of a setting grew more than 10% (beyond 4 MB), an output stopped being exact, or a file that
worked now fails; it prints the load and the calibration of both runs, says when they differ, and says when
NEW has not measured every file of BASE (an unfinished run compares only the files it has).
`selftest` plants each of those (and a run 80% slower on a machine 80% slower, which passes only with
`--scale`) and checks they are caught. Measuring a new build is `benchmark.py time RUN --set all --who ours`;
`report RUN --with all907-speed-ca8fa92` and `compare` take the competitors from the stored rows.

## Lightweight

The download and the start-up are part of the time a person waits (`docs/tasks/lightweight.md`). Measured on
a 4-core cloud machine in October 2026 with binaryen's wasm-opt 108 and Node 22.22: the sizes and what each
feature costs with Rust 1.99.1, the compiler CI uses, and the choice of the module's build and its start-up
with Rust 1.97.0, on the code of 066b65e and 4a60e08.

### Sizes

`python3 -I bench/sizes.py measure [--features SET]` builds each part and records its size in
`bench/sizes.json`: the native tool and the C library with the release profile, the WebAssembly module the
way the npm package builds it (cargo's `wasm` profile, opt-level "s", then `wasm-opt -O1`; see below). Gzip is
level 9. A feature set is the cargo features a build has: "default" is a plain build, every part of the engine;
"existing" adds Lepton (as they were before the parts became features, next section). The sizes are of main
at 1a8aee1 with this work; "Before" is the same code with the module built the way the package built it until
now, with the release profile (opt-level 3, symbol names kept, no wasm-opt). The release profile is unchanged,
so the native builds are the same as before.

| Build | Feature set | Raw bytes | Gzip bytes | Before: raw, gzip | Gzip change |
| --- | --- | ---: | ---: | ---: | ---: |
| WebAssembly module | default (stands in for "web") | 614,252 | 233,730 | 905,778, 312,697 | -25.3% |
| WebAssembly module | existing | 945,114 | 341,277 | 1,405,707, 459,129 | -25.7% |
| Native tool `gpress` | default | 1,700,424 | 770,376 | the same | 0 |
| Native tool `gpress` | existing | 2,327,568 | 1,012,831 | the same | 0 |
| C library, shared (`libgpress_c.so`) | default | 1,512,864 | 685,128 | the same | 0 |
| C library, shared (`libgpress_c.so`) | existing | 2,139,736 | 926,018 | the same | 0 |
| C library, static (`libgpress_c.a`) | default | 10,178,304 | 2,189,875 | the same | 0 |
| C library, static (`libgpress_c.a`) | existing | 11,275,648 | 2,464,402 | the same | 0 |

The target for the web module is 400 KB gzip (400,000 bytes). Before the "web" feature set (next section), the
module with every part of the engine stood in for it: 233,730 bytes, 166,270 bytes (42%) under the target.
Lepton adds 107,547 bytes of gzip to the module. The static library carries Rust's standard library whole, so
it is far larger than what a program linked with it gets. Main's Flate lists (A5, A6, B3, at 1a8aee1) made
the module 16,191 bytes larger raw and 4,472 bytes larger gzip (598,061 and 229,258 bytes at 4a60e08).

### Sizes of the feature sets

Each part of the engine is a Cargo feature (README, "Building only what a product needs"), and a set is built
with `--no-default-features`, so that it is exactly what is named: "default" is a plain build, every part of
the engine; "web" is the web module's set (`jpeg`, `fax` and `encryption`, without the archive);
"default,existing" is every part and Lepton, which is what "existing" built before the parts were features;
"existing" is now the archive and Lepton and nothing else. Built with Rust 1.99.1, the version CI uses, the
same way as above. "Before" is commit 3b5b501, the start of this work, built and measured the same way.

| Build | Feature set | Raw bytes | Gzip bytes | Before: raw, gzip | Gzip change |
| --- | --- | ---: | ---: | ---: | ---: |
| WebAssembly module | web | 436,113 | 167,352 | | |
| WebAssembly module | default | 599,399 | 229,289 | 598,061, 229,258 | +0.01% |
| WebAssembly module | default,existing | 930,300 | 336,762 | 928,969, 336,784 | -0.01% |
| WebAssembly module | existing | 748,621 | 269,356 | | |
| Native tool `gpress` | web | 1,241,136 | 560,740 | | |
| Native tool `gpress` | default | 1,671,208 | 758,588 | 1,670,088, 757,541 | +0.14% |
| Native tool `gpress` | default,existing | 2,298,816 | 1,000,028 | 2,297,648, 999,551 | +0.05% |
| Native tool `gpress` | existing | 1,844,896 | 786,123 | | |
| C library, shared (`libgpress_c.so`) | web | 1,110,952 | 503,727 | | |
| C library, shared (`libgpress_c.so`) | default | 1,483,672 | 675,456 | 1,482,544, 674,888 | +0.08% |
| C library, shared (`libgpress_c.so`) | default,existing | 2,111,040 | 916,426 | 2,109,864, 915,951 | +0.05% |
| C library, shared (`libgpress_c.so`) | existing | 1,737,192 | 737,554 | | |
| C library, static (`libgpress_c.a`) | web | 9,431,552 | 1,969,658 | | |
| C library, static (`libgpress_c.a`) | default | 10,151,640 | 2,179,659 | 10,145,960, 2,178,308 | +0.06% |
| C library, static (`libgpress_c.a`) | default,existing | 11,249,488 | 2,453,773 | 11,243,800, 2,452,897 | +0.04% |
| C library, static (`libgpress_c.a`) | existing | 10,645,712 | 2,252,805 | | |

With every part built in, the features change each build by -22 to +1,351 bytes of gzip (at most 0.14%): the
error that names a part, and the checks for archive parts a build can't restore. The web module is 163,286
bytes (27.2%) smaller than the whole one, 61,937 bytes (27.0%) of gzip; the next section says where the bytes
go.

The target for the web module is 400 KB gzip (400,000 bytes): at 167,352 bytes it is 232,648 bytes (58%) under
it. Lepton adds 107,473 bytes of gzip to the module with every part.

The budget: CI's WebAssembly job runs `python3 -I bench/sizes.py check`, which builds the web module as the
package does and fails when its gzip size is more than 5% over the recorded one (175,719 bytes today) or over
400,000 bytes. A growth that is wanted is recorded with `measure --features web`. When the module is more than
5% under the record (under 158,984 bytes today), the check passes with a warning to record the new size, since
the 5% is counted from the record and would otherwise allow more. The whole module (`dist/gpress.wasm`) has no
budget of its own; `measure --features default` records it with the other sets.

### What each feature costs

The builds of the Sizes table (Rust 1.99.1; the module as the package builds it, the native tool with the
release profile; gzip level 9) for each set of parts:

| Build | Module raw | Module gzip | Tool raw | Tool gzip |
| --- | ---: | ---: | ---: | ---: |
| Every part (default) | 599,399 | 229,289 | 1,671,208 | 758,588 |
| `web`: every part but `archive` | 436,113 | 167,352 | 1,241,136 | 560,740 |
| Every part but `jpeg` | 555,775 | 213,472 | 1,587,880 | 720,264 |
| Every part but `fax` | 540,210 | 206,832 | 1,452,952 | 660,745 |
| Every part but `encryption` | 522,308 | 200,969 | 1,533,168 | 690,006 |
| None (`--no-default-features`) | 298,671 | 115,455 | 955,976 | 426,959 |
| Every part and `existing` | 930,300 | 336,762 | 2,298,816 | 1,000,028 |

Naming the three parts (`--features jpeg,fax,encryption`) instead of `web` compiles the same code: the same
raw module, 12 bytes more gzip, and 496 bytes less tool (symbol names).

What a part costs is the bytes the build with every part has over the build with every part but that one
(for Lepton, every part and `existing` over every part). "Sample gain" is what the part saves on the
normal-PDF output of the 120-file sample at effort 0: the native tool's total output without the part, less
its total with every part (70,778,025 bytes from 78,313,418; all 120 files byte-identical to the baseline
run). One run each, one process per file, `gpress optimize IN OUT --effort 0 --threads 1`.

| Feature | Module raw | Module gzip | Tool raw | Tool gzip | Sample gain, bytes | Files changed |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `archive` | 163,286 | 61,925 | 430,568 | 197,878 | (not used by normal PDF) | |
| `jpeg` | 43,624 | 15,817 | 83,328 | 38,324 | 27,878 (0.04%) | 10 |
| `fax` | 59,189 | 22,457 | 218,256 | 97,843 | 217,421 (0.31%) | 4 |
| `encryption` | 77,091 | 28,320 | 138,040 | 68,582 | 2,998 (0.004%) | 1 |
| `existing` | 330,901 | 107,473 | 627,608 | 241,440 | (archive only) | |

The four costs add up to more than the 300,728 raw bytes (113,834 gzip) between every part and none, because
some code is there only with two parts: the archive's fax parts and its decrypted streams.

- `jpeg` saves 27,878 bytes on 10 files (freeculture.pdf 11,990, issue19971.pdf 8,683); one file,
  issue13931.pdf, comes out 1,541 bytes larger with the step than without it. It costs 15,817 bytes of gzip in
  the module, a little more than half of what it saves on the whole sample. Kept.
- `fax` saves 217,421 bytes on 4 files (freeculture.pdf 189,561, the OCRmyPDF ccitt.pdf 26,616) for 22,457
  bytes of gzip: worth its cost. Kept.
- `encryption` costs the most of the three, 28,320 bytes of gzip in the whole module (24,708 in the web module:
  142,644 bytes without it), and saves the least: the sample has three encrypted files, one of which the
  engine rewrites (issue17215.pdf, 2,998 bytes smaller); the other two are left untouched in every build (one
  needs a password, one has a handler the engine doesn't take). On this sample it costs far more than it
  saves. It stays in the default and in `web`: without it an encrypted file the engine can rewrite is left
  untouched, so the web compressor would give a different answer from the native tool for those files, and
  the web module is 58% under its budget with it.
- `archive` is most of the difference between the two modules (61,925 bytes of gzip), and Lepton (`existing`)
  would cost more than every normal-PDF part together; neither is in `web`.

### Choosing the module's build

Every build of the module in Node (`bench/wasm_speed.mjs`): one process per run, pinned to one core, two runs at
a time on two cores in a rotating order, three runs of each build; the CPU time of the optimize calls, median of
the three. Effort 0 on all 120 files of the sample, effort 50 on the 30 files of
`bench/sets/sample120-size30.txt` (the sample sorted by size, every fourth file: 657 B to 6.3 MB). Opt-level 3
is the `wasm` profile at opt-level 3 (symbols stripped). The effort-0 cells of the -O1 rows come from a later
batch that measured opt-level 3 and "s" again beside them (245.6 s and 257.6 s, +4.9%), and their "vs 3" is
against that batch's 245.6 s; -O1 of "z" was not run at effort 0. "First call": from the module's bytes to the
first optimized PDF (one page, 3.8 KB, effort 0) in a fresh Node process, median of 21 processes, on two cores,
measured with `bench/wasm_startup.mjs` (see Start-up below).

| opt-level | wasm-opt | Raw bytes | Gzip bytes | Effort 0, CPU s | vs 3 | Effort 50, CPU s | vs 3 | First call, ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3, before (release profile) | none | 841,082 | 293,702 | | | | | 79.7 |
| 3 | none | 766,709 | 279,771 | 261.1 | | 83.4 | | 79.5 |
| 3 | -O1 | 711,712 | 272,498 | 247.7 | +0.8% | 82.0 | -1.7% | 80.4 |
| 3 | -Os | 697,417 | 275,292 | 251.6 | -3.6% | 85.9 | +3.0% | 79.9 |
| 3 | -Oz | 692,060 | 274,932 | 267.4 | +2.4% | 88.5 | +6.1% | 78.8 |
| s | none | 652,516 | 235,380 | 268.5 | +2.8% | 87.6 | +5.0% | 68.1 |
| **s** | **-O1** | **600,115** | **228,746** | **256.9** | **+4.6%** | **86.7** | **+3.9%** | **71.8** |
| s | -Os | 579,983 | 244,594 | 273.6 | +4.8% | 90.6 | +8.6% | 71.9 |
| s | -Oz | 572,933 | 244,364 | 273.9 | +4.9% | 88.1 | +5.6% | 76.1 |
| z | none | 587,289 | 212,192 | 476.3 | +82.4% | 140.6 | +68.5% | 116.9 |
| z | -O1 | 530,236 | 204,651 | | | 134.2 | +60.8% | 95.1 |
| z | -Os | 508,982 | 218,570 | 464.5 | +77.9% | 133.4 | +59.8% | 103.4 |
| z | -Oz | 501,190 | 218,626 | 538.9 | +106.4% | 148.4 | +77.8% | 128.1 |

Every output of every run was byte-identical to the native tool's at 066b65e (the hashes of the baseline
benchmark run, `--features existing`, one thread): the 120 files at effort 0 and the 30 at effort 50, three
times over for each build.

- opt-level "z" is 10% smaller than "s" but 1.8 times slower at effort 0 (1.7 at effort 50). "s" is 3% to 5%
  slower than opt-level 3 at effort 0 (two batches) and 5% at effort 50, inside the 10% the brief allows, and
  its first call comes 10 to 15 ms sooner (less code to compile). The profile uses "s".
- wasm-opt: -Oz and -Os take 11% to 12% off the raw bytes of the "s" build but add 4% to its gzip size, which is
  what a browser downloads, and the first call with -Oz took 4 to 8 ms longer than with -O1 in three batches
  (the table's, and two more of 31 processes for the "s" builds: 73.1 against 65.0 ms, 77.2 against 71.8). -O1
  takes 8% off the raw bytes and 3% off the gzip size at the same speed, so the package build runs -O1 (the
  brief named -Oz; it is one word in `crates/gpress-wasm/js/build-wasm.mjs`). Without wasm-opt on PATH the
  build uses cargo's module as it is and says so.
- For scale: the native tool takes 208.6 s of CPU for the same 120 files at effort 0 (the baseline run, one
  process per file), so the module takes about 1.2 to 1.3 times the native time.

### Start-up (the WebAssembly module in Node)

The target: compiling the module, making an instance and optimizing a one-page PDF of a few KB take at most
50 ms in all, measured in CI's Node test. `bench/wasm_startup.mjs` measures it from the module's bytes in
memory: compile, instantiate, the first `optimize` call, and a second call in the same process (the same work
once the code is compiled). Each sample is a fresh Node 22.22 process, because V8 reuses compiled code within
a process; the modules are measured in turn, round after round, and the tables give medians. The PDF is 3,851
bytes, made by `crates/gpress-wasm/js/test/onepage.mjs`: a Flate content stream of text in two fonts, and the
two fonts with their widths and descriptors (named, not embedded: the file holds no font program; real files
that do are below). It comes out at 2,774 bytes at effort 0, 2,777 at effort 50 and 2,767 at effort 100, the
same bytes as the native tool. CI runs the same measurement in `crates/gpress-wasm/js/test/startup.test.mjs`:
7 processes at effort 0 and 7 at the default effort, and it prints the median and its distance to 50 ms. Its
limit is 50 ms at effort 0 and twice that at the default effort; `GPRESS_STARTUP_LIMIT_MS` raises it, and CI
sets it to 150 ms (300 at the default effort) because its runner has two cores (below).

"today (3)" is the module as main built it before this change: every part built in (the web feature set does not
exist yet), the release profile (opt-level 3). The package now builds opt-level s with `wasm-opt -O1` (Sizes,
above); its numbers on one core are below the one-core table. "s" and "z" are the same code at those opt-levels, "+Oz" after `wasm-opt -Oz` (binaryen
108), and "S1" is session 1's unmerged branch (A1, A3 and A4 of the plan, commit 5e37ac6) built the same way.
Medians in ms of 11 processes on three idle cores of the 4-core machine (the fourth was kept for the baseline
benchmark), from `taskset -c 0-2 node bench/wasm_startup.mjs --runs 11 --quiet 0-2 LABEL=module.wasm ...`.
Other work shared the machine, so a round counted only when the three cores had been idle for a second
before it and after it (`--quiet`; no round was dropped in this run):

| Module | Bytes (gzip) | Effort 0: compile | instantiate | first call | **total** | second call | Effort 100: **total** | second call |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| today (3) | 841,082 (294,455) | 4.1 | 0.2 | 50.6 | **54.5** | 23.9 | **85.5** | 52.5 |
| s | 846,550 (263,987) | 4.2 | 0.2 | 50.5 | **55.0** | 27.6 | **92.9** | 59.2 |
| z | 897,443 (256,028) | 4.1 | 0.2 | 87.5 | **91.8** | 58.9 | **166.8** | 149.9 |
| s +Oz | 573,121 (244,046) | 3.6 | 0.2 | 56.1 | **59.9** | 25.5 | **90.6** | 54.5 |
| s +Oz, one-caller inlining capped (`-ocimfs 20`) | 576,816 (227,619) | 3.9 | 0.2 | 58.3 | **62.3** | 31.0 | **88.6** | 59.0 |
| z +Oz | 501,378 (218,201) | 3.7 | 0.2 | 96.4 | **100.3** | 68.8 | **209.5** | 166.6 |
| S1 (3) | 855,462 (301,977) | 4.3 | 0.2 | 41.8 | **46.7** | 21.4 | **68.7** | 29.6 |
| S1 s | 855,987 (268,373) | 4.1 | 0.2 | 35.1 | **39.4** | 16.1 | **63.8** | 33.7 |
| S1 s +Oz, `-ocimfs 20` | 585,050 (231,454) | 4.2 | 0.2 | 38.2 | **42.8** | 15.5 | **61.3** | 31.5 |

Medians on the same idle cores move by about a tenth from one session to the next: at effort 0 today's
module took 54.5, 54.9 and 59.8 ms in three sessions of 9 to 11 processes (56.1 in an earlier one), and
session 1's branch 46.7, 47.9 and 46.8 (42.5 earlier); single samples of session 1's branch ranged from 38 to
64 ms.

The CI test on the same idle cores: 59.9 ms at effort 0 with today's module, 47.4 ms with session 1's branch
and 41.3 ms with session 1's branch and the `memcmp` change below; 85.9, 61.9 and 61.9 ms at the default
effort. Run 6 times more on those cores at the plain 50 ms limit, the CI test passed 4 times with session 1's
branch (medians 44.1 to 48.5 ms) and failed twice (51.2 and 55.0 ms): its margin is within the spread of seven
samples. The native tool takes about 24 ms at effort 0 and 49 ms at effort 100 on the same file (medians of
three sessions of 21 runs on one idle core, process start included: 24.9, 24.4 and 24.0 ms, and 49.4, 47.8
and 49.2 ms; the fastest runs 22 and 43 ms), so the module's start-up at effort 0 takes about 2.3 times the
native tool's whole run, and 1.7 times at effort 100.

On two cores (`taskset -c 0,2 node bench/wasm_startup.mjs --quiet 0,2`), as GitHub gives a private
repository's runner, the first call is slower: V8's optimizing compiler then has one core beside the work
instead of two. Session 1's branch gains little there (two sessions of 11 processes, medians in ms):

| Module | Effort 0: **total** | second call | Effort 100: **total** | second call |
| --- | ---: | ---: | ---: | ---: |
| today (3) | **71.9**, **81.4** | 24.2, 26.1 | **104.7**, **104.9** | 51.7, 52.9 |
| S1 (3) | **70.9**, **69.9** | 18.5, 18.2 | **88.9**, **92.0** | 33.0, 31.3 |

`npm test` runs the test files one at a time (`--test-concurrency=1`): on three cores the start-up test
otherwise shares them with the round-trip tests, and it took 70 to 87 ms at effort 0 in four runs with today's
module, against 59 to 70 ms alone. Run on two cores here (`taskset -c 0,2`) with today's module it passed 3
runs of 3 at the 150 ms limit, with medians of 73.7, 81.5 and 93.6 ms at effort 0 and 98.1 to 115.4 ms at the
default effort; session 1's module failed 3 runs of 3 there at a 50 ms limit (68.0 to 76.7 ms). GitHub's
two-core runner is slower: the first CI run of the test, with main's Flate lists merged (below), took 135,
152, 141, 133, 114, 100 and 97 ms at effort 0 (median 132.8) and a median of 147.2 ms at the default effort.
Its first processes were the slowest, so the test now starts one process first that it does not count.

On one core (`taskset -c 0 node bench/wasm_startup.mjs --runs 11`) the first call is much slower, because V8's
optimizing compiler then shares the core with the work; `--liftoff-only` turns that compiler off:

| Module | Effort 0: **total** | second call | Effort 100: **total** | second call | `--liftoff-only`, effort 0: first call | **total** | second call |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| today (3) | **118.7** | 29.6 | **154.4** | 54.0 | 53.7 | **60.6** | 41.5 |
| s | **86.5** | 29.6 | **129.2** | 68.0 | 57.6 | **63.9** | 45.7 |
| z | **116.9** | 63.8 | **211.1** | 149.8 | | | |
| s +Oz | **111.9** | 28.5 | **149.0** | 65.2 | | | |
| s +Oz, `-ocimfs 20` | **93.3** | 30.9 | **122.7** | 67.6 | | | |
| z +Oz | **139.2** | 75.0 | **257.0** | 172.5 | | | |
| S1 (3) | **118.1** | 37.8 | **150.5** | 32.0 | 36.2 | **43.6** | 21.7 |
| S1 s | **76.3** | 18.7 | **103.9** | 38.4 | 34.5 | **41.6** | 24.2 |
| S1 z | **82.6** | 28.3 | **115.3** | 54.0 | | | |
| S1 s +Oz, `-ocimfs 20` | **83.5** | 22.5 | **99.0** | 37.1 | | | |
| S1 (3), `package_merge` without `memcmp` | | | | | 32.6 | **39.7** | 20.1 |

One core gives the widest spread: a second session (`--quiet 0`) gave 131.1 ms at effort 0 for today's module,
86.0 for s, 118.9 for S1 (3) and 80.2 for S1 s.

The package's build, opt-level s with `wasm-opt -O1` (600,066 bytes), measured later beside today's module in
the same rounds on one idle core (`taskset -c 3 node bench/wasm_startup.mjs --runs 11 --quiet 3`, built from
main 4a60e08): 89.0 ms at effort 0 (second call 30.5) and 117.6 ms at effort 100 (68.6), against 137.3 and
154.4 ms for today's module (27.3 and 55.0). It starts 35% sooner at effort 0 on one core, like s alone.

Main's Flate lists (A5, A6 and B3 of the plan, merged at 1a8aee1) slow the module down. Built the package's
way from this branch before and after merging them (Rust 1.97.0, outputs byte-identical), start-up on one idle
core went from 85.7 to 110.5 ms at effort 0 and from 125.7 to 158.0 ms at effort 100 (second calls 31.1 to
34.7 and 64.6 to 81.7 ms), and `bench/wasm_speed.mjs` on the 30 files of `sample120-size30` at effort 0 from
38.4 and 37.8 s of CPU to 41.5 and 41.2 s (+9%). The other tables here were measured before that merge.

Where the time goes, today's module at effort 0:

- **Compile, 4 to 7 ms.** V8 decodes and validates the 717 KB of code; it compiles each function only when it
  is first called. Making later instances from the compiled `WebAssembly.Module` (`Gpress.compile` once,
  then `Gpress.load(module)`) costs 0.3 ms each instead; compiling the same bytes again in one process cost 4
  to 7 ms.
- **Instantiate, 0.2 to 0.5 ms.** Nothing is built at start-up: 48 KB of data and 17 pages (1.06 MiB) of
  memory, no constructor; every table of the engine is made when a call needs it.
- **The first call: 51 ms on idle cores, 112 to 122 ms on one core, for work that takes 24 to 30 ms once
  compiled.** The first call compiles the 217 functions it reaches (242 KB of code) with V8's quick compiler,
  Liftoff, and touches its memory for the first time: about 12 ms (53.7 against 41.5 ms with Liftoff alone).
  It then runs in Liftoff's code, 1.4 to 1.7 times slower than optimized code here, while the optimizing
  compiler (TurboFan) works on the 21 hot functions in the background. V8 switches a function to optimized
  code only when it is next called, so a large function that runs once runs unoptimized throughout: deflate's
  per-segment closure (12.8 KB of code, with the parse rounds, the block split and the polish inlined into it)
  took TurboFan up to 90 ms on a busy machine. On idle cores the optimized code comes too late for most of
  the first call (50.8 ms, against 53.7 with Liftoff alone on one core); on one core compiling it costs the
  first call more than it gives back (118.7 ms against 60.6). Smaller functions are compiled sooner: on one
  core opt-level s starts 27 to 34% faster than 3 at effort 0 (16 to 18% at effort 100), and `wasm-opt -Oz`,
  which inlines every function that has one caller, gives that back unless that inlining is capped (`-ocimfs
  20`: 576,816 bytes against 573,121, and a smaller gzip, 227,619 against 244,046). On idle cores 3 and s start
  about alike (54.5 and 55.0 ms at effort 0, 85.5 and 92.9 at effort 100). z runs 2.2 to 3 times slower than 3
  once compiled, and starts 50 to 70% slower at effort 0.
- **The work is Flate.** In a profile of the two calls at effort 0 (`--cpu-prof`, which names the module's
  functions) 88% of the time is `deflate_zlib`, for four streams: the content stream, the object stream and
  two tries of the cross-reference stream. Its fixed costs lead: `header_search` 22%, `estimate_dynamic_bits`
  15%, `limited_lengths_into` 13%, `optimal_nice` 9%, `fast_lengths` 5%, `find_matches` 3% (session 1's A1,
  A3 and A4). With session 1's branch, 11% of the time at effort 0 is `memcmp`, called from `package_merge`
  for `cur[..k] == prev[..k]`: in WebAssembly `memcmp` is a loop over bytes. Comparing the u64 values in a
  loop takes it out of the profile, and the CI test's median from 47.4 to 41.3 ms.

Tables sized for the largest case: on the optimize path only the match finder's two tables (`head`, 256 KiB,
and `son`, 512 KiB), which every `find_matches` call allocates and fills with NIL, even for a 60-byte stream.
Session 1's A4 keeps them per thread, allocated zeroed (fresh pages, no fill), and clears only the entries a
search reads, so a small stream touches a few KB of them; this branch leaves `matcher.rs` to it, so until
session 1's branch is merged the match finder still builds its largest-case tables on every call. Everything
else on the path is sized to its input: the parse's cost arrays, the match and symbol lists, the bit writer,
the inflate output. The archive is not on this path, and its tables are fixed: in the module a compress of
the same PDF takes about 120 ms and an expand 50 ms, and the memory grows to 12.7 MB. A model holds about
11.4 MB whatever the input (context tables of at least 2.6 MB, a 4 MiB match table, and a 65,536-context APM
table of 4.3 MiB that is filled entry by entry with a computed value at every `Model::new`); `Model::new` is
23 ms of the two calls, and priming the model with its fixed text is most of the rest (`src/cm/`,
`src/pack/`: the archive briefs).

The test PDF holds a single Flate stream. Real one-page PDFs of 3 to 6 KB that embed a font program (a
Flate `FontFile2` or `FontFile3` of 0.9 to 4.2 KB) take from about as long to two and a half times as long,
the more the larger the program (`--pdf`, files from the corpus's `pdfjs/` folder, effort 0, 9 processes on
three idle cores, medians in ms):

| File | Bytes | Font program | today: **total** | second call | S1: **total** | second call |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| the test PDF | 3,851 | none | **59.8** | 24.8 | **46.8** | 17.0 |
| `Embedded_font.pdf` | 3,323 | 909 bytes | **51.7** | 24.0 | **46.3** | 17.5 |
| `TrueType_without_cmap.pdf` | 3,989 | 1,916 bytes | **74.9** | 41.7 | **62.9** | 24.5 |
| `issue7492.pdf` | 4,619 | 2,191 bytes | **78.7** | 45.5 | **75.2** | 27.7 |
| `issue5256.pdf` | 6,136 | 4,244 bytes | **149.7** | 104.9 | **119.7** | 60.1 |

The target, at effort 0, on the test PDF. On three idle cores session 1's branch meets it, but not reliably:
its medians were 46.7 to 47.9 ms in three sessions here (42.5 in an earlier one), and the CI test failed 2 runs
of 6 (medians 51.2 and 55.0 ms). Without that branch it is 5 to 10 ms short (54.5 to 59.8 ms). On two cores,
as CI's runner has, neither meets it: 72 to 81 ms today and about 70 with session 1's branch; on one core 119
to 131 ms today and 76 to 80 with session 1's branch at opt-level s. So in CI's Node test the target is not
met, with or without session 1's branch (GitHub's two-core runner took a median of 133 ms): the test prints
the distance, and fails only above 150 ms, to catch a large step back. Real one-page files that embed a font program are further from it: 46 to 120 ms
with session 1's branch on three idle cores. The default effort (100) does not reach 50 ms: 85.5 ms today and
68.7 with session 1's branch on idle cores, when the native tool itself takes about 49 ms on this file. The
fast tier (`fast-tier.md`, F1), which replaces the optimal parse and the block search that are most of the
first call at effort 0, would leave a wide margin. Opt-level s for the module (L2) starts as fast as 3 at
effort 0 on idle cores (9% slower at effort 100) and saves a quarter to a third on one core.

## Security (hostile input)

Every number here comes from `bench/security.py` (run `security`, build main-d86b799, 2026-10-08 17:45:03 UTC), engine at effort 0 with 1 thread, each command its own process measured by bench/measure.c (parent under 1 MB); address-space limit by prlimit. Time bounds apply to the child's CPU time. The bounds: a damaged archive is refused in under 5 s with a peak under 512 MiB; a hostile PDF returns (untouched, or smaller and proven by the engine's own self-check) or gives a clean error in under 30 s plus 5 s per MiB of input, with a peak under 512 MiB or 16 bytes per input byte, whichever is more; a run past 60 s of wall time (2x its bound when that is more) is a hang. The fuzzer runs with a hard 512 MiB address space.

### Damaged archives

1506 damaged versions of 20 packs (bit flips at 3 bits of 17 positions, cuts at every 1/16 of the length, trailing garbage, a garbage body, a future version and the older versions 0 and 2 with this body, a declared length of 16 GiB, over the format's maximum, and zero, the checksum flipped and zeroed). unpack must error or restore the exact original.

| Result | Cases | Slowest refusal | Peak memory |
| --- | ---: | ---: | ---: |
| Passed (1476 refused, 30 restored exactly) | 1506 | 1.00 s (1.03 s wall) | 167 MiB |
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
| `nested_array.pdf` | array nested 100,000 deep | 200,344 | untouched (0.00 s, 4 MiB) | packed-exact (0.06 s, 17 MiB) | 31 s, 512 MiB |
| `nested_dict.pdf` | dictionary nested 100,000 deep | 900,344 | untouched (0.00 s, 5 MiB) | packed-exact (0.13 s, 36 MiB) | 34 s, 512 MiB |
| `length_overflow.pdf` | /Length 9,999,999,999 on a 3-byte stream | 426 | untouched (0.00 s, 3 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `huge_image.pdf` | image 100,000 x 100,000 with 64 bytes of data | 570 | smaller (0.01 s, 5 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `flate_bomb.pdf` | Flate stream of 1019 KiB that decodes to 1024 MiB | 1,044,090 | untouched (0.44 s, 70 MiB) | packed-exact (0.97 s, 260 MiB) | 35 s, 512 MiB |
| `flate_bomb_nested.pdf` | two-stage Flate (564 B) decoding to 256 MiB | 1,017 | untouched (0.44 s, 68 MiB) | packed-exact (0.05 s, 16 MiB) | 30 s, 512 MiB |
| `lzw_bomb.pdf` | LZW stream of 5409 B that keeps repeating one byte | 5,847 | smaller (0.38 s, 32 MiB) | packed-exact (0.06 s, 14 MiB) | 30 s, 512 MiB |
| `runlength_bomb.pdf` | RunLength stream of 2 MiB expanding 128x | 2,097,603 | smaller (0.13 s, 29 MiB) | packed-exact (0.27 s, 92 MiB) | 40 s, 512 MiB |
| `bad_utf16_name.pdf` | invalid UTF-16 strings and #-escaped names | 492 | smaller (0.01 s, 5 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `prev_loop.pdf` | /Prev pointing in a loop (classic tables) | 263 | untouched (0.00 s, 3 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `xref_stream_loop.pdf` | xref stream whose /Prev is itself and /XRefStm points back | 223 | untouched (0.00 s, 3 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `objstm_selfref.pdf` | xref stream saying an object lives inside object stream 2: itself | 165 | untouched (0.00 s, 3 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `objstm_loop.pdf` | object stream that contains itself and /Extends itself | 401 | untouched (0.00 s, 4 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `million_objects.pdf` | 1,000,000 tiny objects reachable from the catalog | 52,778,080 | smaller (25.13 s, 523 MiB) | packed-exact (117.10 s, 470 MiB) | 282 s, 805 MiB |

Passed 28 of 28 runs.

### Mutation fuzzing

Mutated corpus files (38 seeds under 1.5 MB whose clean pack peaks under 256 MiB; 1 to 8 random edits each: bit flips, byte changes, cuts, inserts, copied chunks, runs) through each entry point, one process at a time, with a hard 512 MiB address space and 60 s. A crash, an allocation failure, a hang, an uncaught panic or (unpack) a wrong file is a failure, and the input is saved under `failures/fuzz/` with the engine's stderr.

| Entry point | Seeds | Seconds | Iterations | Crashes | Allocation failures | Hangs | Wrong files | Panics |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| optimize | 38 | 120 | 2623 | 0 | 0 | 0 | 0 | 0 |
| pack | 38 | 120 | 766 | 0 | 0 | 0 | 0 | 0 |
| unpack | 12 | 120 | 752 | 0 | 0 | 0 | 0 | 0 |

Seeds left out because their clean pack at effort 0 does not fit the limit with room to spare (a hit on the limit would then be legitimate work, not the mutation's doing); each is a memory finding in itself:

- `ocrmypdf/tests__resources__enormous.pdf` (0.31 MB): the clean pack ended as oom-abort (370 MiB resident at that point, 4.33 s CPU)
- `ocrmypdf/tests__resources__multipage.pdf` (1.12 MB): the clean pack ended as oom-abort (271 MiB resident at that point, 0.97 s CPU)

### Supply chain

- `cargo audit`: 0 advisories.
- Dependencies of the measured build (`--features existing`): 43; the engine's own code has none, these come with the optional existing libraries. Licences seen: (MIT OR Apache-2.0) AND Unicode-3.0, 0BSD OR MIT OR Apache-2.0, Apache-2.0, Apache-2.0 OR MIT, BSD-2-Clause, MIT, MIT OR Apache-2.0, MIT OR Zlib OR Apache-2.0, MIT/Apache-2.0, Unlicense OR MIT, Zlib, Zlib OR Apache-2.0 OR MIT.
- Every dependency licence is on the allowed list.
- `unsafe` in our own code: 2 blocks, each with a reason next to it.
  - `src/flate/parse.rs:85` (reason given) `return unsafe { parse_avx2(data, a, b, m, c, alt, nice, sc, out) };`
  - `src/flate/parse.rs:94` (reason given) `unsafe fn parse_avx2(data: &[u8], a: usize, b: usize, m: &Matches, c: &Costs, alt: bool, nice: usize, sc: &mut ParseScra`

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

The same inputs through the C API: 0 failures.

#### Damaged archives through gpress_expand

1506 damaged versions of 20 packs (bit flips at 3 bits of 17 positions, cuts at every 1/16 of the length, trailing garbage, a garbage body, a future version and the older versions 0 and 2 with this body, a declared length of 16 GiB, over the format's maximum, and zero, the checksum flipped and zeroed). expand must error or restore the exact original.

| Result | Cases | Slowest refusal | Peak memory |
| --- | ---: | ---: | ---: |
| Passed (1476 refused, 30 restored exactly) | 1506 | 0.99 s (1.46 s wall) | 167 MiB |
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

#### Hostile PDFs through gpress_optimize and gpress_compress

28 runs over the files below, each through optimize and pack (pack's archive is then unpacked and compared). Verdicts: `untouched` or `smaller` (optimize), `packed-exact`, `clean-error`, or the failure.

| File | What it is | Bytes | optimize | pack | Bound |
| --- | --- | ---: | --- | --- | --- |
| `nested_array.pdf` | array nested 100,000 deep | 200,344 | untouched (0.00 s, 4 MiB) | packed-exact (0.05 s, 17 MiB) | 31 s, 512 MiB |
| `nested_dict.pdf` | dictionary nested 100,000 deep | 900,344 | untouched (0.00 s, 5 MiB) | packed-exact (0.13 s, 36 MiB) | 34 s, 512 MiB |
| `length_overflow.pdf` | /Length 9,999,999,999 on a 3-byte stream | 426 | untouched (0.00 s, 3 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `huge_image.pdf` | image 100,000 x 100,000 with 64 bytes of data | 570 | smaller (0.01 s, 5 MiB) | packed-exact (0.03 s, 14 MiB) | 30 s, 512 MiB |
| `flate_bomb.pdf` | Flate stream of 1019 KiB that decodes to 1024 MiB | 1,044,090 | untouched (0.42 s, 70 MiB) | packed-exact (0.98 s, 260 MiB) | 35 s, 512 MiB |
| `flate_bomb_nested.pdf` | two-stage Flate (564 B) decoding to 256 MiB | 1,017 | untouched (0.43 s, 67 MiB) | packed-exact (0.05 s, 16 MiB) | 30 s, 512 MiB |
| `lzw_bomb.pdf` | LZW stream of 5409 B that keeps repeating one byte | 5,847 | smaller (0.40 s, 32 MiB) | packed-exact (0.05 s, 14 MiB) | 30 s, 512 MiB |
| `runlength_bomb.pdf` | RunLength stream of 2 MiB expanding 128x | 2,097,603 | smaller (0.13 s, 29 MiB) | packed-exact (0.31 s, 91 MiB) | 40 s, 512 MiB |
| `bad_utf16_name.pdf` | invalid UTF-16 strings and #-escaped names | 492 | smaller (0.01 s, 5 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `prev_loop.pdf` | /Prev pointing in a loop (classic tables) | 263 | untouched (0.00 s, 3 MiB) | packed-exact (0.03 s, 14 MiB) | 30 s, 512 MiB |
| `xref_stream_loop.pdf` | xref stream whose /Prev is itself and /XRefStm points back | 223 | untouched (0.00 s, 3 MiB) | packed-exact (0.03 s, 14 MiB) | 30 s, 512 MiB |
| `objstm_selfref.pdf` | xref stream saying an object lives inside object stream 2: itself | 165 | untouched (0.00 s, 3 MiB) | packed-exact (0.05 s, 14 MiB) | 30 s, 512 MiB |
| `objstm_loop.pdf` | object stream that contains itself and /Extends itself | 401 | untouched (0.00 s, 3 MiB) | packed-exact (0.04 s, 14 MiB) | 30 s, 512 MiB |
| `million_objects.pdf` | 1,000,000 tiny objects reachable from the catalog | 52,778,080 | smaller (26.89 s, 523 MiB) | packed-exact (119.83 s, 471 MiB) | 282 s, 805 MiB |

Passed 28 of 28 runs.

### Failures to fix

None.

### Notes on the failures

Run on build integration-8892b9e (preflate-rs gone, format version 7, the licence gate in): the two failures
below are closed, with no change to the engine's bounds. `flate_bomb.pdf` through pack now ends as
packed-exact in 1.0 s of CPU with a peak of 260 MiB (the failed inflate of the bomb stops at `max_stream`,
256 MiB, and the stream is then stored as it is, which is what optimize already did); the spec's fallback, a
decoded-bytes budget for pack, was therefore not added, since it would store some legitimate streams raw at
low effort and make those archives larger. `cabac` left with preflate-rs, so `supply` reports 0 disallowed
licences; `deny.toml` and `cargo deny check licenses advisories` keep it that way. What stays open from the
observations below is in `docs/tasks/` (the model's tables sized by the declared length; the archive's four
copies of every decoded byte, which also keeps the two large scans out of the fuzz seeds).

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

## Known limits

- Normal PDF: JPEG images only get the optional Flate layer; JPEG 2000 and JBIG2 images are not improved.
- Speed: maximum effort is slow on large image-heavy files (545 s for the slowest test file); the quick
  setting is faster but still too slow for some of them. A time budget is needed before this runs in a browser.
- peyaji format: very repetitive huge images and data that is already compressed (Lepton output, JPEG 2000)
  are where xz still does better; the model needs a long-repeat path and a "store as is" path for those.
- Encrypted PDFs are left as they are in both modes (they would need decrypting and re-encrypting exactly).
- Untouched files: see the table above; they are returned unchanged on purpose.
