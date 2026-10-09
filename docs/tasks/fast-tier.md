# Fast tier: effort 0 in qpdf's time, and smaller than qpdf (F1 to F5)

Branch `opt-fast-tier`. The rules and the procedure in full: `docs/tasks/README.md`. This brief is the
owner's first priority: "No users will wait this much time for a PDF compression. Also it must be
lightweight."

## Setup (a fresh clone)

    git clone <the repository> gpress && cd gpress
    cargo build --release --features existing
    cargo test --release --workspace && cargo test --release --workspace --features existing
    bench/fetch_corpus.sh && export CORPUS=$PWD/bench/corpus      # 907 public PDFs, pinned in bench/sets/corpus.lock
    export GPRESS=$PWD/target/release/gpress

The corpus files are untrusted: never open or run them; the benchmark scripts run under `python3 -I`. The
checking tools the benchmark needs are listed in `docs/tasks/README.md`; install them first, or the checks
columns stay empty.

Baseline before any change (about an hour on two CPUs):

    python3 -I bench/benchmark.py time base --set sample120 --who ours --efforts 0,10,20,50,100 --threads 1

## Why

Measured on the 120-file sample (`bench/sets/sample120.txt`, 78.3 MB), one thread, CPU time, on the build
this brief starts from:

| | Whole sample | Files of 1 to 10 MB: median / p90 / slowest | Saved on the 90 files gpress rewrites |
| --- | ---: | --- | ---: |
| gpress, effort 0 (today's fastest) | 187 s | 1.46 s / 12.0 s / 63.8 s | 14.3% |
| qpdf, strongest lossless setting | 24 s | 0.21 s / 1.58 s / 6.5 s | 12.9% |

A 1 MB paper (`pdfjs/tracemonkey.pdf`) takes 12 s at effort 0 and a 150 KB file (`pdfjs/canvas.pdf`) 6.8 s.
Effort 0 still runs the optimal parser, the block-split search and a trial encode per image predictor: it is
the slowest setting of a strong encoder, not a fast encoder. This brief adds a real fast path.

## Targets (all measured with `benchmark.py` on the sample, one thread, CPU time, same machine as the baseline)

Measure qpdf on the same machine first, so the targets compare like with like: install qpdf from the package
manager; if `bench/benchmark.py`'s competitor table has no `qpdf-max` entry yet, add one beside `qpdf` with the
command `qpdf --object-streams=generate --compress-streams=y --recompress-flate --compression-level=9
--decode-level=specialized {in} {out}` (qpdf's strongest lossless setting), then run
`python3 -I bench/benchmark.py time base --set sample120 --who qpdf-max` next to the baseline. The numbers
below are this machine's; on another machine, scale them by qpdf's own time there.

- **T1, whole sample at effort 0:** at most qpdf's strongest setting (24 s here); stretch goal half of it.
- **T2, time to result at effort 0:** files under 1 MB p90 at most 0.15 s; files of 1 to 10 MB median at most
  0.25 s and p90 at most 1.5 s; no file over 5 s.
- **T3, size at effort 0:** on the files gpress rewrites, the total at most qpdf's verified total (87.1% of
  their input; today's effort 0 gives 85.7%, so about 1.4 points can be traded for speed and no more), and no
  output larger than its input (as today).
- **T4, no data loss:** every output passes `bench/verify_pdf.py` and `bench/verify_render.py` (the
  benchmark's check columns); the engine's self-check stays on.
- **T5, lightweight:** peak memory at effort 0 not above today's (95 MB on the sample), lower where the fast
  path allows.
- **T6, nothing else moves:** efforts 21 to 100 byte-identical to the baseline; effort 100 above all.

## The work

**F1. A fast Flate encoder of our own** in a new file, `src/flate/fast.rs`: a hash-chain match finder (3- or
4-byte hashes, a chain-length and a nice-length limit per level), lazy matching (zlib's lazy evaluation, or
libdeflate's greedy plus one-step lookahead), block ends chosen from the symbol statistics as they change
(libdeflate's observation-based split: no search) within a minimum and maximum block size, the existing
length-limited Huffman code builder and block writer. Aim for at least 50 MB/s on text-like data on one core.
Round-trip tests against our inflate and zlib on random data, corpus streams and every size from 0 to 70,000
bytes; identical output for any thread count.

**F2. Image predictors without trial encodes** at effort 0: pick the PNG predictor per row (or one per image)
from a cheap estimate (the sum of absolute residuals per row, libpng's heuristic, or an entropy estimate on
sampled rows); keep "no predictor" when that estimate says so; count the parameters' bytes as today.

**F3. Only the cheap steps at effort 0.** No exact-zlib search (the archive needs it, a normal PDF doesn't), no
header search, no polishing rounds. Keep what is cheap and pays: object streams, the compressed cross-reference
stream, JPEG Huffman tables (about 0.07 s per MB; measure it on the sample and drop it at effort 0 only if it
breaks T1). The self-check stays, and decodes each stream once.

**F4. Leave alone what won't shrink.** A stream already compressed by Flate whose fast re-encode is not smaller
is kept as it is, decided from sizes alone, so time isn't spent on lost causes and T3 still holds.

**F5. The effort scale.** Effort 0 is the fast path. Efforts 1 to 20 bridge to the strong encoder (longer
chains, a near-optimal parse with a bounded number of candidates per position, libdeflate's level 10 to 12
idea), so that time and size both move smoothly with effort. Efforts 21 to 100 are today's encoder,
unchanged. Write the mapping in `README.md` and in the `Options::effort` documentation.

The site will call effort 0 with a time budget first and offer more effort to whoever wants to wait; the
library already has `time_budget`, keep it working on the fast path.

## Where it lives

`src/flate/fast.rs` (new), the effort plan in `src/pdf/rewrite.rs`, the predictor choice in
`src/pdf/images.rs`, the effort documentation in `src/api.rs` and `README.md`. Other sessions change
`src/flate/{block,huffman,matcher,parse,deflate}.rs` and parts of `src/pdf/` at the same time: keep the edits
in shared files small and in their own commits.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,10,20,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

- T1 to T6 hold, with the numbers in the report (whole-sample CPU, the time-to-result percentiles per size
  class, the saved percentage against qpdf's on the same files, peak memory, effort 21 to 100
  byte-identical);
- both test suites, clippy on both feature sets, `cargo fmt --all --check`;
- `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

## Report

A pull request against `main` from `opt-fast-tier` carrying the `compare` output, the time-to-result table per
size class against qpdf's, the size against qpdf's verified outputs, and the test, clippy and security lines.
