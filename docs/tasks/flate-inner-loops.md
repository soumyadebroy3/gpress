# Flate: the inner loops (A5, A6)

Branch `opt-flate-inner-loops`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

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
full sample. Work on a branch named `opt-flate-inner-loops`.

## The change

Two pure speed-ups of the Flate encoder's hot loops (`src/flate/`), from `docs/optimisation-plan.md` section
8, group A. The arithmetic stays the same, in the same order, so the output bytes do not change.

**A5. Matcher inner loop.** In `find_matches` (`matcher.rs`): skip the distance-bucket bookkeeping
(`dist_code` per visited node, 8% of the matcher) when `alternatives` is off; read the 8-byte compare words
without the `try_into` checks (slice chunking); keep `len0`/`len1` in registers. Expected: 10 to 15% off the
matcher, which is 64 to 77% of a quick encode of an image, so 7 to 10% off every candidate quick encode and 3
to 5% off a full encode of an image. Memory: none. Format: none. Proof: `compare`; the existing
`every_reported_match_is_real` and brute-force tests.

**A6. Parse pass without bounds checks.** 30% of a parse pass (`parse.rs`) is index and iterator checking
(`index.rs`, `zip`, `enumerate`, `range`, `uint_macros`). Restructure `relax_range` on fixed-size slices
taken once per position and the alternatives' 32-candidate insertion sort on a stack array with explicit
lengths; the arithmetic stays the same f32 additions and comparisons in the same order per element (no
reassociation, so the results are bit-identical). Expected: 1.25 to 1.4x per pass, 15 to 25% off every stream
above 100 KB at effort 100 (where the passes are 47 to 86% of the work). Memory: none. Format: none. Proof:
`compare`, and the AVX2 and plain paths compared on the test streams (already required to agree; keep that
test and run it on both paths).

No `unsafe` is needed for either; if a bounds check can only go with `get_unchecked`, prefer a slice taken once
and indexed with a proven-in-range loop, and if `unsafe` is still the only way, give the block its reason on
the line above (the security suite counts `unsafe` blocks without one as failures). Do A6 first (the larger
gain), then A5, each a commit with its test.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- every output byte-identical to the baseline (the `compare` line must count all 120 files in every setting as byte-identical);
- peak memory not up (`compare` fails over 10%);
- the 20 slowest files of the sample at effort 100 at least 1.15x faster in CPU time in total, and nothing slower than the baseline by more than the 10% `compare` allows;
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-flate-inner-loops` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
