# Normal PDF: rank the image predictors on sampled rows at effort 100 (C1)

Branch `opt-predictor-sample`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

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
full sample. Work on a branch named `opt-predictor-sample`.

## The change

The largest single item of `docs/optimisation-plan.md` (section 8, C1): the predictor search is 55% of the 20
slowest files' stream time at effort 100 and 69% of geotopo's. Below effort 100 the ranking already uses
bands of rows with a budget of 64 KiB << (effort / 20) (`predictor_budget`, `predictor_sample` in
`src/pdf/rewrite.rs`); at 100 it quick-encodes all five candidates on the whole image. The plan's probe
re-ranked every image of 11 slow files (62 images, 131 MB of samples) on 256 KiB, 1 MiB and 4 MiB samples:

| budget | images at or under the budget (unchanged) | same choice | different choice | bytes lost |
| --- | --- | --- | --- | --- |
| 256 KiB | 25 | 32 | 5 | +5,749 (issue12963's 26 MB scan +3,950; geotopo +1,544 and +108; issue1905 +91; freeculture +56) |
| 1 MiB | 37 | 23 | 2 | +4,041 (issue12963 +3,950, issue1905 +91) |
| 4 MiB | 55 | 7 | 0 | 0 |

Make the effort-100 ranking use a sample too, with the budget an option of `pdf::rewrite::Options` (and
through `gpress::Options`), defaulting to 4 MiB: on the evidence above that changes no byte on the 62 images
probed, so it is taken as a pure change and `compare` must prove it on the sample. Expected: the quick
encodes run on at most the budget instead of the whole image; weighted by the images' bytes over the 256
ranked images of the 20 slowest files, a 4 MiB budget leaves 68% of today's quick-encode bytes, so about 70 s
off the slowest 20 (9%) and 7% of the whole set at effort 100. Memory: lower (the sample instead of five
whole filtered images). Format: none. Proof: `compare` against the baseline (identical expected).

Smaller budgets are trades the owner decides: 1 MiB gives 14% of the set for +4,041 bytes on 2 of 62
images, 256 KiB 19% for +5,749 bytes on 5. Do not change the default below 4 MiB on this branch; the option
makes them available for a measured run, and the report may carry such a run as information, with its
per-file size changes, clearly separate from the acceptance run.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- every output byte-identical to the baseline (the `compare` line must count all 120 files in every setting as byte-identical);
- peak memory not up (`compare` fails over 10%);
- the 20 slowest files of the sample at effort 100 at least 1.08x faster in CPU time in total;
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-predictor-sample` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
