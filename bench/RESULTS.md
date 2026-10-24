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

## Effort levels

The README's effort table: two runs of `bench/benchmark.py time` on the 120-file sample, normal PDF, one
thread. CPU time is summed over the files; output is the total size as a share of the input on the 90 files
the engine rewrites.

| Effort | CPU s, first measured | CPU s, current code | Output |
| ---: | ---: | ---: | ---: |
| 0 | 16.1 | 16.8 | 87.02% |
| 5 | 23.0 | | 86.70% |
| 10 | 40.7 | | 86.11% |
| 15 | 111.2 | | 85.74% |
| 19 | 183.2 | | 85.59% |
| 20 | 243.8 | | 85.43% |
| 50 | 333.8 | 175.7 | 85.17% |
| 100 | 983.1 | 511.7 | 85.01% |
| qpdf 11.9.0, strongest lossless setting | 25.3 | | 87.23% |

- First measured in October 2026 for the change that added the fast path (#1), on a 4-core 2.1 GHz Xeon cloud
  machine. Efforts 0 to 19 are commit 52a91f0; 20, 50 and 100 are 20f8e69 (at effort 20 its 120 outputs are
  52a91f0's byte for byte). Calibration: 2.675 s (efforts 0, 5 and 10), 2.674 s (15 and 19), 2.870, 2.819 and
  2.845 s (20, 50 and 100). Before that change, effort 0 took 195.5 s (main 066b65e, calibration 2.972 s).
  qpdf's 87.23% counts the 85 outputs `verify_pdf.py` finds the same as their input apart from the trailer's
  /ID, and the other 5 files at their input size.
- The current code measured again in October 2026 at efforts 0, 50 and 100 only: the lightweight change (#15)
  merged with main c8e9f1a, the code of main 37bd3f3, on a 4-core cloud machine, with c8e9f1a timed beside it
  on another core and other work on the other two. Calibration 3.315 s, about 1.2 times the first run's, so
  the two columns compare only roughly; c8e9f1a, timed beside it (calibration 3.169 s), took 16.6, 178.9 and
  512.0 s. The outputs total the first run's bytes exactly at each effort (71,470,799, 70,493,915 and
  70,408,873), and the 120 outputs are byte-identical to c8e9f1a's.
- Calibration is the CPU time of `xz -9e` on 4 MiB of fixed text, the smallest of three, taken at the start of
  each run (section 6).

## Archive format version 9

The model's logistic tables (squash and stretch) and its adaptive probability maps were written again from
their formulas, by implementers kept from the PAQ code the earlier ones followed, so the coded bytes changed
and the archive format went from version 8 to 9 (`src/cm/mod.rs`). `bench/benchmark.py time` on
`--set sample120` (120 files, 78,313,418 bytes), `--modes archive --efforts 0,50,100 --threads 1`, version 8
built from 74114cf against version 9 (1b43830), one run of each after the other on a 4-core cloud machine
(calibration 4.212 s before the version 8 run, 4.237 s before version 9):

| Effort | Version 8 | Version 9 | Change | CPU, version 8 | CPU, version 9 |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 48,015,564 | 47,883,482 | -0.28% | 315.2 s | 332.4 s |
| 50 | 47,164,857 | 47,013,243 | -0.32% | 442.7 s | 470.3 s |
| 100 | 47,164,857 | 47,013,243 | -0.32% | 422.1 s | 445.9 s |

- Every archive restored byte for byte, and peak memory stayed at 262 MB. Efforts 50 and 100 give the same
  archives, as before.
- Two files grew, which `benchmark.py compare` reports as a failure: ocrmypdf's
  `tests__resources__2400dpi.pdf`, 73,708 to 73,801 bytes (+93, +0.13%) at every effort, and pdf.js's
  `issue5567.pdf`, 2,169,428 to 2,169,612 bytes (+184, +0.008%) at efforts 50 and 100 (at effort 0 it is 8
  bytes smaller). Every other file is smaller at every effort; the largest saving is pypdf's
  `009-pdflatex-geotopo__GeoTopo.pdf`, 2,195,852 to 2,174,038 bytes.
- CPU: the two runs above, one after the other with other work on the machine, differ by 5 to 6%. Packing the
  37 files that take over a second with both builds in turn, file by file and pinned to one core, gives 316.0
  against 315.7 s at effort 0 (-0.1%) and 426.9 against 439.0 s at effort 100 (+2.8%).
- Of the other designs measured on 30 of these files (`--set sample120-size30`), steps of 1/32 or 1/128
  instead of 1/64, or the same ends with steps rounded from a half, gave larger totals over efforts 0 and 100.
  Maps stepping toward 0 and 65535, rounded from a half and with their output clamped to 1 to 4095, came out 8
  and 11 bytes smaller; the ends used (16 and 65520) keep the output in that range with no clamp.
- The normal PDF is unchanged: the MQ decoder, written again from T.88 Annex E.3, reads back what the engine
  writes as before, and the 19 corpus files with CCITT images give the same outputs at efforts 0 and 100.
- The whole-set archive figure in section 3 is from earlier builds and was not measured again.

## Lightweight

The download and the start-up are part of the time a person waits (`docs/tasks/lightweight.md`). Measured on
a 4-core cloud machine in October 2026 with Rust 1.99.1 (the compiler CI uses), binaryen's wasm-opt 108 and
Node 22: the sizes and the memory on this work merged with main at c8e9f1a (main with the Flate fixed costs,
#2, the object-stream fix, #13, and the pure gains, #14), in an Ubuntu 24.04 x86_64 container like CI's image
(built from #14's 8e6a108 and this work merged with it: their code is c8e9f1a's but for comments);
the choice of the module's build and its start-up on main at d093ab4 (the fast tier, #1, and the Flate parse,
#9) with PR #8. Main's later changes add code to the module (Sizes, below); the start-up is measured again by
CI on every push (Start-up, below).

### Sizes

`python3 -I bench/sizes.py measure [--features SET]` builds each part and records its size in
`bench/sizes.json`: the native tool and the C library with the release profile, the WebAssembly module the
way the npm package builds it (cargo's `wasm` profile, opt-level 3, then `wasm-opt -O1`; see below). Gzip is
level 9. Each part of the engine is a Cargo feature (docs/reference.md, "Building only what a product needs"), and a set
is built with `--no-default-features`, so that it is exactly what is named: "default" is a plain build, every
part of the engine (`archive`, `jpeg`, `fax` and `encryption`); "web" is the web module's set, every part but
the archive; "existing" is the archive and Lepton and nothing else; "default,existing" is every part and
Lepton, which is what "existing" built before the parts were features. "Before" is the same build without
this work (main at c8e9f1a), where the engine had no parts and "existing" was every part and Lepton, so its
sizes are the "Before" of "default,existing".

| Build | Feature set | Raw bytes | Gzip bytes | Before: raw, gzip | Gzip change |
| --- | --- | ---: | ---: | ---: | ---: |
| WebAssembly module | web | 710,751 | 267,937 | | |
| WebAssembly module | default | 1,038,544 | 383,303 | 1,027,366, 379,342 | +1.0% |
| WebAssembly module | default,existing | 1,451,939 | 519,213 | 1,440,744, 513,825 | +1.0% |
| WebAssembly module | existing | 1,197,498 | 424,699 | | |
| Native tool `gpress` | web | 1,508,088 | 675,533 | | |
| Native tool `gpress` | default | 2,112,816 | 936,953 | 2,100,656, 930,455 | +0.7% |
| Native tool `gpress` | default,existing | 2,729,704 | 1,172,549 | 2,718,240, 1,165,193 | +0.6% |
| Native tool `gpress` | existing | 2,269,816 | 954,665 | | |
| C library, shared (`libgpress_c.so`) | web | 1,379,032 | 617,707 | | |
| C library, shared (`libgpress_c.so`) | default | 1,927,608 | 853,947 | 1,915,648, 846,676 | +0.9% |
| C library, shared (`libgpress_c.so`) | default,existing | 2,545,248 | 1,089,217 | 2,533,096, 1,083,769 | +0.5% |
| C library, shared (`libgpress_c.so`) | existing | 2,162,480 | 906,551 | | |
| C library, static (`libgpress_c.a`) | web | 9,846,120 | 2,095,285 | | |
| C library, static (`libgpress_c.a`) | default | 10,769,696 | 2,367,811 | 10,749,288, 2,361,198 | +0.3% |
| C library, static (`libgpress_c.a`) | default,existing | 11,850,824 | 2,638,632 | 11,830,272, 2,630,541 | +0.3% |
| C library, static (`libgpress_c.a`) | existing | 11,238,056 | 2,435,855 | | |

With every part built in, this work (the features, and the memory changes of L3) adds 0.3% to 1.0% of gzip
to each build: 3,961 bytes to the module and 6,498 to the tool. The web module is 327,793 bytes (31.6%)
smaller than the whole one, 115,366 bytes (30.1%) of gzip; the next section says where the bytes go.

The target for the web module is 400 KB gzip (400,000 bytes): at 267,937 bytes it is 132,063 bytes (33%) under
it, and the whole module is 16,697 bytes under it. Built at opt-level "s", before the choice below, the web
module was 185,288 bytes of gzip (on f34ba33, before main's later changes): opt-level 3 added 24% there.
Lepton adds 135,910 bytes of gzip to the module with every part. The static library carries Rust's standard
library whole, so it is far larger than what a program linked with it gets. The module grows with main: the
whole module was 305,524 bytes of gzip at d093ab4, the archive model (#3) added 33,893 bytes, the decode limit
(#12) 428, and #2, #13 and #14 together 39,497 (379,342 at c8e9f1a, without this work). The web module grows
with main's normal-PDF code the same way: it was 229,312 bytes of gzip on main at dcd8a8b with this work.

The budget: CI's WebAssembly job runs `python3 -I bench/sizes.py check`, which builds the web module as the
package does and fails when its gzip size is more than 5% over the recorded one (281,333 bytes today) or over
400,000 bytes. A growth that is wanted is recorded with `measure --features web`. When the module is more than
5% under the record (under 254,540 bytes today), the check passes with a warning to record the new size, since
the 5% is counted from the record and would otherwise allow more. The whole module (`dist/gpress.wasm`) has no
budget of its own; `measure --features default` records it with the other sets.

### What each feature costs

The builds of the Sizes table (the module as the package builds it, the native tool with the release profile;
gzip level 9) for each set of parts:

| Build | Module raw | Module gzip | Tool raw | Tool gzip |
| --- | ---: | ---: | ---: | ---: |
| Every part (default) | 1,038,544 | 383,303 | 2,112,816 | 936,953 |
| `web`: every part but `archive` | 710,751 | 267,937 | 1,508,088 | 675,533 |
| Every part but `jpeg` | 987,579 | 363,779 | 2,029,664 | 896,619 |
| Every part but `fax` | 952,569 | 348,107 | 1,889,648 | 832,440 |
| Every part but `encryption` | 920,646 | 343,616 | 1,960,376 | 863,308 |
| None (`--no-default-features`) | 537,008 | 204,635 | 1,222,312 | 542,657 |
| Every part and `existing` | 1,451,939 | 519,213 | 2,729,704 | 1,172,549 |

Naming the three parts (`--features jpeg,fax,encryption`) instead of `web` compiles the same code: a module 1
byte larger raw and 15 bytes larger gzip, and 624 bytes less tool (symbol names).

What a part costs is the bytes the build with every part has over the build with every part but that one
(for Lepton, every part and `existing` over every part). "Sample gain" is what the part saves on the
normal-PDF output of the 120-file sample at effort 0: the native tool's total output without the part, less
its total with every part (71,470,799 bytes from 78,313,418). One run each, one process per file, `gpress
optimize IN OUT --effort 0 --threads 1`, with main's fast tier at effort 0: on f34ba33, the start of this work,
and again on this work merged with main at c8e9f1a, which gave the same bytes for every build; the outputs with
every part are byte for byte main's (Memory, below).

| Feature | Module raw | Module gzip | Tool raw | Tool gzip | Sample gain, bytes | Files changed |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `archive` | 327,793 | 115,366 | 604,728 | 261,420 | (not used by normal PDF) | |
| `jpeg` | 50,965 | 19,524 | 83,152 | 40,334 | 27,906 (0.04%) | 10 |
| `fax` | 85,975 | 35,196 | 223,168 | 104,513 | 220,632 (0.31%) | 4 |
| `encryption` | 117,898 | 39,687 | 152,440 | 73,645 | 2,758 (0.004%) | 1 |
| `existing` | 413,395 | 135,910 | 616,888 | 235,596 | (archive only) | |

The four costs add up to more than the 501,536 raw bytes (178,668 gzip) between every part and none, because
some code is there only with two parts: the archive's fax parts and its decrypted streams.

- `jpeg` saves 27,906 bytes on 10 files (freeculture.pdf 12,198, issue19971.pdf 8,524); one file,
  issue13931.pdf, comes out 1,479 bytes larger with the step than without it. It costs 19,524 bytes of gzip in
  the module, about two thirds of what it saves on the whole sample. Kept.
- `fax` saves 220,632 bytes on 4 files (freeculture.pdf 191,828, the OCRmyPDF ccitt.pdf 27,583) for 35,196
  bytes of gzip: worth its cost. Kept.
- `encryption` costs the most of the three, 39,687 bytes of gzip in the whole module, and saves the least: the
  sample has three encrypted files, one of which the engine rewrites (issue17215.pdf, 2,758 bytes smaller);
  the other two are left untouched in every build (one needs a password, one has a handler the engine doesn't
  take). On this sample it costs far more than it saves. It stays in the default and in `web`: without it an
  encrypted file the engine can rewrite is left untouched, so the web compressor would give a different
  answer from the native tool for those files, and the web module is 33% under its budget with it.
- `archive` is most of the difference between the two modules (115,366 bytes of gzip), and Lepton (`existing`,
  135,910 bytes) would cost more than the three normal-PDF parts together (94,407); neither is in `web`.

### Choosing the module's build

The brief's rule (L2): opt-level "z" or "s", whichever is smaller and not more than 10% slower on the sample.
Every build of the module was run in Node with `bench/wasm_speed.mjs` on the 30 files of
`bench/sets/sample120-size30.txt` (the 120-file sample sorted by size, every fourth file: 657 B to 6.3 MB),
one process per run pinned to one core, timing the CPU of the optimize calls: 22 runs of each build at effort
0, 8 at effort 50 and 8 at effort 100, in a rotating order on four cores, among them batches of four runs
started together. The machine was shared with other work, so a build's runs moved by up to 14% during the
day (its later runs 4% to 15% slower than its earlier ones); the ratios between builds run at the same time
did not. So "vs 3" is the median ratio over every pair of runs that overlapped in time with a run of an
opt-level-3 build (the first three rows), and the CPU columns are medians of all runs. "First call": compile,
instantiate and the first optimize of the one-page PDF (Start-up, below), median of fresh Node processes:
21 on one core (`taskset -c 3`, the other three busy with other work) and 11 on two idle cores (`taskset -c
2-3`, as GitHub gives a private repository's runner). Opt-level 3, 2, "s" and "z" are the `wasm` profile at
that opt-level (symbols stripped, `panic = "abort"`, which changes nothing on wasm32-unknown-unknown: that
target aborts on a panic anyway); "dependencies z" builds the other crates at "z".

| opt-level | wasm-opt | Raw bytes | Gzip bytes | Effort 0, CPU s | vs 3 | Effort 50, CPU s | vs 3 | Effort 100, CPU s | vs 3 | First call, effort 0, one core / two cores, ms | Effort 100, one core / two cores, ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 (release profile, as main builds it) | none | 961,453 | 328,536 | 8.23 | -2.1% | 94.3 | +0.4% | 271.0 | -0.1% | 45.2 / 33.9 | 304.2 / 167.0 |
| 3 | none | 879,083 | 313,649 | 8.21 | +0.8% | 93.6 | -0.8% | 271.0 | -0.5% | 45.0 | 296.7 |
| **3 (the package)** | **-O1** | **817,696** | **305,524** | **8.45** | **+2.2%** | **94.1** | **+0.4%** | **273.6** | **+0.7%** | **43.1 / 34.8** | **303.9 / 177.8** |
| 3, dependencies z | -O1 | 814,631 | 304,847 | | | | | | | 43.4 | 302.7 |
| 2 | -O1 | 737,185 | 285,328 | 9.55 | +15.3% | 98.9 | +5.0% | 291.4 | +6.5% | 41.6 / 30.2 | 224.0 / 130.8 |
| s | none | 709,868 | 255,097 | 9.65 | +15.2% | 113.5 | +21.5% | 337.6 | +23.7% | 37.4 | 170.6 |
| s | -O1 | 652,660 | 247,366 | 9.05 | +8.1% | 116.0 | +22.3% | 339.8 | +25.9% | 36.6 / 26.6 | 164.8 / 128.8 |
| z | -O1 | 575,462 | 219,249 | 14.58 | +81.0% | 179.4 | +89.2% | 529.8 | +92.9% | 39.9 | 249.6 |

wasm-opt's levels, gzip bytes (raw bytes in brackets):

| opt-level | none | -O1 | -O2 | -Os | -Oz |
| --- | ---: | ---: | ---: | ---: | ---: |
| 3 | 313,649 (879,083) | **305,524** (817,696) | 309,330 (808,786) | 308,555 (802,131) | 308,022 (794,936) |
| 2 | 293,241 (795,650) | **285,328** (737,185) | 291,133 (729,527) | 291,393 (723,300) | 291,164 (715,461) |
| s | 255,097 (709,868) | **247,366** (652,660) | 266,536 (637,606) | 264,738 (631,565) | 264,435 (621,454) |
| z | 226,919 (636,219) | **219,249** (575,462) | 235,670 (557,564) | 234,924 (551,899) | 234,391 (542,313) |

Every output of every run was byte-identical to the native tool's on the same code (`--threads 1`): 4,620
outputs at effort 0, 1,680 at 50 and 1,680 at 100.

- Neither size level meets the rule. "s" is 19% smaller in gzip than opt-level 3 and 8% slower at effort 0,
  but 22% slower at effort 50 and 26% at effort 100 (at d093ab4; on #8's head it was 13.9% slower at effort
  50, still over the rule); "z" is 28% smaller and 1.8 to 1.9 times slower at every effort. Opt-level 2 is not
  a size level, and is 15% slower at effort 0 anyway. So the profile keeps opt-level 3 and takes only what
  costs little or no speed: stripped symbols (4.5% less gzip than the release profile; the module is the same
  bytes without `panic = "abort"`) and wasm-opt (2.6% less again). Together they cost a little time at the
  higher efforts: on #8's head, 4 paired rounds of the package's module against main's release module (cores
  swapped, the same outputs) gave -0.8% at effort 0, +2.4% at effort 50 and +2.9% at effort 100. An earlier
  measurement, on main before its Flate work (A5, A6 and B3 of the plan, then the fast tier), had "s" 3% to 5%
  slower at effort 0, inside the rule, and this branch used "s" until main's changes widened the gap.
- wasm-opt: -O1 leaves the smallest gzip at every opt-level. -O2, -Os and -Oz take more off the raw bytes (-Oz
  10% at opt-level 3, against 7% for -O1) but less off the gzip, which is what a browser downloads, so the
  package build runs -O1 (the brief named -Oz; it is one word in `crates/gpress-wasm/js/build-wasm.mjs`).
  Without wasm-opt on PATH the build uses cargo's module as it is and says so.
- What opt-level 3 costs: 58 KB more to download than "s" (305,524 against 247,366 bytes of gzip here, at
  d093ab4; 269,742 for "s" on main at f5f9701, against 339,845 for opt-level 3 on main at dcd8a8b), and a
  slower first call at the default effort, where V8's optimizing compiler has more code to work on (below):
  304 against 165 ms on one core, 178 against 129 on two idle cores. At effort 0 the first call is 6 to 8 ms
  slower (43.1 against 36.6 ms on one core, 34.8 against 26.6 on two cores).

### Start-up (the WebAssembly module in Node)

The target (L4): compiling the module, making an instance and optimizing a one-page PDF of a few KB take at
most 50 ms in all, measured in CI's Node test. `bench/wasm_startup.mjs` measures it from the module's bytes in
memory: compile, instantiate, the first `optimize` call, and a second call in the same process (the same work
once the code is compiled). Each sample is a fresh Node 22.22 process, because V8 reuses compiled code within
a process; the modules are measured in turn, round after round, and the tables give medians. The PDF is 3,851
bytes, made by `crates/gpress-wasm/js/test/onepage.mjs`: a Flate content stream of text in two fonts, and the
two fonts with their widths and descriptors (named, not embedded: the file holds no font program; real files
that do are below). It comes out at 2,794 bytes at effort 0, 2,777 at effort 50 and 2,767 at effort 100, the
same bytes as the native tool. CI runs the same measurement in `crates/gpress-wasm/js/test/startup.test.mjs`:
one process not counted, then 7 at effort 0 and 7 at the default effort (100), and it prints the medians and
the distance to 50 ms. It fails only above 150 ms at effort 0 and 300 ms at the default effort, since a busy
machine or one with fewer cores can take two or three times as long; `GPRESS_STARTUP_LIMIT_MS` raises the
limits on such a machine (CI sets nothing).

**In CI** (GitHub's two-core runner, the WebAssembly package job), six runs with this change: medians of
26.3, 32.3, 46.2, 49.2, 33.4 and 31.1 ms at effort 0 (compile 3.8 to 9.1, instantiate 0.3, first call 22.2 to
39.7; single processes 24 to 62 ms), so the target is met in every run, but in the slowest with only 0.8 ms to
spare, and 136.9, 180.9, 180.0, 263.6, 174.1 and 183.3 ms at the default effort. Runners differ from one run
to the next by about that much: the two slow runs at effort 0 took 9 ms to compile, the others about 5. Before
the fast tier (#1), the same test took a median of 133 ms at effort 0 there.

The web module goes through the same test in `npm run test:web`. With the parts as features, one run of each
on the 4-core machine with other work running (`GPRESS_STARTUP_LIMIT_MS=5000`, so that the load could not fail
it): 22.7 ms at effort 0 and 112.5 ms at the default effort for the whole module, 28.5 and 135.9 ms for the
web module. One run on a busy machine says little about the difference; both are under 50 ms at effort 0.

On this machine, at d093ab4 (the rows of the table above; one core: core 3 alone, 21 processes, the other
cores busy with other work; two cores: cores 2 and 3, both idle, 11 processes; `--quiet` kept a round only
when its cores had been idle for a second before and after it):

| Module | Cores | Effort 0: compile | instantiate | first call | **total** | second call | Effort 100: **total** | second call |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| the package (3, -O1) | one | 8.5 | 0.2 | 34.0 | **43.1** | 13.3 | **303.9** | 88.3 |
| the package (3, -O1) | two | 6.4 | 0.3 | 26.8 | **34.8** | 4.5 | **177.8** | 72.6 |
| main's build (3, release profile) | one | 9.0 | 0.2 | 34.5 | **45.2** | 11.8 | **304.2** | 85.4 |
| main's build (3, release profile) | two | 6.6 | 0.3 | 26.1 | **33.9** | 5.0 | **167.0** | 73.9 |
| 2, -O1 | one | 8.0 | 0.2 | 33.4 | **41.6** | 12.9 | **224.0** | 70.5 |
| 2, -O1 | two | 6.4 | 0.3 | 22.9 | **30.2** | 5.3 | **130.8** | 66.5 |
| s, -O1 | one | 7.3 | 0.2 | 29.2 | **36.6** | 10.5 | **164.8** | 88.7 |
| s, -O1 | two | 6.1 | 0.3 | 18.8 | **26.6** | 6.1 | **128.8** | 78.6 |
| z, -O1 | one | 7.0 | 0.2 | 32.2 | **39.9** | 15.8 | **249.6** | 178.2 |

Single processes vary widely on a shared machine: on one core the package's effort-0 totals ranged from 36
to 68 ms. The native tool takes 7 to 8 ms at effort 0 and 54 to 66 ms at effort 100 on the same file (medians
of three sets of 21 runs on core 3, process start included), so the module's start-up at effort 0 takes five
to six times the native tool's whole run, and three to six times at effort 100.

Where the time goes, the package's module:

- **Compile, 4 to 9 ms.** V8 decodes and validates the 859 KB of code; it compiles each function only when it
  is first called. `Gpress.compile` once, then `Gpress.load(module)` for each instance, skips this step for
  every instance after the first.
- **Instantiate, 0.2 to 0.3 ms.** Nothing is built at start-up: 64 KB of data and 18 pages (1.13 MiB) of
  memory, no start function; every table of the engine is made when a call needs it.
- **The first call.** It compiles the functions it reaches with V8's quick compiler, Liftoff, and runs in
  that code while the optimizing compiler (TurboFan) works on the hot functions in the background; a function
  switches to optimized code only when it is next called. At effort 0 the fast tier's work is small, so most
  of the call is the cost of new code: 27 to 34 ms here (22 and 27 in CI), against 5 to 13 ms for the same
  call once compiled. At effort 100 the call is longer and TurboFan has more to do; with one core it competes
  with the work: the package's first call at effort 100 takes 304 ms on one core against 178 ms on two, and
  "s", with less code to compile, 165 and 129 ms.

Tables sized to the input: at effort 0 the fast tier's hash tables and window are sized to the stream
(`table_bits` and `window_slots` in `src/flate/fast.rs`). At efforts 20 and above the match finder still
makes its two largest-case tables (`head`, 256 KiB, and `son`, 512 KiB, filled with NIL) on every
`find_matches` call, even for a 60-byte stream; A4 of the plan (`docs/tasks/flate-fixed-costs.md`) reuses
them and clears only what a search reads, so this branch leaves `src/flate/` to it. Everything else on the
optimize path is sized to its input: the parse's cost arrays, the match and symbol lists, the bit writer, the
inflate output. The archive is not on this path, and its model's tables are fixed (`src/cm/`, `src/pack/`:
the archive briefs).

The test PDF holds a single Flate stream. Real one-page PDFs of 3 to 6 KB that embed a font program (a Flate
`FontFile2` or `FontFile3` of 0.9 to 4.2 KB), from the corpus's `pdfjs/` folder, take about as long at effort
0 (`--pdf`, 9 processes on core 3, the modules of the table above, medians in ms):

| File | Bytes | Font program | Output bytes | the package: **total** | second call | s, -O1: **total** | second call |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| the test PDF | 3,851 | none | 2,794 | **48.5** | 12.4 | **39.7** | 13.9 |
| `Embedded_font.pdf` | 3,323 | 909 bytes | 2,444 | **51.5** | 2.5 | **33.8** | 7.6 |
| `TrueType_without_cmap.pdf` | 3,989 | 1,916 bytes | 2,944 | **42.3** | 10.9 | **36.5** | 7.6 |
| `issue7492.pdf` | 4,619 | 2,191 bytes | 3,878 | **43.9** | 8.2 | **39.2** | 11.3 |
| `issue5256.pdf` | 6,136 | 4,244 bytes | 5,265 | **43.7** | 7.5 | **42.0** | 11.5 |

The target, at effort 0 on the test PDF, is met in CI (26.3 to 49.2 ms), on two idle cores here (34.8 ms)
and on one core here (43.1 ms, and 48.5 in the busier run of the table above; single processes took up to 68
ms). The default effort does not reach 50 ms, and is not meant to: the native tool itself takes 54 to 66 ms
there.

### Memory

The target (L3): at effort 0, a peak of at most 32 MiB (32 x 2^20 bytes) plus twice the input. The peak is the
resident set that `wait4` gives (bench/measure.c) for `optimize` with 1 thread, one process per file, on the
120-file sample, measured by `benchmark.py space` (`--verify 0`) in an x86_64 container of a 4-core cloud
machine (16 GB), every feature on (`--features existing`). Two builds, whose runs were started together:

- before: main at c8e9f1a (with the Flate fixed costs, #2, the object-stream fix, #13, and the pure gains, #14);
- after: this work merged with it.

Three runs of each at effort 0 and one at effort 100. Each file's peak is the median of its runs. Every output
of the after build is byte for byte main's (the same SHA-256): 120 files in every run at efforts 0 and 100.

Each build fitted as a fixed cost plus bytes per input byte, by least squares and as the lowest line (on
average over the files) with every file on or under it (MiB; "at slope 2" is the fixed cost every file would
need with the target's 2 bytes per input byte):

| Effort | Build | Median | Peak (file, MiB in) | Least squares | Every file under | At slope 2 | Files over the target |
| --- | --- | ---: | --- | --- | --- | ---: | ---: |
| 0 | before | 4.6 | 80.0 (issue19517.pdf, 5.97) | 5.14 + 6.61 x input | 12.88 + 11.23 x input | 68.01 | 3 |
| 0 | after | 4.5 | 50.9 (GeoTopo.pdf, 5.07) | 5.23 + 4.77 x input | 12.96 + 8.92 x input | 40.79 | 2 |
| 100 | before | 5.3 | 109.6 (issue12963.pdf, 0.58) | 10.84 + 8.55 x input | 109.60 + 0.00 x input | 108.45 | 18 |
| 100 | after | 5.3 | 81.3 (issue12963.pdf, 0.58) | 9.80 + 5.80 x input | 81.30 + 0.00 x input | 80.15 | 12 |

The fixed cost stays small: on the empty one-page PDF (420 bytes, left untouched; the smallest wall time of 5
runs) the peak is 4,232 to 4,332 KB with main and 4,028 to 4,184 KB with the after build at effort 0 (one
measurement in each run), and 5,092 and 5,196 KB at effort 100.

The target is not met at effort 0: 2 files are over the line after the changes (3 before), by 8.8 and 3.5
MiB. The files over it with either build, in MiB ("runs": the lowest and the highest of the three):

| File | MiB in | Line | Before | Before runs | After | After runs | After - line |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| GeoTopo.pdf | 5.07 | 42.1 | 54.5 | 54.5 to 54.6 | 50.9 | 50.9 to 51.0 | +8.8 |
| issue9262_reduced.pdf | 5.28 | 42.6 | 61.5 | 61.4 to 61.5 | 46.0 | 45.9 to 46.0 | +3.5 |
| issue19517.pdf | 5.97 | 43.9 | 80.0 | 79.9 to 80.0 | 27.9 | 27.8 to 28.0 | -16.0 |

Two files that were over the line earlier in this work are under it with both builds: issue12841_reduced.pdf
37.6 MiB with main and 32.0 after (line 43.2), issue9940.pdf 15.1 and 14.5 (line 32.8). The largest increase
at effort 0 is 0.7 MiB, on a file far under its line (issue6931_reduced.pdf, 12.8 to 13.4, line 32.1); no
other file is more than 0.3 MiB above main.

At effort 100 the 12 files over the line after the changes are issue12963.pdf (+48.2 MiB), GeoTopo.pdf
(+31.7), issue9940.pdf (+21.8), 2400dpi.pdf (+21.4), base64image.pdf (+14.9), issue1905.pdf (+14.5),
issue13931.pdf (+12.2), issue9262_reduced.pdf (+8.0), issue12810.pdf (+5.0), issue12841_reduced.pdf (+3.3) and
the two multipage files (+0.8 each). The changes took enormous.pdf (68.7 to 5.8 MiB), issue19517.pdf (83.1 to
28.2), PDFJS-9279-reduced.pdf (40.0 to 33.9), issue6931_reduced.pdf (33.0 to 30.8), freeculture.pdf (37.7 to
36.2) and issue19971.pdf (35.3 to 35.2, on its line before) under it. Other large savings at effort 100: the
two multipage files (70.2 and 70.6 to 34.9), issue12841_reduced.pdf (75.8 to 46.5), issue12963.pdf (109.6 to
81.3), issue9262_reduced.pdf (70.3 to 50.6) and issue13931.pdf (59.1 to 44.8). No file is more than 0.4 MiB
above main there (issue5804.pdf and issue17215.pdf, about 5 MiB each).

Peaks move from one run to the next with the same build and input, but little on this machine: over the three
runs at effort 0 the largest spread is 0.4 MiB with the after build (bug1308536.pdf) and 0.5 MiB with main. On
the machine of the earlier measurements (below) spreads of 3 MiB at effort 0 and 10 MiB at effort 100 were
seen on one file, so a difference of a few MiB in one run on one file says little, and the effort-100 columns
are one run each. The CPU times of these runs, summed over the sample: 16.0 to 16.3 s with main and 16.0 to
16.4 s with the after build at effort 0 (two builds running at once on a machine with other work), 511.2 s with
main and 509.3 s with the after build at effort 100. Timed together, main on core 0 and the after build on core
1 at the same time while other work used the other two (`benchmark.py time`, one thread, efforts 0, 50 and 100,
normal PDF and archive), the two builds gave the same 720 outputs, with CPU from -1.8% to +0.7% per setting, and
`benchmark.py compare` passes.

What each change saved. These and the rest of this section were measured before main's fast tier (#1) was
merged, in the Linux aarch64 container of a shared 4-core machine (2 GiB), when effort 0 ran the strong
encoder's quick settings. The commits from before the merge were measured one at a time before #6 was
merged, on opt-lightweight e220e03 ("start"), each column adding one commit; in KB, the median of 3 runs, 1
thread (one run for GeoTopo.pdf and issue12963.pdf), every output the same. With #6 the match finder holds less, so
the same commits save different amounts today; the tables above are what the whole branch gives now.

| File | Effort | start | +23594ec | +e999065 | +058ac94 | +a54f45c | +d782526 | +8a6906e | +d75ddc0 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| issue12841_reduced | 0 | 83,524 | 59,364 | 59,488 | 59,532 | 59,320 | 59,400 | 53,652 | 69,116 |
| issue19517 | 0 | 85,016 | 85,036 | 84,892 | 34,764 | 34,632 | 28,548 | 28,648 | 28,632 |
| enormous | 0 | 70,168 | 70,216 | 70,344 | 70,352 | 5,964 | 5,608 | 5,512 | 5,624 |
| issue6286 | 0 | 48,844 | 38,060 | 38,072 | 38,052 | 36,276 | 36,280 | 37,140 | 40,412 |
| PDFJS-9279-reduced | 0 | 42,704 | 39,648 | 39,592 | 39,568 | 39,616 | 39,592 | 36,844 | 39,932 |
| issue6931_reduced | 0 | 37,752 | 37,096 | 34,604 | 34,788 | 35,604 | 35,248 | 35,256 | 33,676 |
| base64image | 0 | 43,364 | 44,456 | 42,456 | 42,420 | 42,108 | 42,112 | 41,356 | 41,148 |
| pikepdf pdfa multipage | 0 | 79,080 | 79,312 | 79,176 | 79,152 | 36,120 | 36,016 | 35,864 | 37,860 |
| ocrmypdf multipage | 0 | 79,284 | 79,316 | 79,260 | 79,280 | 36,080 | 36,028 | 35,952 | 37,588 |
| issue9940 | 0 | 57,212 | 57,036 | 53,348 | 53,396 | 54,788 | 59,460 | 54,008 | 57,644 |
| issue9262_reduced | 0 | 92,156 | 80,036 | 80,004 | 79,944 | 70,652 | 60,444 | 55,108 | 71,120 |
| GeoTopo | 0 | 90,628 | 90,508 | 83,608 | 83,084 | 81,792 | 83,428 | 81,108 | 84,024 |
| issue19517 | 100 | 84,748 | 84,848 | 84,820 | 34,768 | 34,680 | 28,628 | 28,588 | 28,620 |
| enormous | 100 | 69,976 | 70,064 | 70,092 | 70,124 | 5,968 | 5,616 | 5,648 | 5,668 |
| issue13931 | 100 | 81,380 | 81,368 | 66,260 | 66,292 | 66,668 | 66,664 | 66,648 | 66,672 |
| issue12963 | 100 | 177,772 | 177,736 | 152,340 | 151,280 | 148,452 | 148,312 | 152,508 | 152,508 |

- **23594ec**: with one thread, a stream's 1 MiB segments are optimised one at a time and each plan is written
  before the next segment is optimised (eight segments were optimised, then written): issue12841_reduced
  83,524 to 59,364 KB, issue9262_reduced 92,156 to 80,036, issue6286 48,844 to 38,060. With more threads it
  made a group as many segments as threads; 916fbfb puts that back to eight (below).
- **e999065**: the image predictor candidates read the decoded samples where they are (the plain candidate was
  a whole copy, held through the full encode): issue12963 at effort 100 177,736 to 152,340 KB, issue13931 at
  effort 100 81,368 to 66,260, GeoTopo 90,508 to 83,608. Taken alone it makes images slower: issue13931 at
  effort 100 takes 24.9 s of CPU time instead of 21.7 (+15%, 4 alternating rounds against 23594ec) with the
  same instructions (+0.001%, cachegrind); the extra time is in the match search over the candidates, and
  moving glibc's mmap threshold does not change it. The time is back from a54f45c on (2 rounds at each commit:
  23594ec 22.4 s, e999065 25.6, 058ac94 26.3, a54f45c 20.8, d782526 21.1, 8a6906e 21.4, d75ddc0 20.6), and the
  whole branch is +0.3% on this file (below). So e999065 should not be taken without a54f45c.
- **058ac94**: RunLength output is sized by a first pass instead of doubling as it grows: issue19517 85,036
  to 34,764 KB.
- **a54f45c**: the effort plan counts each stream's decoded size, holding only the end of the last stage's
  output that later matches can reach (32 KiB, up to 160 KiB between cuts; it decoded every stream whole, and
  the stream was then decoded again to be encoded), and a stream that does not decode within the 64 MiB limit
  is kept at once, at every effort, instead of being decoded up to the limit only to fail: enormous.pdf (one
  image that decodes to 243 MB) 70,352 to 5,964 KB, the two multipage files 79,152 to 36,120,
  issue9262_reduced 79,944 to 70,652. Only the last stage is counted that way: in a chain of several general
  filters (ASCII85 then Flate, say) the stages before it are still decoded whole. At effort 100 the plan
  decoded nothing before, so there the counting pass is new work, one more decode of every stream (0.66% more
  instructions on bug1743245.pdf, below); it stops counting when the time budget has run out (on a file of 40
  Flate images of 60 MiB of zeros each, `--effort 100 --time-budget 1000` gives up after 1.23 s with 1 thread
  or 4, as main does).
- **d782526** (main's, taken onto this work before main was merged into it, so it is no longer one of its
  commits): each object's bytes are freed once they are in the output: issue9262_reduced 70,652 to 60,444
  KB, issue19517 34,632 to 28,548.
- **8a6906e**: a stream with no general filter is read in place before it is re-encoded, not copied:
  issue12841_reduced 59,400 to 53,652 KB, issue9262_reduced 60,444 to 55,108, PDFJS-9279-reduced 39,592 to
  36,844.
- **d75ddc0** (taken out again by da52656, below): with one thread, a segment's plan is held until the next
  one's is made. Freed at once (23594ec), its memory and the segment's tables went back to the system after
  every segment and were faulted in again: twice the page faults (40,508 against 20,110 on issue12841_reduced,
  64,006 against 24,647 on issue9262_reduced) and 4 to 5% more CPU time at effort 0. Held, the faults are as
  before, and so is the time. This gives back part of 23594ec's saving on the files with several segments
  (issue12841_reduced 53,652 to 69,116 KB, issue9262_reduced 55,108 to 71,120: still 14,408 and 21,036 KB under
  the start).
- **6752422**: the JPEG with new tables is not copied while Flate is tried over it; it goes into the result at
  the end when nothing smaller came after it. On a PDF with one 3000 x 2400 JPEG (4.2 MB, 3.9 MB with new
  tables), 3 runs: 53.6 to 49.7 MB at effort 0 and 62.2 to 58.3 MB at effort 100, the same time. The sample's
  retabled JPEGs are 1 MB or less and the peak is elsewhere: no change there.
- **916fbfb**: with more than one thread, segments go in groups of eight again, as before 23594ec. In groups of
  as many segments as threads, a thread that finished its segment waited for the slowest of the group: scheduled
  from the one-thread segment times of issue12841_reduced (six segments, the first the slowest), its segments
  would take 26% longer with 3 threads and 40% longer with 4. One thread is unchanged.
- bbecf93 and 61d2ccf change no behaviour (clippy, rustfmt).

d75ddc0 measured again on top of #6, against the same build without it (16 alternating rounds, 1 thread):
without the hold the peak is lower (issue12841_reduced 48.1 MB instead of 60.9 at effort 0 and 57.2 instead of
67.9 at effort 100, issue6286 28.8 instead of 34.2, issue9262_reduced 57.5 instead of 59.4) and the CPU time
within 1%, but issue9262_reduced's page faults double again (48,101 against 21,492) and its system time with
them (0.18 against 0.09 s). Giving up about 0.1 s of system time on such files for up to 13 MB on files like
issue12841_reduced was the owner's choice, and the owner chose the memory: da52656 takes the hold out, so a
segment's plan is let go as soon as it is written, as with 23594ec alone. The outputs are the same (the hold
only decided when the plans were freed), and the tables at the top of this section are without it.

Time (before the fast tier, and not measured again on this base). CPU-time medians with 1 thread,
opt-lightweight (60c247d) and the after build run back to back in
alternating order on cores 0-2, 9 rounds (3 for GeoTopo.pdf and issue12963.pdf), on the file and effort pairs
the changes touch; every output the same:

| File | Effort | opt-lightweight (s) | After (s) | Change | Peak, opt-lightweight (MB) | Peak, after (MB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| GeoTopo.pdf | 0 | 67.305 | 65.987 | -2.0% | 82.9 | 71.7 |
| base64image.pdf | 0 | 4.113 | 4.005 | -2.6% | 38.0 | 34.6 |
| PDFJS-9279-reduced.pdf | 0 | 2.274 | 2.247 | -1.2% | 35.5 | 32.5 |
| freeculture.pdf | 0 | 16.513 | 17.111 | +3.6% | 39.6 | 35.5 |
| issue12841_reduced.pdf | 0 | 1.663 | 1.663 | +0.0% | 74.9 | 60.8 |
| issue19517.pdf | 0 | 0.216 | 0.094 | -56.7% | 84.9 | 28.7 |
| issue19971.pdf | 0 | 7.142 | 7.390 | +3.5% | 33.3 | 29.2 |
| issue6286.pdf | 0 | 1.941 | 1.904 | -1.9% | 39.5 | 34.2 |
| issue6931_reduced.pdf | 0 | 2.101 | 2.128 | +1.3% | 28.0 | 26.6 |
| issue9262_reduced.pdf | 0 | 9.983 | 9.968 | -0.1% | 90.5 | 59.5 |
| issue9940.pdf | 0 | 6.027 | 5.971 | -0.9% | 47.2 | 44.7 |
| enormous.pdf | 0 | 0.307 | 0.122 | -60.4% | 70.2 | 5.8 |
| ocrmypdf multipage.pdf | 0 | 6.544 | 6.607 | +1.0% | 79.2 | 32.0 |
| pikepdf pdfa multipage.pdf | 0 | 6.526 | 6.384 | -2.2% | 79.1 | 32.0 |
| PDFJS-9279-reduced.pdf | 100 | 4.859 | 4.768 | -1.9% | 40.5 | 38.7 |
| issue12841_reduced.pdf | 100 | 3.140 | 3.079 | -2.0% | 77.2 | 68.0 |
| issue12963.pdf | 100 | 67.799 | 67.142 | -1.0% | 136.1 | 112.9 |
| issue13931.pdf | 100 | 14.081 | 14.127 | +0.3% | 70.3 | 45.8 |
| issue14821.pdf | 100 | 6.153 | 6.120 | -0.5% | 28.7 | 28.9 |
| issue19517.pdf | 100 | 0.169 | 0.117 | -30.8% | 84.8 | 28.6 |
| issue6286.pdf | 100 | 2.794 | 2.710 | -3.0% | 35.3 | 33.8 |
| enormous.pdf | 100 | 0.160 | 0.130 | -18.8% | 70.0 | 5.8 |

The sum of the medians is 0.9% lower (231.8 to 229.8 s). enormous.pdf and issue19517.pdf take less than half
the time at effort 0, where a stream is no longer decoded only to be dropped.

Two rows are over 2%. issue19971.pdf at effort 0 (+3.5%) was -0.9% in 9 more rounds (the per-round ratios of
the 18: +0.6%), and the after build runs the same instructions (53,467 million against 53,457 million, +0.02%,
cachegrind). freeculture.pdf at effort 0 is slower in most sets: +3.6% here, then +3.8%, +1.3%, +2.7% and -0.4%
(9, 9, 7, 9 and 7 rounds); over the 41 rounds the medians are 16.76 and 17.18 s (+2.5%), and 31 rounds were
slower. It is not more work: the after build runs the same instructions (129,742 million against 129,721
million, +0.02%) and, in cachegrind's cache model, has as many first-level data misses (116.5 million each) and
fewer last-level ones (0.68 against 0.99 million). Nor is it 8a6906e: with its in-place read undone (one line),
the build was +1.9% in the same 7 rounds. The cause was not found. Differences this size with the same
instructions came and went with unrelated changes during this work (below), so it is probably where code and
data fall, which cachegrind does not model. On main's fast tier, merged since, it is gone: at effort 0 the
after build runs 0.28% fewer instructions on freeculture.pdf (38,652 million against 38,761 million,
callgrind), and over 12 rounds (both builds at once on two cores, which they swap each round) the median of
the per-round ratios is -1.3% (5 rounds slower; in round 9 the two builds' lines came out interleaved, and
they are told apart by their peak memory, about 32.2 MB for main and 29.9 MB for the after build).

Files that the space runs showed as slower were timed the same way: GeoTopo-komprimiert.pdf at effort 0 +1.7%
over 16 rounds (+4.2% in the first 7, -1.2% in the next 9), with the same instructions (84,732 million against
84,718 million); issue12810.pdf at effort 0 -0.8% over 34 rounds; bug1743245.pdf at effort 100 +3.2% on the
medians of 34 rounds but +1.3% on the per-round ratios (its times ran from 1.01 to 1.54 s). That file runs
0.66% more instructions: at effort 100, a54f45c adds a counting pass over each stream.

The review found that, merged into opt-lightweight with #6, the deflate grouping (23594ec with d75ddc0) cost
over 2% on issue12841_reduced, issue9262_reduced and issue6286. Measured again on 60c247d, it does not: the
merge against opt-lightweight, 16 alternating rounds, was issue12841_reduced +2.0% at effort 0 (-0.2% in 25
more rounds) and -1.6% at effort 100, issue9262_reduced +0.5%, issue6286 -1.1% (-2.3% in 25 more),
PDFJS-9279-reduced -1.0%, and the sum -0.02%. It runs the same instructions (issue12841_reduced: 12,595.7
million against 12,592.2 million, +0.03%), with fewer cache misses in cachegrind's model (first level 57.3
against 57.6 million, last level 1.59 against 1.92 million). The same merge with the grouping taken out ran
the same instructions again and was 3.5% slower over those files: on this machine a few percent come and go
with code that does the same work.

What is left. massif (valgrind's heap profiler) on the after build, 1 thread, shows what the files over the
line hold at their peak (MB of heap; the resident peaks above are lower where a buffer is reserved but not all
used):

- GeoTopo.pdf at effort 0 (50.9 MiB, line 42.1): the job of one image stream, which decodes the stream (11.1
  MB, Flate's output grown by doubling), undoes its predictor (9.7 MB of samples) and decodes the re-encoded
  stream again to check it (12.5 MB, grown the same way); next to it the input (5.3 MB) and its parsed objects
  (5.0 MB).
- issue9262_reduced.pdf at effort 0 (46.0 MiB, line 42.6): the self-check at the end, which decodes one stream
  as it was and as it is written, each whole (two buffers of 16 MiB: made at four times the compressed size,
  then doubled); next to it the input and the output (5.5 MB each) and both files' parsed objects (5.4 and 5.3
  MB).
- issue12963.pdf at effort 100 (81.3 MiB, line 33.2): one image's job: the decoded image (30.0 MB, grown by
  doubling), the filtered candidate being encoded (26.1 MB), the encoder's steps for it (12.6 MB), its output
  so far (13.0 MB) and the parse's tables (4.2 MB). Before the fast tier the best candidate and the one being
  tried were both held there (49.8 MiB together); since main's #14 only the candidates' scores are kept, and
  the winner is filtered again at the end.

So the next steps are decoding the two streams of a check a piece at a time and comparing them as they go (the
stream jobs' check and `self_check`, src/pdf/rewrite.rs), an output buffer made at a stream's decoded length
where it is known instead of grown by doubling, and plans that hold less than 12 MiB of steps for a 1 MiB
segment.

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
