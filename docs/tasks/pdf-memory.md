# Normal PDF: the matcher's lists without the copies and the doubling (B3)

Branch `opt-pdf-memory`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

## Setup (a fresh clone)

    git clone <the repository> gpress && cd gpress
    cargo build --release --features existing
    cargo test --release --workspace && cargo test --release --workspace --features existing
    bench/fetch_corpus.sh && export CORPUS=$PWD/bench/corpus      # 907 public PDFs, pinned in bench/sets/corpus.lock
    export GPRESS=$PWD/target/release/gpress
    # plus the checking tools (qpdf, xz, brotli, zstd, a C compiler, pikepdf, pypdfium2, Pillow, libjpeg-turbo's
    # jpegtran, pdf.js with PDFJS_MODULES): the install lines are in docs/tasks/README.md, step 2. benchmark.py
    # warns about each missing one; a baseline made without them does not run the "no data loss" checks.

The corpus comes from pdf.js `test/pdfs`, pypdf `sample-files`, OCRmyPDF `tests/resources` and pikepdf
`tests/resources`, each at the commit named in `bench/sets/corpus.lock`; the script checks every sha256 and
`bench/fetch_corpus.sh --check` proves a copy is complete. The files are untrusted: never open or run them,
and run the benchmark scripts with `python3 -I`. The release profile of `Cargo.toml` is the measured build;
build the baseline and the change the same way.

## Baseline first

    python3 -I bench/benchmark.py time base --set sample120 --who ours --efforts 0,50,100 --threads 1

The 120-file sample at three efforts, normal PDF and archive, one thread, with the machine calibration
(`meta.json`); about an hour on two CPUs. `--limit 20` is enough while iterating; the acceptance run is the
full sample. Work on a branch named `opt-pdf-memory`.

## The change

A pure memory change to the Flate match finder (`src/flate/matcher.rs`), from `docs/optimisation-plan.md`
section 8, item B3. The step list is reserved at 2 per position and then grows by doubling (64 MB for a 1 MiB
segment of the RGB image at effort 100, where the steps needed are between 32 and 64 MB; 16 MB at effort
50); the chain list repeats the smallest-distance steps (16 MB more); `start` and `chain_start` are 4 MB each
and `same` 2 MB. Grow the list in fixed 4 MB blocks (a `Vec<Vec>` or a reserve that stops doubling, wasting
at most one block) and store the chain as a 1-bit flag per step (the chain is the subsequence of steps longer
than every earlier one, which the walk already knows when it pushes).

Expected: the segment's peak from 102 MB to about 60 MB at effort 100 and from 34 to about 26 MB at effort
50, per thread; on the sample's peak file (`pdfjs/issue12963.pdf`, 169 MB) about a quarter; no time change
worth measuring. Format: none. Proof: `compare` (identical bytes); the matcher tests (`chain_at` must equal
the old chain); the space benchmark's peak RSS.

A further pure step is open once this is in: storing each step as 3 bytes (u16 length, u8 distance code,
the 16-bit distance shared by the pair) instead of 4 plus the chain copy, worth another 10 to 15% of the top
efforts' peak. Leave it for a second commit on the same branch if the first lands cleanly.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- every output byte-identical to the baseline (the `compare` line must count all 120 files in every setting as byte-identical);
- peak memory not up (`compare` fails over 10%);
- the normal-PDF peak memory on the sample (`memory.tsv`, the `ours-pdf` rows) at effort 100 at least 20% below the baseline's, and `pdfjs/issue12963.pdf` at effort 100 at least a fifth below its baseline peak; total CPU time within the 10% `compare` allows either way;
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run new-security --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-pdf-memory` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
