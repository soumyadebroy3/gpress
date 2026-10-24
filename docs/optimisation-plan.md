# gpress: where the time and the memory go, and the plan to cut them

Where this stands (added when the plan went into the repository, 2026-10-08): it was measured on branch
`speed` (commit ca8fa92), which is the engine now on `main` with these differences. preflate-rs is gone
(its dependency `cabac` is LGPL), so item D6 and the preflate planning costs in sections 4 and 7 no longer
apply, and the non-zlib streams it rebuilt are stored as they are (D5's failed parameter search still
applies to them). The archive is the gpress archive, format version 9, written as `.gpz`, where the text
below says "peyaji format" or "version 4" (the model and the parts it measures are the same, except that
version 9 works out the logistic tables and probability maps from their formulas; version 8 adds
the decoded total to the header and a `max_decoded` budget on both sides, and its planner keeps no decoded
data and its residual history lives per segment, so the archive memory figures of sections 5 and 8 (D4) are
from before those two of D4's four copies went). The JPEG
Huffman step and the fax steps are on the optimize path now (section 3.3's note that they are not refers
to the branch measured). The binary is `gpress` where the text says `pdfe`, and the benchmark rows named
below live in `bench/results/`; the raw profile data (`profile-data/`, 35 MB) stays outside the repository,
on the machine the measurements were made on. The task briefs under `docs/tasks/` turn the candidates into
work for separate branches.

Measured on branch `speed` (commit ca8fa92), built with debug symbols (`CARGO_PROFILE_RELEASE_DEBUG=1`, LTO
off, 16 codegen units, `--features existing`) in its own target directory. The raw measurements are under
`profile-data/` (outside the repository; layout at the end). Nothing was committed to the engine; the only
code change is a measuring patch (`profile-data/instrumentation.patch`: extra timing lines that print under
`PDFE_DEBUG=1` and a predictor probe under `PDFE_PROBE_PRED=1`; the output bytes are unchanged).

The owner's rules apply to every item below: output byte for byte identical where the item says "pure", or
else a trade whose loss is measured and that must not make any file larger before it is taken; deterministic;
everything through options; no new dependency.

## 1. The numbers at a glance

Machine: 2 CPUs, 8 GB, shared with two other running jobs all through these measurements (load average 4.5 to
7). Wall times are therefore 2 to 3 times the CPU times; every figure called "CPU" below is the child's user +
system time from `os.wait4`, every "Ir" is an instruction count from callgrind (independent of the load), and
"wall" is only used for shares inside one run (every part of a run is slowed the same way).

Normal PDF (`optimize`), 1 thread, CPU seconds:

| Set | effort 0 | effort 50 | effort 100 | qpdf |
| --- | --- | --- | --- | --- |
| sample120 (78.3 MB: the 20 slowest, the 20 largest, 6 files in both lists, and 86 random) | 156.0 | 274.4 | 854.8 | 15.0 |
| the 20 slowest files of `pdfmax3/ours.tsv` (26.3 MB) | 138.6 | 254.7 | 818.3 | 8.9 |
| the 14 largest files not among the slowest (45.9 MB) | 7.2 | 7.8 | 15.4 | 3.9 |
| the 86 random files (6.1 MB) | 10.2 | 11.9 | 21.0 | 2.2 |
| output of the sample, % of input | 90.70 | 90.33 | 90.23 | |

(All rows are the time benchmark's records of this same build, `bench/results/sample120-speed-ca8fa92/ours-pdf-e*-t1.jsonl`, summed over the subsets of `bench/sets/sample120.txt`; my own instrumented runs of the slowest 20 at effort 100 total 817.8 s CPU against the benchmark's 818.3.) The 20 slowest files took 1,980 s in the earlier measurement at d85cd7a and take 818 s CPU on this build with outputs 90.8 KB smaller in total; two files are larger than before: `3small.pdf` by 2,148 bytes (156,398 against 154,250; worth a look by whoever changed the JPEG rule) and `issue9940.pdf` by 43 bytes.

Whole set (907 files, 130.5 MB), predicted for this build from the sample (slowest 20 measured, the 14 other
largest files measured, the remaining 787 files of 52.2 MB at the 86 random files' rate: 3.42 s/MB at effort
100, 1.93 at 50, 1.67 at 0): 818 + 15 + 178 = about 1,010 s CPU at effort 100, 255 + 8 + 101 = about 360 s
at effort 50, 139 + 7 + 87 = about 230 s at effort 0, against qpdf's 25 s CPU (28 s wall) measured on all
907 by the time benchmark (the spec's 58 s was the earlier wall-time measurement). So we are 40x qpdf at
effort 100 and 9x at effort 0.

Archive (`pack`, which includes the restore check), 1 thread, CPU seconds:

| Set | effort 0 | effort 50 | xz -9e | zstd 19 long | brotli 11 |
| --- | --- | --- | --- | --- | --- |
| sample120 | 341.7 | 378.7 | 27.5 | 16.8 | 147.7 |
| output, % of input | 53.07 | 52.98 | 74.83 | 75.01 | 74.72 |
| peak RSS, MB | 950 | 950 | 136 | 100 | 110 |

Effort 100 behaves like 50 in the archive (the effort only switches plain coding below 50), so the whole set is
predicted at about 650 s CPU with 1 thread (half of it is the restore check), against xz's 57 s: the sample's
379 s plus the 52.2 MB outside it at the random files' rate, which is 6.3 s/MB with `enormous.pdf` (311 KB,
14.1 s: it fell into the random part by the seed) and 4.2 s/MB without it, so 600 to 700 s.

Where it goes, in one sentence each:

- Normal PDF at effort 100: the big images. On the slowest file (geotopo, 5.3 MB, 868 streams) 19 images take
  94% of the stream time, and the predictor search (five whole-image quick encodes per image, then the full
  encode of the winner) takes 69% of the file's time. Everything outside stream encoding (parsing, the gate,
  object streams, the xref stream, the self-check) is under 3% at effort 100 and under 6% at effort 0 on the
  image-heavy files; on files whose streams are already small (geotopo-komprimiert, base64image) the object
  streams, which are Flate streams encoded at the same effort, are 7 to 9% and the xref stream up to 2%.
- Normal PDF on typical files: fixed costs per stream, per segment and per block that the effort knob does
  not touch: block splitting (`estimate_dynamic_bits`, 42 to 84% of the instructions of streams between
  18 KB and 216 KB at effort 0), the Huffman and header searches (88% for a 61-byte stream, 3 to 4 ms per
  block), zeroing the matcher's 768 KiB tables per call. tracemonkey.pdf (377 streams) takes 10.1 s CPU at
  effort 0 and 15.8 s at effort 100 in the benchmark's run (10.8 and 16.5 s in my instrumented runs): the
  effort knob reaches only a third of its time.
- Archive: the context-mixing model at about 1,400 instructions per coded bit, on every decoded byte, twice
  (encode, then the restore check); for big files a third of that is memory latency in 160 MiB of tables.
  Planning (zlib parameter search and preflate) is a second-order cost except on non-zlib streams, where a
  failed parameter search costs seconds.
- Memory: the archive holds four copies of every decoded byte (decoded plans, history, residual history,
  the restored copy): 949 MB RSS for a 311 KB file whose one image decodes to 243 MB. The PDF path's peak is
  the matcher's alternative steps at effort 60 and above: up to 100 bytes per input byte of a 1 MiB segment.

Predicted totals after the plan (section 9, with the arithmetic): effort 100 from about 1,010 s to about
740 s CPU with no byte changed; to about 690 s with the predictor ranking sampled at 4 MiB (the one trade
whose loss is measured: no byte changed on the 62 images probed), about 630 s at 1 MiB (+4,041 bytes on 2 of
the 62) or about 590 s at 256 KiB (+5,749 bytes on 5 of the 62); and to about 350 to 430 s if the parse
trades pass the per-file rule; effort 0 from 230 s to about 160 s pure; the archive from 650 s to 400 to
450 s pure and 210 to 240 s wall with 2 threads; peak memory of the archive from 4 bytes per decoded byte
to 1, the PDF path's top-effort peak by a quarter to a third.

## 2. How it was measured

- Build: as above, with the measuring patch. The patch adds `Instant` timers around the phases of
  `rewrite()`, around decode / encode / predictor search in `reencode()`, around each stream's planning in
  `pack::plan()`, and per-kind accumulators in `Io::bytes` (the archive); all printing only under
  `PDFE_DEBUG`. The probe (`PDFE_PROBE_PRED`) is measuring-only code that re-runs the predictor ranking on
  sampled rows and fully encodes the other choice when it differs.
- Whole files: `profile-data/run_files.py` (resumable; `nice -n 10`; rusage via `os.wait4`; the engine's
  stderr kept per file under `runs/logs/`): the 20 slowest PDF files at effort 100 (all 20; also tracemonkey
  and geotopo at effort 0) and at effort 50 (the first 7 before the account's usage cut-off; the benchmark's
  own effort-50 run covers all 20), and the 20 slowest archive files at effort 100 with a separate `unpack`
  (all 20). Summaries: `runs/*.analysis.txt` from `analyse_runs.py`.
