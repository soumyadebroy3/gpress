# Normal PDF: the image candidates in parallel (B1)

Branch `opt-image-candidates-parallel`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

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
full sample. Work on a branch named `opt-image-candidates-parallel`.

## The change

A pure wall-time change to the predictor search (`src/pdf/rewrite.rs`, `repredict`), from
`docs/optimisation-plan.md` section 8, item B1. The five quick encodes of an image's candidates (no
predictor, PNG per-row minimum-sum, Up, Sub, Paeth) are independent; today they run one after the other,
each parallel over its segments only. With more threads than segments (images under 2 MiB on a 4-thread
machine, every image on an 8-thread one) run the candidates on separate threads through the job's existing
pool (`src/par.rs`; never more threads than the job was given). The output is the same: the ranking reads the
five sizes in order, and a thread the system refuses is no loss (the calling thread does the work).

Expected: the wall time of the predictor search of small images down by the thread count; nothing for CPU
time. Memory: up to five filtered copies at once instead of two (+3 x the image); keep that within the
`compare` bound on the sample (tracemonkey's strips are small; the big images already run their segments in
parallel and need not change). Format: none. Proof: `compare` with 1, 2 and 8 threads (identical outputs),
and the wall-time column of the time benchmark with 2 threads.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- every output byte-identical to the baseline (the `compare` line must count all 120 files in every setting as byte-identical);
- peak memory not up (`compare` fails over 10%);
- with `--threads 2` on the sample (`time base2` / `time new2` with `--threads 2`, compared with `--metric wall`) the wall time of `pdfjs/tracemonkey.pdf` and `pdfjs/canvas.pdf` at effort 100 at least 1.3x lower, CPU time within the 10% `compare` allows, and `compare` of the one-thread runs identical;
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-image-candidates-parallel` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
