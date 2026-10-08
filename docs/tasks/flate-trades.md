# Flate: the trades (C3, C4, C5), each as its own measured run

Branch `opt-flate-trades`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

## Setup (a fresh clone)

    git clone <the repository> gpress && cd gpress
    cargo build --release --features existing
    cargo test --release --workspace && cargo test --release --workspace --features existing
    bench/fetch_corpus.sh && export CORPUS=$PWD/bench/corpus      # 907 public PDFs, pinned in bench/sets/corpus.lock
    export GPRESS=$PWD/target/release/gpress

The corpus comes from pdf.js `test/pdfs`, pypdf `sample-files`, OCRmyPDF `tests/resources` and pikepdf
`tests/resources`, each at the commit named in `bench/sets/corpus.lock`; the script checks every sha256 and
`bench/fetch_corpus.sh --check` proves a copy is complete. The files are untrusted: never open or run them,
and run the benchmark scripts with `python3 -I`. The release profile of `Cargo.toml` is the measured build;
build the baseline and the change the same way.

## Baseline first

    python3 -I bench/benchmark.py time base --set sample120 --who ours --efforts 0,50,100 --threads 1

The 120-file sample at three efforts, normal PDF and archive, one thread, with the machine calibration
(`meta.json`); about an hour on two CPUs. `--limit 20` is enough while iterating; the acceptance run is the
full sample. Work on a branch named `opt-flate-trades`.

## The change

Three changes to the Flate encoder that may move bytes, from `docs/optimisation-plan.md` section 8, group C.
Each is its own commit, its own benchmark run and its own `compare`; the per-file rule decides each one
separately, and one that fails it is reverted (or kept behind an option that is off by default, with the
measurement in the report), never merged as a default. The plan's own numbers say C3 moves bytes both ways
on the six probe streams (-9 to +9), so expect some of these to fail the rule as blanket defaults.

**C3. Stop the entropy rounds when the cost stops improving, keep the best** (`deflate.rs`, `refine` and
`exact`). `--iter 4` moves the six probe streams by -9 to +9 bytes against effort 100 for 0.64 to 0.93x the
time; `--exact 2` costs +2 bytes on one stream. The search already keeps the best and breaks after 4 stuck
rounds; breaking after 2 is the cheap version. Expected 10 to 25% at effort 100. Loss: within +/-0.1% on the
probe, in both directions, so some files will be larger. Memory: none. Format: none. Proof: the per-file rule.

**C4. A cheaper split estimate for the DP, the exact one for the fine tuning** (`deflate.rs`, `split`). The
Shannon estimate (`entropy_estimate_bits`, 10x cheaper) for the 10,900 pairs and the Huffman estimate only
around the chosen cuts. Changes block boundaries; blocks are re-parsed and polished afterwards, so the final
size moves by a few bytes either way. Expected: block splitting from 42 to 84% of a small stream at effort 0
down to under 10%: typical PDFs at effort 0 about 2x faster. Loss: unknown until measured. Memory: none.
Format: none. Proof: the per-file rule (if it fails, A1 in `flate-fixed-costs.md` gives the pure half of
this gain; A2, pruning the DP with a lower bound, was tried and does not help).

**C5. Bound the candidates per position (libdeflate's near-optimal parser)** (`parse.rs`). Relax only the
length codes' boundary lengths and the longest length for each distance instead of every length: the parse
passes on images (86% of the RGB image at effort 100) would drop 3 to 5x. Expected: the full encodes of big
images 2 to 3x at effort 100, about 25% of the set. Loss: the parse is no longer optimal under its cost model;
libdeflate reports fractions of a percent; unknown here until measured. Memory: lower. Format: none. Proof:
the per-file rule; taken only where no file grows, or as a lower effort level.

A trade that passes the rule on the sample should be run on the whole set (`--set all`, several hours)
before it is merged as a default; the report says which runs were made. A trade that fails may still be
offered as a new effort level below 100 (the effort steps are in `flate::Options::effort`): say so in the
report rather than deciding it.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- no output larger than the baseline on any file of the sample (`compare` fails on one that is), the changed files listed with their sizes;
- peak memory not up (`compare` fails over 10%);
- for each trade kept as a default, `compare` passes on the full sample (no file larger, total CPU time not worse, peak memory not up) and the report lists every file whose output changed with the old and new sizes;
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-flate-trades` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