- Streams: six streams pulled out of the corpus with pikepdf (`streams/`): a page content stream (65 KB),
  a large vector content stream (888 KB), a TrueType font (117 KB), a highly compressible RGB image (3.0 MB,
  compresses 100:1), an indexed image that compresses 15:1 (2.9 MB), a JPEG behind Flate (216 KB), plus a
  61-byte, a 4 KB and an 18 KB content stream for the fixed costs. `time_streams.py` ran `pdfe deflate` (now `gpress deflate`) on
  each at efforts 0, 20, 50, 80, 100 (`streams.tsv`, per-run logs in `stream-logs/`); `knobs.py` ran the
  effort-100 switches (`knobs.tsv`).
- callgrind (`--dump-instr=no --collect-jumps=no`, summaries with `cg_summary.py`, call counts with
  `callgrind_annotate --tree=caller`): the streams at efforts 100 and 0, `pack` of canvas.pdf (150 KB, 23
  streams, 14 images), `unpack` of its archive, `optimize` of a 10 KB form.
- massif: `optimize` and `pack` of canvas.pdf, the 10 KB form, the RGB image alone; peaks read with
  `ms_peak.py`.
- The time benchmark's own results on this build (same binary source, its own target directory) are used
  for the sample totals, the fixed cost of an empty page and the competitors.

## 3. Normal PDF: where the time goes

### 3.1 By phase

From the phase timers (wall, shares within the run), effort 100 and 0:

