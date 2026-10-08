# Archive: one copy of the decoded bytes instead of four (D4)

Branch `opt-archive-memory`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

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
full sample. Work on a branch named `opt-archive-memory`.

## The change

A pure memory change to the archive encoder (`src/pack/mod.rs`, `src/cm/model.rs`), from
`docs/optimisation-plan.md` section 8, item D4. When the plan was written the pack held four copies of every
decoded byte (the decoded plans, the model's history, the residual history, and the restored copy the
encoder builds): 949 MB resident for `ocrmypdf/tests__resources__enormous.pdf` (311 KB, one image that
decodes to 243 MB), and the time benchmark's whole sample peaked there (950 MB; xz 136 MB). Two threads add
a second model and a second restored copy (1,086 MB).

Two of the four copies are gone since the review fixes that followed the plan (format version 8): the
residual history now lives per segment (`rhist` is cleared at every `set_kind`, since no residual is read
across a segment boundary), and the planner keeps no decoded data at all (a `Plan::Rebuilt` holds the
stream's zlib settings and decoded length, and the stream is inflated again when its turn comes; the
`max_decoded` budget in `PackOptions` and `gpress::Options` bounds what the parts of one archive may claim
in all, on both sides). Measured on a 154 KB file with three 50 MiB streams: pack 549 MB to 368 MB, unpack
382 MB to 283 MB with one thread. What is left:

(a) `rhist` as a ring of 2 x stride + bpp bytes instead of a byte per modelled byte of the current image part
(a 243 MB image still keeps 243 MB of residuals while it is coded). (b) The encoder's `out` (the restored
copy built during encoding) is only used for `dict_before` look-backs and a debug assertion: look back into
the input instead (the encoder has it; the decoder keeps `out` because it is the result). (c) The per-part
buffers: a decoded stream is held whole while it is modelled (`inflated`, then `unfilter` makes a second
copy for an image with a PNG predictor), so one part costs up to twice its size on top of the history;
feeding the model row by row from the inflated data would take that to one row. (d) The restore thread's
model is a second full model: unavoidable with the check alongside, but (a) to (c) apply to it too.

Expected: enormous.pdf to about 260 MB with 1 thread (history 243 + tables 10 + one row in flight) and under
520 MB with 2; the sample's peak the same. Time: slightly faster (fewer page faults). Format: none (the bytes
coded are the same, in the same order). Proof: the benchmark's peak RSS (`memory.tsv` of the run) and
`compare` (identical bytes).

This also closes the memory finding of the security suite's notes (`bench/results/security/notes.md`): the
two large scans left out of the fuzz seeds because their clean pack needs more than 512 MiB. Measure each of
(a), (b), (c) as its own commit on enormous.pdf with
`python3 -I bench/benchmark.py space probe --set <a list with that file> --who ours --modes archive`.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- every output byte-identical to the baseline (the `compare` line must count all 120 files in every setting as byte-identical);
- peak memory not up (`compare` fails over 10%);
- the archive's peak memory on the sample (`memory.tsv`, the `ours-archive` rows) at most a third of the baseline's at every effort with one thread, and `enormous.pdf` under 300 MB with one thread; after the change, add `ocrmypdf/tests__resources__enormous.pdf` and `ocrmypdf/tests__resources__multipage.pdf` to the fuzz seeds by running `python3 -I bench/security.py fuzz --fresh` and showing them kept;
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run new-security --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-archive-memory` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