| File | streams | objstm | xref | self-check | parse+gate |
| --- | --- | --- | --- | --- | --- |
| geotopo 5.3 MB, e100 (785 s wall, 316 s CPU) | 99.2% | 0.5% (349 KB in 17 object streams) | 0.1% | 0.14% | 0.01% |
| geotopo, e0 (172 s wall in my run; 55 s CPU in the benchmark's) | 97.4% | 1.1% | 0.4% | 0.9% | 0.03% (plus planning 0.3%) |
| tracemonkey 1.0 MB, e100 (38.8 s wall, 16.5 s CPU) | 98.8% | 0.6% | 0.4% | 0.2% | 0.01% |
| tracemonkey, e0 (32.4 s wall in my run; 10.1 s CPU in the benchmark's) | 98.3% | 0.8% | 0.4% | 0.3% | 0.06% (plus planning 0.1%) |
| freeculture 2.6 MB, e100 (73.8 s wall, 23.7 s CPU) | 95.9% | 2.9% | 0.8% | 0.3% | 0.01% |
| geotopo-komprimiert 1.8 MB, e100 (51.8 s wall, 16.2 s CPU) | 90.2% | 7.4% | 1.8% | 0.4% | 0.13% |
| base64image 2.3 MB, e100 (40.4 s wall, 12.7 s CPU) | 90.4% | 9.1% | 0.05% | 0.3% | 0.03% |

(Shares from the `phases:` line of each log under `runs/logs/`, printed for all 29 logs by `phases.py`; no
file has the non-stream work above 10%.) Reading the file, loading every
object, the gate, planning, writing and the self-check are together under 1% at effort 100 and about 2% at
effort 0 even on the file with the most objects (4,192). The self-check (read the output back, decode every
stream, compare) is 1.1 to 1.5 s on geotopo: it is a full decode of the output, which is cheap next to any
encode. The object streams and the xref stream are Flate streams encoded at the same effort as the rest:
0.5 to 3% on the image files, 7 to 9% on geotopo-komprimiert and base64image, whose other streams are quick;
every Flate item of section 8 applies to them too. Nothing else to win here beyond a few percent at effort 0.

### 3.2 By stream kind, at effort 100

The 20 slowest files (my runs; `runs/optimize-e100-t1.analysis.txt`):

| File | CPU s | streams | of which images | predictor search | biggest stream |
| --- | --- | --- | --- | --- | --- |
| geotopo | 315.6 | 868 | 19 images = 94% | 69% of the file | 9.7 MB RGB image: 147 s wall = encode 42 + predictors 105 |
| issue12963 | 78.2 | 124 | 1 image = 99% | 56% | 26 MB (2480x3506 RGB): encode 72 + predictors 93 |
| issue1905 | 85.2 | 32 | 9 = 97% | 46% | 3 MB RGB: encode 17.6 + predictors 12.8 |
| 2400dpi | 34.9 | 2 | 1 = 100% | 49% | 11.5 MB 1-bit scan: encode 52 + predictors 50 |
| issue9940 | 34.4 | 33 | 2 = 99% | 71% | 2.9 MB indexed: encode 17 + predictors 48 |
| multipage (x2) | 32.4 | 18 | 4 = 98% | 52% | 4.7 MB: encode 28 + predictors 23 |
| canvas | 26.9 | 23 | 14 = 97% | 61% | 189 KB RGB x 14: 5 to 7 s each |
| tracemonkey | 16.5 | 377 | 180 = 67% | 51% | 888 KB vector content: 2.6 s |
| freeculture | 23.7 | 391 | 1 = 31% | 14% | 2.8 MB gray image 22 s; 2 CCITT and 1 JPEG stream 0.5 to 1.7 s each |
| issue9262 | 16.1 | 4 | 0 | 0 | 10 MB content stream that barely compresses (5.4 MB to 5.2 MB): 43 s wall |
| S2 | 15.6 | 40 | 3 = 75% | 28% | 1.3 MB indexed x 2 |

Across the 20 files (`runs/optimize-e100-t1.analysis.txt`): 818 s CPU in all; images are 87% of the stream
time and the predictor timer 55% (1,131 of 2,067 s wall over the 256 images that were ranked). That timer
covers the five quick encodes, the five PNG filter passes and the full encode of the winning candidate; the
five quick encodes alone are about a quarter of the files' time (2400dpi, whose winner is "no predictor" and
whose full encode is remembered: 50 s of 103 s wall for the five quick encodes of 11.5 MB; geotopo's 9.7 MB
image: 147 s = plain encode 42 + about 45 for the five quick encodes + about 60 for the full encode of the
Paeth-filtered winner). The predictor search is `repredict()`: for an image it quick-encodes (effort 0) five
candidates (no predictor, PNG per-row minimum-sum, Up, Sub, Paeth) on the *whole* image at effort 100, picks
the best size, and fully encodes that one. So a big image is encoded seven times: once plain in `reencode()`,
five times quick, once fully for the winner (the plain full encode is remembered and not repeated). Below
effort 100 the ranking uses sampled rows (64 KiB at effort 0, doubling every 20 points), which is why
geotopo takes 103 s at effort 50 and 316 s at 100.

### 3.3 Inside one stream: the Flate encoder stage by stage

Instruction shares from callgrind (`callgrind/*.out`), effort 100 unless said:

| Stream | Ir total | matcher | parse passes | block split (`estimate_dynamic_bits`) | Huffman + header (`dynamic_search`) | passes |
| --- | --- | --- | --- | --- | --- | --- |
| 61 B content, e0 | 13.3 M | 6% | <1% | 0 | 88% (header search 76%) | |
| 61 B content, e100 | 17.5 M | 4.5% | <1% | 0 | 89% (header search 74%) | |
| 4 KB content, e0 / e100 | 41 M / 71 M | 4% / 2.5% | 20% / 24% | 7% / 10% | 66% / 60% (header search 40% / 34%) | |
| 18 KB content, e0 | 930 M | 2% | 2.5% | 73% | 15% | |
| 18 KB content, e100 | 1.20 G | <1% | 9% | 59% | 23% (header search 13%) | |
| 65 KB page content, e0 | 810 M | 12% | 19% | 42% | 17.5% (header search 15.5%) | |
| 65 KB page content, e100 | 1.91 G | 6.6% | 48% | 19% | 22% (header search 18.5%) | 98 |
| 117 KB TrueType, e0 | 1.19 G | 4.5% | 15% | 62% | 9.5% | |
| 117 KB TrueType, e100 | 2.32 G | 2.5% | 47% | 34% | 10% | 98 |
| 216 KB JPEG behind Flate, e0 | 2.95 G | <1% | 7.5% | 84% | 2.7% | |
| 216 KB JPEG behind Flate, e100 | 4.20 G | <1% | 27% | 61% | 4.4% | |
| 888 KB vector content, e100 | 8.93 G | 7% | 74% | 10.6% | 4.2% | |
| 2.9 MB indexed image, e0 | 13.9 G | 64% | 20% | 13% | 1.5% | |
| 2.9 MB indexed image, e100 | 58.3 G | 19% | 77% | 3.3% | <1% | 204 |
| 3.0 MB RGB image (100:1), e0 | 10.95 G | 77% | 5% | 14% | 1.7% | |
| 3.0 MB RGB image, e100 | 86.4 G | 11.6% | 86% | 1.9% | <1% | 158 |

(Inclusive shares of `find_matches`, `parse_avx2`, `estimate_dynamic_bits` and `dynamic_search` from
`cg_summary.py` on each `.out`; on top of these, `memset` (the matcher's tables and the Huffman arrays) is
7 to 13% of every stream under 100 KB: 9.5% of the 61-byte stream, 7% of the 4 KB one, 12.6% of the 65 KB
one at effort 0.)

What this says, stage by stage:

**Match finder** (`matcher.rs`, binary trees per 3-byte hash, `max_chain` nodes per position). Cost is
positions x nodes visited. On compressible images at effort 0 it visits about 70 of the allowed 128 nodes
per position (2,790 Ir per byte; 8.4 G Ir of 11 G) and is the whole cost of a quick encode; on text and
fonts it is 2 to 7%. Inside it, 8% is `dist_code()` for the distance-bucket bookkeeping that is only used
when `alternatives` is on, and 5% is the `try_into().unwrap()` bounds checks of the 8-byte compare. It
also zeroes 768 KiB of tables (`head` and `son`) on every call: 1.5 M Ir and fresh pages from the
allocator for a 61-byte input.

**Optimal parse** (`parse.rs`): about 95 Ir per input byte per pass on text and fonts, up to 250 on
images with long matches (the relaxation runs over every length up to the longest match: the four lines of
`relax_range` are 34% of the RGB image's 86 G Ir, already vectorised at under one instruction per relaxed
length). The number of passes is the multiplier: 98 passes for the font and content stream, 158 and 204 for
the images (8 entropy-priced rounds on the segment, then per block 8 more and up to 4 exact rounds x 3
variants, then merge and split polishing with more exact rounds). At effort 100 this is the dominant cost
of every stream above about 100 KB. Bounds checks and iterator overhead inside the pass are about 30% of
its instructions (`index.rs` 173 M of 1,089 M on the font).

**Block splitting** (`split()` in `deflate.rs`): a dynamic programme over up to 161 candidate cut points
(`g = max(n/160, 32)` symbols apart) with a span limit of 96, so up to 10,900 evaluations of
`estimate_dynamic_bits` per segment, each a full Huffman construction on 316 symbols (`fast_lengths` sorts
the counts: 68 k Ir per call, half of it the sort) and on data where the unlimited code exceeds 15 bits
the package-merge fallback at 185 k Ir (on the JPEG, 4,148 of 10,896 pairs: rare long length codes next to
frequent literals). The count of candidates does not shrink for small segments, so this is a fixed 0.7 to
2.5 G Ir (0.2 to 0.8 s) for every segment with more than about 5,000 symbols, which is why an 18 KB
content stream costs 930 M Ir at effort 0 and why the JPEG behind Flate spends 84% of its effort-0 time
deciding block boundaries for data that is nearly incompressible.

**Huffman code and header search** (`block.rs`): `price()` after every parse pass builds two
length-limited codes by package-merge and a quick header (2 token strategies x 2 rounds of `dp_tokens`),
about 1 M Ir; `best_dynamic` per block tries 6 count-smoothing variants, each with the full header search
(8 token strategies x up to 4 rounds): about 10 M Ir, 3 ms. For a 61-byte stream that is the whole cost
(32 header searches at 317 k Ir each, 76% of its instructions), and tracemonkey's 377 streams have a
median stream time of about 27 ms CPU at effort 0 for this reason.

**Polish** (merge and split of neighbouring blocks, effort 60 and up): 21% of the 65 KB content stream
(111 of 522 ms), 5% of the font, 0.2% to 1% of the images.

**zlib re-creation** (`zlibexact`): not used by `optimize` at all; archive only (4.3).

**Image predictor search**: section 3.2; it multiplies the whole encoder cost by about 2.5 for big images
at effort 100 and adds five fixed-cost quick encodes to every small image (tracemonkey's 180 strips of 47
pixels: 19.6 of 25.6 s).

**JPEG step, Lepton, preflate**: not on this branch's `optimize` path (the JPEG Huffman step lives on the
`jpeg` branch; Lepton and preflate are archive-only).

**Decode of the input streams**: 0.4 s of geotopo's 316 s. **Self-check**: 0.14% (above).

### 3.4 What the effort knob buys, per stream kind (CPU seconds, output bytes)

| Stream | e0 | e20 | e50 | e80 | e100 | bytes e0 / e100 |
| --- | --- | --- | --- | --- | --- | --- |
| 65 KB page content | 0.08 | 0.08 | 0.08 | 0.17 | 0.22 | 10,523 / 10,469 (+0.5%) |
| 117 KB font | 0.15 | 0.15 | 0.18 | 0.23 | 0.28 | 58,655 / 58,632 (+0.04%) |
| 216 KB JPEG behind Flate | 0.24 | 0.24 | 0.21 | 0.38 | 0.37 | 213,166 / 213,148 (+0.01%) |
| 888 KB vector content | 0.48 | 0.48 | 0.51 | 1.05 | 1.17 | 281,400 / 280,981 (+0.15%) |
| 2.9 MB indexed image | 1.24 | 2.07 | 2.44 | 4.87 | 5.93 | 196,271 / 193,144 (+1.6%) |
| 3.0 MB RGB image | 0.91 | 2.77 | 3.08 | 7.63 | 7.98 | 31,105 / 29,961 (+3.8%) |

(`streams.tsv`.) Efforts 0 to 50 cost the same on text and fonts: their time is the fixed costs. The step
from 50 to 80 (alternatives, polish, more rounds, chain 4096) is where the time doubles.

The effort-100 switches on the same streams (`knobs.tsv`, a separate run whose effort-100 base times are
6.30 s and 8.61 s for the two images, the machine's load having moved between the runs; time relative to
that run's effort 100, size change):

| Switch | content 65 KB | font | indexed image | RGB image | JPEG | vector 888 KB |
| --- | --- | --- | --- | --- | --- | --- |
| `--nice 128` | 0.79x, -0.02% | 0.75x, +0.01% | 0.49x, +0.57% | 0.16x, +1.56% | 0.99x, 0 | 1.02x, 0 |
| `--noalt` | 0.79x, +0.04% | 1.02x, +0.01% | 0.79x, 0 | 0.86x, +0.03% | 0.96x, 0 | 0.82x, +0.02% |
| `--iter 4` | 0.64x, -0.09% | 0.76x, 0 | 0.77x, 0 | 0.93x, 0 | 0.92x, 0 | 0.79x, 0 |
| `--exact 2` | 0.99x, +0.02% | 0.95x, 0 | 0.87x, 0 | 0.86x, 0 | 0.98x, 0 | 0.93x, 0 |
| `--nopolish` | 0.66x, +0.08% | 1.06x, 0 | 0.99x, 0 | 0.82x, +0.19% | 0.92x, 0 | 1.00x, +0.02% |
| `--nosplit` | 0.52x, +2.6% | 0.54x, +1.6% | 0.88x, +0.7% | 0.94x, +0.95% | 0.18x, +0.05% | 0.99x, +0.4% |

Two things to note: the search is not monotone (`--iter 4` is smaller than effort 100 on the content stream
and the vector stream, `--nice 128` on the content stream), and no single switch is free on every stream.
That is exactly why a trade can only be taken with the per-file check of the benchmark.

## 4. Archive: where the time goes

### 4.1 Planning, model, restore, rebuild

`pack` = plan (cut the file; per stream try the zlib parameter search, then preflate, then Lepton) then
`run` with the encoder (the model codes every part), then `run` again with the decoder as the restore check
(a second thread does it alongside when there are two). Measured with the per-kind timers:

| File | input | decoded bytes through the model | bypassed by the long-repeat path | encode | of which planning | restore check | `unpack` alone (CPU) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| canvas.pdf | 150 KB | 2.59 MB | 1.85 MB | 2.28 s wall | 0.11 s | 1.92 s wall | |
| enormous.pdf | 311 KB | 243 MB (one image) | 241.4 MB | 28.3 s wall (15.5 s CPU for the whole pack) | 4.3 s (inflate + parameter trials) + 2.7 s re-creating the zlib bytes | 23.5 s wall | 6.7 s |
| multipage.pdf | 1.1 MB | 142 MB | 137.9 MB | 38.3 s wall (19.5 s CPU whole) | 15.1 s: one 450 KB non-zlib stream = 2.6 s of failed parameter trials + 11.3 s preflate | 24.5 s wall | 7.8 s |
| geotopo | 5.3 MB | 53 MB (881 rebuilt streams) | 31.1 MB | 77.5 s wall (156 s wall, 52.7 s CPU for the whole pack) | 4.9 s + 4.9 s re-creating the zlib bytes | 78.1 s wall | 25.6 s |
| filled-background.pdf | 401 KB | 401 KB of syntax | 300 KB | 0.47 s | | 0.45 s | |

(`filled-background.pdf` took 51.8 s at d85cd7a because of a 400 KB number token; this branch's incremental
token hash fixed it: 1.1 s now, 129 us per byte then, 1.2 now.)

The model costs the same on the way out as on the way in, so the restore check doubles the CPU of every
pack; with two threads it is hidden in wall time (encoder and decoder run side by side at the same speed),
with one thread (a browser without threads) it is a plain 2x.

### 4.2 The model per bit

callgrind on `pack` of canvas.pdf: 17.5 G Ir in all, 8.2 G encode and 8.5 G restore check. About 730 K
bytes went through the full model in each direction (the rest were long repeats at one decision per byte):
1,400 Ir per coded bit. Inside it (both directions together, self costs): `Model::update` 1.76 G plus 2.66 G
in the counters it drives (`Counter::update`: `np = p + ((target - p) * RECIP[n]) >> 16` is 0.66 G by
itself, the pack/unpack of the 32-bit counter 0.8 G), `Model::predict` 1.39 G plus 1.43 G in the state
maps and stretch, the mixer dot product and weight update 0.75 G, the two APMs 0.4 G, `contexts()` 0.78 G
per byte (10 hashes and 10 bucket finds), `byte_done` 0.7 G, `image_neighbours` 0.42 G. Bounds checks
(`slice/index.rs`) are 2.5 G, 14% of everything; iterator machinery (`zip`, `range`, `cmp`) another 1.5 G.
So about 30% of the model's instructions are checks and iterator overhead, not arithmetic.

The tables: 10 contexts x 2^bits buckets of 16 bytes, bits from 14 to 20 by the *file* length (`table_bits`:
2.5 MiB for files under 128 KB, 160 MiB from 8 MB up), plus a 4 MiB match table and a 4.1 MiB APM. Every bit
reads 10 buckets and writes them back; at 160 MiB they are random DRAM reads. The per-kind timers show
2.7 to 3.2 us per byte (wall, loaded) on canvas (5 MiB of tables) for text, fonts and image residuals alike.

### 4.3 Planning

`rebuild_params` runs our inflate and then `find_params`: up to 9 memory levels x 1 to 4 levels x 2
strategies = 72 trials of a bit-exact zlib run that stops at the first differing block. A zlib stream is
usually found in the first trial (canvas: 0.6 to 9.5 ms per stream). A stream that zlib did not write pays
for all 72 trials, each running up to a whole first block (2.6 s on multipage's 450 KB stream), and then
preflate (an existing library, 25 us per byte: 11.3 s on that stream). Lepton on a 164 KB JPEG: 128 ms.

### 4.4 Memory

massif peaks: `pack` of canvas.pdf 26.1 MB = 14 MB fixed (APM 4.24, match table 4.0, 10 tables of 0.5 MB,
states and mixers) + history and residual history of all modelled bytes (two vectors that double as they
grow: 9 MB for 2.6 MB of bytes) + the decoded streams of the plan (2.7 MB) + the restored copy. A 10 KB
form: 13.6 MB. `unpack` of canvas: 22 MB. With 243 MB of decoded image, enormous.pdf reaches 949 MB RSS
(decoded plan + history + residual history + restored copy = 4 x 243 MB, plus 10 MiB of tables), and the
time benchmark's whole sample peaks there (950 MB; xz 136 MB). Two threads add a second model and a second
restored copy (1,086 MB).

## 5. Memory shape of the PDF path

massif peak of `optimize` canvas.pdf at effort 100: 30 MB, of which 23.1 MB is the matcher's step list
for a 189 KB image (`steps` with alternatives: up to 24 steps of 4 bytes per position, grown by doubling)
and 2.9 MB the chain list; at effort 0: 6.4 MB (2.9 MB steps, 0.75 MB tables, 0.72 MB parse arrays). The
largest allocations grow with the segment (1 MiB), not the file. `deflate` of the 3 MB RGB image alone
peaks at 102 MB at effort 100: 64 MB of steps for one 1 MiB segment (the `Vec` doubled past 32 MB), 16 MB
for the chain (a second copy of the smallest-distance steps), 4 + 4 MB for the parse's f32 costs and u32
choices, 4 MB of `start` indexes; at effort 50 (no alternatives) 34 MB with 16 MB of steps. This is per
thread: a 2-thread run holds two segments. The 10 MB content stream of issue9262 at effort 50 peaks at
87 MB: 22.7 MB of steps, 20.6 MB of decoded data, the parse arrays. On top of the segment: the document's
objects (stream data kept raw, about 1x the file), the serialised output bodies (1x), the output (1x), the
self-check's second load (1x) and, per image, the decoded samples, one filtered candidate and the best
candidate so far (3x the decoded image). The time benchmark's sample peaks at 169 MB at effort 100
(issue12963: the 26 MB decoded scan's three copies plus one segment's lists; geotopo is 158 MB) and 96 MB at
effort 0 (issue9262: the 10 MB content stream, decoded data plus the parse arrays of one segment). The fixed
cost for an empty page is 4.6 MB and 5 ms CPU (archive: 14 MB and 25 to 32 ms: table set-up and the priming
text run twice through the model). The archive on tracemonkey (1 MB, 5.4 MB decoded): 54 MB with 1 thread,
99 MB with 2 (a second model, history and restored copy for the check).

## 6. The 20 slowest files, explained

Normal PDF (from `pdfmax3/ours.tsv`; times now are CPU on this build, effort 100, 1 thread):

| File | then | now | What the time is |
| --- | --- | --- | --- |
| geotopo (5.3 MB) | 544.7 s | 315.6 s | 19 images = 732 of 779 s of stream time; the five biggest (9.7, 7.4, 6.5, 4.9, 3.2 MB decoded) each 50 to 150 s, of which 2/3 is the predictor search. 849 other streams: 47 s. |
| issue12963 (603 KB) | 297.9 | 78.2 | one 26 MB RGB scan (2480x3506): 166 of 168 s; encode 72, predictor search 93. |
| issue1905 (918 KB) | 297.1 | 85.2 | nine images of 1 to 3 MB (screenshots, 100:1): 155 of 159 s; long matches make every parse pass expensive (158 passes). |
| multipage, pdfa_multipage (1.1 MB, same content) | 99.5 / 79.2 | 32.4 / 33.3 | one 4.7 MB image 45 to 52 s, a 1.15 MB image 15 to 17 s; 4 images = 98%. |
| 2400dpi (164 KB) | 89.6 | 34.9 | one 11.5 MB 1-bit scan: 103 s wall = encode 52 + predictor search 50. |
| issue9940 (395 KB) | 81.7 | 34.4 | two indexed images (2.9 and 1.7 MB): 103 of 104 s, predictors 74. |
| issue19971 (1.7 MB) | 80.3 | 21.1 | ten images = 50 of 64 s; a 2.7 MB image 30 s. |
| canvas (151 KB) | 57.7 | 26.9 | fourteen 189 KB RGB images at 5 to 7 s each (predictors 60%): small images pay the fixed costs 6 times. |
| tracemonkey (1.0 MB) | 57.5 | 16.5 | 180 image strips (47 pixels wide): 25.6 s wall of which 19.6 s predictor search; 197 other streams 12.7 s; an 888 KB vector stream 2.6 s. |
| freeculture (2.6 MB) | 51.6 | 23.7 | one 2.8 MB gray image 22 s; 390 small streams (CCITT and JPEG kept, fonts, pages) 49 s wall at about 0.1 s each; object streams 2.2 s. |
| S2 (478 KB) | 44.6 | 15.6 | two 1.3 MB indexed images 28 + 18 s; 38 other streams 16 s. |
| issue6931 (56 KB) | 34.8 | 11.7 | one 2.6 MB image: 44 of 44 s. |
| 3small (166 KB) | 33.5 | 15.8 | three images (1.4 MB, 437 KB): 42 of 42 s, predictors 61%. |
| issue9262 (5.5 MB) | 32.4 | 16.1 | one 10 MB content stream that barely compresses: 43 s of 44 (4.3 us per byte, no predictor search; the parse passes on data without matches). |
| issue13931 (303 KB) | 25.1 | 13.5 | one 12.75 MB 1-bit image (4256x2996). |
| geotopo-komprimiert (1.8 MB) | 22.9 | 16.2 | geotopo's 865 streams with the images already small: the fixed costs of 571 re-encodes. |
| issue14821 (312 KB) | 16.9 | 6.0 | a 1.1 MB 1-bit image. |
| base64image (2.3 MB) | 16.4 | 12.7 | 3 streams re-encoded; a 2.2 MB text stream (base64) at effort 100. |
| issue12810 (254 KB) | 16.1 | 8.0 | two images. |

Archive (from `pack6/pack.tsv`; now = CPU of `pack` including the restore check, 1 thread):

| File | then | now | What the time is |
| --- | --- | --- | --- |
| enormous (311 KB) | 577.2 s | 15.5 s | 243 MB decoded image; 241.4 MB now go through the one-decision long-repeat path (version 4), 1.6 MB through the full model; 4.3 s inflate and parameter trials, 2.7 s re-creating the zlib bytes, twice (encode and check). 949 MB RSS. |
| multipage, pdfa_multipage (1.1 MB) | 357.7 / 270.4 | 19.5 / 19.3 | 142 MB decoded, 138 MB bypassed; 15 s of planning on one non-zlib 450 KB stream (72 failed zlib trials 2.6 s, preflate 11.3 s). |
| geotopo (5.3 MB) | 114.2 | 52.7 | 53 MB decoded (881 rebuilt streams), 31 MB bypassed; 22 MB through the model twice (images 46 s, content 15 s, fonts 4 s of the 77.5 s encode). |
| issue12963 (603 KB) | 89.1 | 7.8 | 52 MB decoded, mostly a flat scan: 49.7 MB bypassed, 2.5 MB modelled (10 s). |
| issue3188 (8.1 MB) | 60.7 | 41.6 | 787 streams, 102 JPEGs through Lepton, 20.6 MB decoded of which 5.5 MB bypassed; the images 54 of the 66 s encode. |
| filled-background (401 KB) | 51.8 | 0.4 | the quadratic token hash, fixed on this branch. |
| bug1978317 (1.5 MB) | 48.8 | 25.3 | 13 MB of syntax (28 s) and 3.3 MB of content (9.5 s) through the model; 7 preflated streams (1.1 MB one: 1.6 s), 4.8 MB bypassed. |
| issue9262 (5.5 MB) | 36.1 | 33.9 | the 10 MB content stream (classed as font) through the model: 51 of the 57 s encode; 2.9 s of parameter search. |
| issue16263 (58 KB) | 33.5 | 1.5 | 18.9 MB decoded, 18.6 MB bypassed. |
| issue1905 (918 KB) | 30.8 | 5.7 | 15.2 MB decoded, 13.5 MB bypassed; a 410 KB raw stream 1.9 s; 5 JPEGs through Lepton. |
| issue8795 (4.2 MB) | 29.1 | 21.9 | one 4.1 MB non-zlib stream: 0.9 s of failed parameter trials, 2.2 s preflate, then its 4.9 MB through the model (28 s). |
| issue13931 (303 KB) | 23.7 | 1.8 | 12.75 MB decoded, 12.6 MB bypassed; one 228 KB JPEG through Lepton (0.5 s). |
| issue12841 (5.9 MB) | 22.9 | 3.8 | one 5.85 MB JPEG: Lepton 2.8 s, taken; 4.6 MB coded plainly (1.3 s). |
| unreadablemetadata (2.8 MB) | 22.2 | 12.4 | 105 preflated streams; 3.9 MB content, 1.15 MB data, 1.15 MB syntax through the model. |
| graph (296 KB) | 21.7 | 4.1 | 12.6 MB decoded, 10.7 MB bypassed; 1.9 MB modelled (5.6 s). |
| 2400dpi (164 KB) | 19.7 | 5.3 | 11.5 MB decoded, 8.3 MB bypassed; 3.1 MB modelled (6.6 s). |
| freeculture (2.6 MB) | 19.7 | 12.0 | 355 preflated streams (planning 4.4 s); 2.4 MB content 8 s, 0.8 MB raw 4.9 s. |
| issue19517 (6.3 MB) | 19.3 | 12.6 | 6.26 MB of raw (not zlib, not preflated) stream bytes through the model (15 s), 3.3 MB bypassed. |
| issue19971 (1.7 MB) | 16.1 | 5.8 | 5.3 MB decoded, 3.6 MB bypassed; 2 JPEGs through Lepton (0.8 s). |

(`runs/pack-e100-t1.analysis.txt` has each file's model bytes by kind and its slowest plans.) The archive
outputs of this branch (format version 4) are smaller than at d85cd7a on 18 of these 20 files but larger on
two: geotopo 2,665,395 against 2,641,723 (+0.9%) and bug1978317 470,398 against 459,430 (+2.4%); that is
the `speed` branch as it stands, not this plan, but the owner's rule applies to it too and the benchmark's
`compare` against the stored d85cd7a results is the place to settle it.

## 7. Complexity of each stage

n = decoded bytes of a stream, s = symbols of its parse, S = segment size (1 MiB), k = candidate cut points
(at most 161), W = span limit (96), B = blocks, P = parse passes. "fixed" means a cost that does not shrink
with a smaller input.

| Stage | Cost | Settings that drive it |
| --- | --- | --- |
| Match finder | n x nodes visited, nodes <= `max_chain` (128 at effort 0, 8192 at 100; measured 70 on average on compressible images at 128); plus 768 KiB zeroed per call (fixed) | `max_chain`, `alternatives` (adds up to 24 steps per position and the bucket bookkeeping) |
| Optimal parse | P x n x (per-position overhead ~40 Ir + longest match length / 8 vector steps); P = 2 to 8 segment rounds + per block (2 to 8 rounds + up to 4 x 3 exact passes) + polish passes; measured P = 98 to 204 at effort 100, about 6 at effort 0 | `iterations`, `exact_rounds`, `alternatives` (a 32-way sort per position), `nice` (skips positions inside long matches), `multi_start_limit` (a second start for segments under 256 KiB), `polish` |
| Block split | k x W evaluations of a 316-symbol Huffman build (68 k Ir, or 185 k with the length limit): fixed 0.7 to 2.5 G Ir per segment above ~5,000 symbols, then fine-tuning 2g/step evaluations per cut | `split` only; no effort dependence |
| Huffman + header per block | `price()` per parse pass: 2 package-merge codes + quick header (~1 M Ir); `best_dynamic` per block: 6 variants x full header search (8 strategies x up to 4 rounds of a 316-position shortest path): ~10 M Ir | `exact_rounds` (one `best_dynamic` per round), block count |
| Polish | per block pair: merge test = histogram + price; per block: up to n/64 cut tests with 2 estimates each; accepted changes re-run `exact` | `polish`, `exact_rounds` |
| Predictor search | 5 x quick encode of the sampled rows (whole image at effort 100) + 1 full encode + 5 PNG filter passes (per pixel per row); the plain full encode is remembered | effort (sample size: 64 KiB << effort/20 below 100) |
| zlib re-creation (archive) | per stream: inflate n + up to 72 trials each up to one block (2^(mem+6) symbols) of a zlib run; success in the first trial costs one block | the stream's header (levels), memory levels in the order 8, 9, 7 .. 1 |
| Context model (archive) | 8 decisions per modelled byte x (10 bucket reads + 10 counter updates + 13-input mixer + 2 APMs) ~1,400 Ir; 1 decision per byte in a long repeat (>= 128 matched); 8 even-odds decisions per byte when coded plainly | `table_bits` (file size), `LONG_MIN` 128, `PLAIN_BELOW` 50 (format constants) |
| Restore check | the same as the encode | threads (hidden at 2+) |
| Self-check (PDF) | one decode of the output + object comparison: 0.1 to 1.5 s per file | none |
| Lepton, preflate | existing libraries: Lepton ~0.8 us per JPEG byte; preflate ~25 us per byte | tried only on non-zlib streams (preflate) and JPEG bytes (Lepton) |

## 8. The candidates, ordered by expected gain per unit of risk

Each item: kind (**pure** = output byte for byte identical, proven by `benchmark.py compare` on the stored
sha256 of every output; **trade** = output can change, measured on a sample, and the benchmark's per-file
rule decides), expected gain (from the numbers above), memory effect, format effect, proof.

### Group A: pure speed-ups of the Flate encoder (no byte changes)

**A1. Block-split estimates without the sort and without allocation.** `estimate_dynamic_bits` is called up
to 10,900 times per segment and is 42 to 84% of the streams between 18 KB and 216 KB at effort 0, 19 to 34%
of the 65 KB content stream and the font at effort 100 and about 60% of the 18 KB stream and the JPEG. Keep the
same algorithm and tie rules (so the lengths, and therefore the estimates, are identical) but: sort the
(count, symbol) keys with a counting or radix sort instead of quicksort (half of `fast_lengths`), keep the
previous sorted order between neighbouring DP pairs and fix it with an insertion pass (the DP walks i for a
fixed j, so the histogram changes by one candidate block at a time), zero only the `n` used entries instead of
the 320-entry arrays (`memset` is 8 to 10% of these streams), and replace the package-merge's 15 `Vec`s by
fixed arrays. Expected: 2 to 3x on this function, i.e. 25 to 45% off every stream under 200 KB at effort 0
and 10 to 20% at effort 100. Memory: none. Format: none. Proof: `compare` (identical outputs) plus a unit
test that the new `fast_lengths` and package-merge give the same lengths as the old on 100,000 random
histograms (the ties are the risk; the test pins them).

**A2. Tried and rejected: pruning the split DP with a lower bound.** `best[j] = min_i best[i] + est(i, j)`
could skip every `i` whose lower bound (Shannon entropy of the range plus the smallest header, or the exact
fixed cost, or the stored cost) already exceeds the best found, with the same tie rule, and give provably the
same cuts. I measured it (`examples/probe_split.rs`, kept under `profile-data/examples/`, same DP as
`split()`; output in `probe_split.log`): the cuts are identical, but the bound is too loose to help: of
11,088 evaluations it skips 2,180 on the 18 KB content stream, 833 of 10,896 on the font, 697 on the 65 KB
content stream, 0 on the JPEG and 10 on the RGB image, and computing the 11,000 bounds costs more than the
skipped evaluations save (18 KB stream: 121 ms plain, 135 ms with the bound; 65 KB content: 50 against 78;
font: 156 against 176; RGB image: 68 against 134). The differences between candidate cuts are a few hundred
bits on prefixes of hundreds of thousands, far inside the slack of any cheap bound. Not worth doing; the
fixed cost of the split can only be cut by A1 (identical) or C4 (a trade).

**A3. Header search without allocation and without repeated work.** `header_search` runs `dp_tokens` (4
`Vec`s, a `VecDeque`) up to 32 times per `best_dynamic`; the search from different starting strategies
usually meets after one round (the `seen` list already exploits that). Rewrite `dp_tokens` on fixed stack
arrays of 316 entries with u32 costs, compute `cl_lengths` once per distinct token list, and stop a strategy
early when its first round already equals a list seen. Expected: 3 to 5x on the header search = 60 to 70%
off a tiny stream (76% of a 61-byte stream is this), 10 to 15% off a 65 KB content stream. Memory: none.
Format: none. Proof: `compare`; `header_search_is_unchanged_by_the_memory_of_tried_tokens` extended to compare
the old and new searches on random lengths.

**A4. Matcher tables reused and not zeroed.** `find_matches` allocates and zeroes `head` (256 KiB) and `son`
(512 KiB) per call; `son` never needs zeroing (every reached node has both children written before they are
read), `head` can be invalidated by an epoch counter (a `head` entry from an earlier call is ignored when its
epoch differs; positions are already checked against the window). Keep one scratch per thread. Expected:
1 to 1.5 M Ir per call: 10% of a 61-byte stream, 5 x per image for the candidates; for tracemonkey's 377
streams plus 900 candidate encodes about 1.5 s of 10. Memory: the scratch lives as long as the thread (768
KiB per thread) instead of being paged in and out. Format: none. Proof: `compare`; the matcher's brute-force
test run twice on the same scratch.

**A5. Matcher inner loop.** Skip the distance-bucket bookkeeping (`dist_code` per visited node, 8% of the
matcher) when `alternatives` is off; read the 8-byte compare words without the `try_into` checks (slice
chunking); keep `len0`/`len1` in registers. Expected: 10 to 15% off the matcher, which is 64 to 77% of a
quick encode of an image, so 7 to 10% off every candidate quick encode and 3 to 5% off a full encode of an
image. Memory: none. Format: none. Proof: `compare`; the existing `every_reported_match_is_real` and
brute-force tests.

**A6. Parse pass without bounds checks.** 30% of a pass is index and iterator checking (`index.rs`,
`zip`, `enumerate`, `range`, `uint_macros`). Restructure `relax_range` on fixed-size slices taken once per
position and the alternatives' 32-candidate insertion sort on a stack array with explicit lengths; the
arithmetic stays the same f32 additions and comparisons in the same order per element. Expected: 1.25 to
1.4x per pass = 15 to 25% off every stream above 100 KB at effort 100 (where passes are 47 to 86%).
Memory: none. Format: none (f32 results identical: no reassociation). Proof: `compare`, and the AVX2 and
plain paths compared on the test streams (already required to agree).

**A7. Remember the parse when the prices did not change.** In `exact()` the three variants of a round are
parsed with different costs, but when a round brings no improvement the next round's code is the same
code and its first variant repeats the previous round's first variant exactly: today the loop breaks
there, so nothing is wasted at that point; however the polish step re-runs `exact` on merged and split
blocks whose symbol lists are unchanged when the merge is rejected, and `refine` on a block re-parses with
`Costs::entropy` of statistics it has already parsed with when `blend` returns to an earlier histogram.
Keep the last few (costs fingerprint, parse) pairs per block; a repeated fingerprint returns the stored
parse. Expected: small on big streams (2 to 5%), up to 10 to 20% on small blocks in polish. Memory: a few
symbol lists per block. Format: none. Proof: `compare`.

**A8. The remembered plain encode checked without a full inflate.** A memo hit (`deflate_zlib` on the same
input) verifies the stored output by inflating it and comparing; a stored (hash, length, sha256 of the input)
checked against the input is enough to make a wrong reuse impossible and costs one hash of the input instead
of an inflate plus a compare. Expected: 20 ms per big image; negligible overall, but it removes a decode
from the hot path of every image. Memory: none. Format: none. Proof: `compare`.

### Group B: pure changes of the PDF pipeline

**B1. Image candidates in parallel.** The five quick encodes are independent; today they run one after the
other, each parallel over its segments only. With more threads than segments (images under 2 MiB on a
4-thread machine, every image on an 8-thread one) run the candidates on separate threads. Output: the same
(the ranking reads the five sizes in order). Expected: wall time of the predictor search of small images
down by the thread count; nothing for CPU time. Memory: up to five filtered copies at once instead of two
(+3 x the image). Format: none. Proof: `compare` with 1, 2 and 8 threads.

**B2. Decode once for the self-check.** `self_check` decodes every stream of the output; the encoder has
just produced those bytes from known content. Keep the content hash (SHA-256 of the decoded data and of the
dictionary without the encoding keys) for every re-encoded stream and compare hashes, decoding only the
streams that were kept as they were. This is "hashing output streams while writing instead of re-reading"
from the idea list. Expected: the self-check from 1.1 to 1.5 s to about 0.3 s on geotopo (0.5 to 2% of a
run). Memory: 32 bytes per stream. Format: none. Proof: `compare` plus `check_the_checkers.py` still
catching a planted wrong stream (the hash of a wrong stream must differ: use the engine's SHA-256).

**B3. The matcher's lists without the copies and the doubling (pure; memory, not time).** The step list is
reserved at 2 per position and then grows by doubling (64 MB for a 1 MiB segment of the RGB image at effort
100, where the steps needed are between 32 and 64 MB; 16 MB at effort 50); the chain list repeats the
smallest-distance steps (16 MB more); `start` and `chain_start` are 4 MB each and `same` 2 MB (massif,
`matcher.rs` lines 82, 84, 142, 144 and 175). Count the steps in a first pass over the tree walk's results?
No: simpler, grow the list in fixed 4 MB blocks (a `Vec<Vec>` or a reserve that stops doubling, wasting at
most one block) and store the chain as a 1-bit flag per step (the chain is the subsequence of steps longer
than every earlier one, which the walk already knows when it pushes). Expected: the segment's peak from
102 MB to about 60 MB at effort 100 and from 34 to about 26 MB at effort 50, per thread; on the sample's
peak file (issue12963, 169 MB) about a quarter; no time change worth measuring. Format: none. Proof:
`compare`; the matcher tests (`chain_at` must equal the old chain); the space benchmark's peak RSS.

### Group C: trades on the Flate side (output changes; the per-file rule decides)

**C1. Rank the image predictors on sampled rows at effort 100 too.** The largest single item: the predictor
timer is 55% of the 20 slowest files' stream time and 69% of geotopo's, and its five quick encodes about a
quarter of the files' time (3.2). Below effort 100 the ranking already uses bands of rows with a budget of
64 KiB << (effort / 20). The probe (`PDFE_PROBE_PRED`, `probe_pred.log`, per-file logs under `probe/`)
re-ranked every image of 11 slow files (62 images, 131 MB of samples, 3.42 MB of output at the full ranking)
on 256 KiB, 1 MiB and 4 MiB samples and fully encoded the other choice when it differed:

| budget | images at or under the budget (unchanged) | same choice | different choice | bytes lost |
| --- | --- | --- | --- | --- |
| 256 KiB | 25 | 32 | 5 | +5,749 (issue12963's 26 MB scan +3,950 = +5.6% of that stream; geotopo +1,544 and +108; issue1905 +91; freeculture +56) |
| 1 MiB | 37 | 23 | 2 | +4,041 (issue12963 +3,950, issue1905 +91) |
| 4 MiB | 55 | 7 | 0 | 0 |

Expected gain: the quick encodes run on at most the budget instead of the whole image. Weighted by the
images' bytes (`predictor_budget.py` over the 256 ranked images of the 20 files), a 4 MiB budget leaves
68% of today's quick-encode bytes, 1 MiB 34%, 256 KiB 12%; with the quick encodes at about 220 s of
the 818 s that is about 70 s, 145 s and 190 s off the slowest 20 (9%, 18% and 23%), and 7%, 14% and 19% of
the whole set at effort 100. Loss: at 4 MiB none on the 62 images probed (the output is byte-identical
whenever the same predictor wins, so on this evidence a 4 MiB budget is a pure change, proven per file by
`compare`); at 1 MiB +4,041 bytes on 2 images; at 256 KiB +5,749 bytes on 5. Memory: lower (the sample
instead of five whole filtered images). Format: none. Below 4 MiB it fails the strict per-file rule on the
files where the choice differs, so it can only be taken as a decision by the owner, or as a new effort level
(99 = sampled ranking, 100 = whole image). Proof: `compare` against the stored sha256s (4 MiB: identical
expected; smaller budgets: a new stored baseline and the probe's log as the evidence of the loss).

**C2. `nice` for images at effort 100.** `--nice 128` cuts the RGB image 6x (8.6 s to 1.4 s) for +1.56%, the
indexed image 2x for +0.57%, and changes text and fonts by +/-0.02%. Expected: image-heavy files 2 to 5x at
effort 100. Loss: up to +1.6% on images, so too costly in bytes for the owner's rule as a blanket setting;
worth it only as an explicit option (for the browser's "fast" mode). Memory: slightly lower (fewer steps
per position inside long matches). Format: none. Proof: the benchmark at the new effort level, with its own
stored baseline.

**C3. Stop the entropy rounds when the cost stops improving, keep the best.** `--iter 4` moves the six
streams by -9 to +9 bytes against effort 100 (smaller on three: the content stream -9, the vector stream -9,
the RGB image -1; larger on two: the font +2, the indexed image +9; equal on the JPEG) for 0.64 to 0.93x the
time; `--exact 2` costs +2 bytes on one stream. The search already keeps the best and breaks after 4 stuck
rounds; breaking after 2 is the cheap version. Expected 10 to 25% at effort 100. Loss: within +/-0.1% on
the sample, in both directions, so some files will be larger. Memory: none. Format: none. Proof: the
benchmark's per-file rule on the set (if it passes, it is free; the measurement so far says it will not on
every file).

**C4. A cheaper split estimate for the DP, the exact one for the fine tuning.** The Shannon estimate
(`entropy_estimate_bits`, 10x cheaper) for the 10,900 pairs and the Huffman estimate only around the chosen
cuts. Changes block boundaries; blocks are re-parsed and polished afterwards, so the final size moves by a
few bytes either way. Expected: block splitting from 42 to 84% of a small stream at effort 0 down to under
10%: typical PDFs at effort 0 about 2x faster. Loss: unknown until measured; must be run on the set with
the per-file rule. Memory: none. Format: none. Proof: the per-file rule of the benchmark (`compare` will
report the changed files; each must be no larger). If it does not pass, A1 gives the pure half of this gain
(A2 was tried and does not help).

**C5. Bound the candidates per position (libdeflate's near-optimal parser).** Relax only the length codes'
boundary lengths and the longest length for each distance instead of every length: the parse passes on
images (86% of the RGB image at effort 100) would drop 3 to 5x. Expected: the full encodes of big images
2 to 3x at effort 100 (their passes are 77 to 86% of the work), about 25% of the set. Loss: the parse is no
longer optimal under its cost model; libdeflate reports fractions of a percent; unknown here until measured
on the six streams, then the set. Memory: lower (fewer candidate steps kept per position). Format: none.
Proof: the per-file rule of the benchmark; taken only where no file grows, or as a lower effort level.

### Group D: the archive

**D1. The model without bounds checks and with a vector mixer (pure).** 30% of the model's instructions are
index checks and iterator overhead; the mixer's 13-input dot product and weight update are scalar. Fixed-size
arrays with masked indices, the ten contexts' state lookups unrolled, the mixer on 16 lanes of i32 (two AVX2
halves, or a plain loop on `[i32; 16]` that the compiler vectorises; the sums are integer, so the result
is bit-identical, including on WebAssembly without SIMD). Expected: 1.4 to 1.7x on the model = 30 to 40%
of every pack and unpack. Memory: none. Format: none (same arithmetic). Proof: the format fixture
(`tests/pack_format.rs` with `tests/data/format-v9.*`) and `compare` on the archive outputs (identical bytes).

**D2. Prefetch the next nibble's buckets (pure).** At the nibble boundary the ten slots are computed and
read at once; issue prefetches for all ten before the first prediction, and compute the next byte's
`contexts()` hashes before coding the last bit of the current byte where they do not depend on it (the
image contexts depend only on neighbours already known). Expected: depends on the table size: nothing at
2.5 MiB, up to 20 to 30% at 160 MiB (DRAM latency is about a third of the time there, judging by the 2.7 us
per byte on canvas against the 1,400 Ir per bit). Loss: none (pure). Memory: none. Format: none. Proof:
identical outputs; timing on geotopo and multipage.

**D3. One decode instead of two: the restore check as a checked re-run of the same model (trade of
guarantee, not of bytes).** The check exists to prove the archive restores; it costs exactly one encode.
Options: (a) keep it, hide it with a second thread (done today); (b) an option `--check off` for callers that
restore immediately anyway (the site decodes every upload to verify, so the check would run twice);
(c) a cheaper proof: the decoder's model is the encoder's model, so the check can be a decode that reuses the
encoder's already computed predictions only if the coder is the only thing that can disagree, which is not
the case (a model bug would hit both the same way: that is precisely what the check cannot catch today
either). The honest statement: the restore check catches coder and I/O bugs and non-determinism between the
two runs; it does not catch a symmetric model bug. Recommendation: keep it on by default, make it an option,
and in the browser run it in a worker alongside. Expected: 2x CPU where it is switched off; nothing by
default. Loss: none in bytes; the guarantee is weaker only where the caller switches it off and verifies by
restoring. Memory: with the check off, no second model and no restored copy (minus about 10 MiB of tables
and one copy of the decoded bytes). Format: none. Proof: the security suite's damaged-archive runs keep the
check on; `compare` for the default output.

**D4. Memory: one copy instead of four (pure).** (a) `rhist` only needs the last two rows of an image: a
ring of 2 x stride + bpp bytes instead of a byte per modelled byte. (b) The encoder's `out` (the restored
copy built during encoding) is only used for `dict_before` look-backs and a debug assertion: look back into
the input instead (the encoder has it; the decoder keeps `out` because it is the result). (c) Plan streams
lazily: decode a stream when its turn comes, with a bounded look-ahead of a few streams for the parallel
planning (bytes in flight capped, say 64 MiB), instead of holding every decoded stream of the file. (d) The
restore thread's model is a second full model: unavoidable with the check alongside, but (a) to (c) apply to
it too. Expected: enormous.pdf from 949 MB to about 260 MB with 1 thread (history 243 + tables 10 + one
stream in flight) and under 520 MB with 2; the sample's peak the same. Time: slightly faster (fewer page
faults). Format: none. Proof: the space benchmark's peak RSS and `compare` (identical bytes).

**D5. Fail the zlib parameter search early (pure).** zlib flushes its first block after exactly
2^(mem_level + 6) - 1 symbols (our `Tally` limit). Count the symbols of the stream's first deflate block
while inflating (one counter in the inflater); unless the block is the last, its count pins the memory
level to one value (or proves the stream is not zlib at all when the count + 1 is not a power of two), so
the search drops from up to 72 trials to at most 8. Every trial is still verified bit for bit, so the result
is identical. Expected: the 2.6 s of failed trials on multipage's 450 KB stream down to under 0.1 s; for the
2,133 preflated (non-zlib) streams of the set and the streams where the first guess fails, a few percent of
the archive's planning overall and seconds on the files that have them. Memory: none. Format: none.
Proof: `compare` on the archives; a test that every stream of `tests/data` is still rebuilt with the same
parameters.

**D6. Bound preflate (trade of coverage).** preflate costs 25 us per byte and is tried on every non-zlib
stream, then checked by re-creating the stream. A size bound (skip preflate above, say, 4 MB decoded unless
the stream is a large share of the file) and a corrections bound that gives up early are the cheap
controls. Expected: multipage's 11.3 s and issue8795's 2.2 s of preflate gone where the bound bites; the
set's 2,133 preflated streams are mostly small, so a few percent of the archive's planning overall. Loss:
the compression of the streams it would have rebuilt (they are then stored as raw bytes through the model,
like issue19517's 6.3 MB of raw stream bytes: our 2,900,793 against xz's 2,911,732 and zstd's 2,908,163
on that file, so the loss is bounded by the model's own performance on compressed bytes); measured per
file by the benchmark. Memory: lower where
preflate is skipped (its state is many times the stream). Format: none. Proof: the per-file rule, and the
security suite's time and memory bound on damaged archives. This overlaps with the pending "bound
preflate-rs on damaged archives" task.

**D7. Smaller tables for small inputs (format change).** `table_bits` follows the file length, so a 10 MB
file with 1 MB of modelled bytes still gets 160 MiB of tables. Sizing them by the modelled byte count
(written in the header as a new field) is a new format version; compression would be equal or better on
files where the tables were oversized (fewer cold misses, same contexts), and the fixed memory drops to
what the data needs (a browser constraint: 160 MiB of tables plus histories is a quarter of a 32-bit
WebAssembly heap). Expected: time 10 to 20% on files whose tables shrink (fewer cold misses: canvas runs at
2.7 us per byte with 5 MiB of tables), memory from 160 MiB of tables to what the modelled bytes need. Loss:
none expected (the same contexts in smaller tables are a loss only when they collide, which the sizing by
modelled bytes avoids); allowed only with equal or better compression on the set. Format: a new version
with one header field. Proof: the space benchmark's whole-set size and peak RSS against the stored version-4
result, and the format fixture.

**D8. Group the predictable bits (format change).** The long-repeat path codes a byte in one decision when
the match is at least 128 bytes; the measured threshold (32, 64, 128, 256) chose 128 by size on a sample.
Lowering it, or adding a one-decision path for "the byte equals the prediction" in images, trades bits for
time. Expected: up to 8x on the bytes that take the one-decision path (one decision instead of eight);
on the set the images are 46 of geotopo's 77 s encode, so tens of percent on image-heavy archives. Loss:
bits where the grouped decision mispredicts; the 128 threshold was chosen by size, so lowering it costs
bytes on some files. Memory: none. Format: a new version. Proof: the whole-set size proof of the space
benchmark before any version bump; the format fixture.

### Group E: wall time in the browser

- WebAssembly: one thread unless the site enables cross-origin isolation for `SharedArrayBuffer`; then the
  pool works as on native (threads are standard-library only), and the restore check hides behind the encode.
- 32-bit addresses, 2 to 4 GB: the archive's four copies (D4) and the 160 MiB tables (D7) are the two things
  that can exceed it on a 250 MB decoded image; the PDF path's 100 bytes per input byte of a segment is
  bounded (1 MiB segments) and fine.
- AVX2 is unavailable; the same Rust code runs on WebAssembly SIMD (128-bit) where enabled, so the parse
  relaxation runs at a quarter of the native width: the pure items A1 to A7 matter more there, not less.

## 9. Order of work and predicted totals

Order (gain per unit of risk, pure first):

1. A1 + A3 + A4 (fixed costs): typical files 1.8 to 2.5x at effort 0, 1.3x at effort 100; two days.
2. A6 + A5 (the inner loops): streams above 100 KB 1.2 to 1.4x at effort 100; one to two days.
3. D1 + D4 (the model's loops and the archive's memory): archive 1.4 to 1.7x, memory 4x down; two days.
4. A7, A8, B2, B3, D2, D5: smaller pure gains (B3 is memory, not time); B1 for wall time where there are
   more threads than segments; one day each. (A2 was tried and is not on the list.)
5. Then the trades, each as a separate measured run of the benchmark with the per-file rule: C1 first,
   starting at the 4 MiB budget (no byte changed on the 62 images probed, 7% of the set at effort 100) and
   going lower only by the owner's decision (1 MiB: 14% for +4,041 bytes on 2 of 62 images; 256 KiB: 19%
   for +5,749 bytes on 5); then C3, C4, C5, D6; D7 and D8 only with a format version and the whole-set size
   proof.

Predicted CPU totals for the 907 files, 1 thread. The arithmetic for effort 100 (today about 1,010 s =
818 the slowest 20 + 15 the 14 other largest files + 178 the remaining 787 files at the random files'
rate): the slowest 20 are quick encodes of image candidates (about 220 s: matcher 64 to 77%, split 13%,
memset 3%, parse 5 to 20%), full encodes of images (main and winner, about 480 s: parse passes 77 to 86%,
matcher 12 to 19%), other streams (about 100 s, fixed costs) and about 20 s of non-Flate work (decoding,
the self-check, parsing and writing: 2 to 3%); the rest of the set (193 s) is about 110 s of fixed costs,
65 s of passes and 18 s of non-Flate work. A1 + A4 + A5 take the quick encodes to 0.77x, A6 + A5 the full
encodes to 0.78x, A1 + A3 + A4 the fixed costs to 0.5x, the passes elsewhere to 0.78x: 170 + 375 + 50 + 20 +
55 + 50 + 18 = about 740 s. C1 takes the quick encodes to 0.68x (4 MiB), 0.34x (1 MiB) or 0.12x (256 KiB)
of their 170 s: about 690, 630 or 590 s. C3 (0.8x on the full encodes) and C5 (3 to 5x on their parse
passes) would take the full encodes from 375 to about 120 s: about 430 s with C1 at 4 MiB, about 350 s with
C1 at 256 KiB. Effort 0 (233 s today: 139 + 7 + 87) is matcher-bound on the image files (A5 and A1 take
their 139 s to about 0.8x) and fixed-cost-bound elsewhere (A1 + A3 + A4: 0.5x): about 160 s after the pure
items, about 130 s with C4 (the split's share of the other files from 42 to 73% to under 10%). The archive
(650 s: the model is about 85% of pack plus check) goes to 400 to 450 s with D1 (1.4 to 1.7x on the model)
and D2, and the 2-thread wall time is the encode alone, 210 to 240 s.

| | PDF effort 100 | PDF effort 0 | archive |
| --- | --- | --- | --- |
| today (this build) | ~1,010 s | ~230 s | ~650 s (600 to 700) |
| after the pure items (A1, A3 to A8, B2, B3, D1, D2, D4, D5) | ~740 s | ~160 s | 400 to 450 s |
| plus C1 at 4 MiB (no byte changed on the 62 images probed) | ~690 s | same | |
| plus C1 at 1 MiB (+4,041 bytes on 2 of 62) or 256 KiB (+5,749 bytes on 5 of 62) | ~630 / ~590 s | same | |
| plus C3 to C5 if they pass the per-file rule | ~430 s (C1 at 4 MiB) to ~350 s (C1 at 256 KiB) | ~130 s | |
| with 2 threads, wall | about half | about half | 210 to 240 s (the check alongside) |
| qpdf / xz for comparison | 25 s | 25 s | 57 s |

The pure items cannot bring effort 100 near qpdf's 25 s: at effort 100 the engine runs 100 to 200 optimal
parse passes per stream where qpdf runs zlib once, and it encodes every big image seven times; after
everything pure we would still be about 30x qpdf at effort 100 and 6x at effort 0. Getting closer needs the
trades or a lower default effort, and the measurement says effort 0 already gives 90.70% against effort
100's 90.23% on the sample (effort 50: 90.33%): the owner's choice is between the last half percent and a
3x in time (effort 50, 363 s today, is predicted at about 250 s after the pure items: its slowest 20 are
full encodes without alternatives, 0.8x, and the rest fixed costs, 0.5x).

Predicted peak memory: archive from 950 MB to about 260 MB with one thread (D4), 160 MiB of it tables on big
files (D7 would make that follow the data); PDF path from 169 MB on the sample to about 130 MB with B3 (the
segment's lists from 102 to about 60 MB per thread), and a further pure step is open: storing each step as
3 bytes (u16 length, u8 distance code, the 16-bit distance shared by the pair) instead of 4 plus the chain
copy, worth another 10 to 15% of the top efforts' peak.

## 10. Files

- `profile-data/instrumentation.patch`: the measuring patch on the `profile` worktree (not
  committed; `git diff` of the worktree, timers and the predictor probe); `examples/`: the three measuring
  programs that live untracked in the worktree (`probe_split.rs`, `probe_lengths.rs`, `filter_image.rs`).
- `runs/optimize-e100-t1.tsv` (20 files), `runs/optimize-e50-t1.tsv` (7 files), `runs/pack-e100-t1.tsv`
  (20 files) and `runs/logs/`: rusage and the engine's own timings; `runs/*.analysis.txt`: per file, from
  `analyse_runs.py`; `phases.py`: the phase shares of every optimize log.
- `streams/`: the extracted streams; `streams.tsv`, `stream-logs/`: efforts 0 to 100; `knobs.tsv`: the
  switches at effort 100.
- `callgrind/*.out` and `*.log`: callgrind data; `cg_summary.py` prints inclusive and self shares
  (`pack-tracemonkey.e100` was cut off by the restart and is not used).
- `massif/*.txt`: ms_print reports; `ms_peak.py` prints the peak's allocation tree.
- `probe/*.log`, `probe_pred.log`: the predictor-ranking probe (complete: 62 images, 11 files);
  `predictor_budget.py` and `.log`: the share of the quick encodes each budget keeps; `probe_split.log`:
  the split-pruning probe (A2).
- `bench_subsets.py` and `.log`: the time benchmark's sample records summed over the subsets used in
  section 1 (the 20 slowest, the 20 largest, the 86 random files) and the per-MB rates of the predictions.
- `run_files.py`, `time_streams.py`, `knobs.py`, `analyse_runs.py`, `find_streams.py`, `extract.py`: the
  scripts; `build.log`.
