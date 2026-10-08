# Archive: one copy of the decoded bytes instead of four (D4)

Branch `opt-archive-memory`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

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
full sample. Work on a branch named `opt-archive-memory`.

## The change

A pure memory change to the archive encoder (`src/pack/mod.rs`, `src/cm/model.rs`), from
`docs/optimisation-plan.md` section 8, item D4. Today the pack holds four copies of every decoded byte
(the decoded plans, the model's history, the residual history, and the restored copy the encoder builds):
949 MB resident for `ocrmypdf/tests__resources__enormous.pdf` (311 KB, one image that decodes to 243 MB), and
the time benchmark's whole sample peaks there (950 MB; xz 136 MB). Two threads add a second model and a
second restored copy (1,086 MB).

(a) `rhist` only needs the last two rows of an image: a ring of 2 x stride + bpp bytes instead of a byte per
modelled byte. (b) The encoder's `out` (the restored copy built during encoding) is only used for
`dict_before` look-backs and a debug assertion: look back into the input instead (the encoder has it; the
decoder keeps `out` because it is the result). (c) Plan streams lazily: decode a stream when its turn
comes, with a bounded look-ahead of a few streams for the parallel planning (bytes in flight capped by an
option, say 64 MiB), instead of holding every decoded stream of the file. (d) The restore thread's model
is a second full model: unavoidable with the check alongside, but (a) to (c) apply to it too.

Expected: enormous.pdf from 949 MB to about 260 MB with 1 thread (history 243 + tables 10 + one stream in
flight) and under 520 MB with 2; the sample's peak the same. Time: slightly faster (fewer page faults).
Format: none (the bytes coded are the same, in the same order). Proof: the benchmark's peak RSS
(`memory.tsv` of the run) and `compare` (identical bytes).

This also closes the memory finding of the security suite's notes (`bench/results/security/notes.md`): the
two large scans left out of the fuzz seeds because their clean pack needs more than 512 MiB, and the fact
that the planner holds every decoded stream of a file at once, so memory follows what the input claims (up
to `max_stream_archive` per stream, 1032 bytes per byte of the file in all) rather than the input's size.
The look-ahead cap is an option (`PackOptions`, and through `gpress::Options`), never a constant. Do (a),
(b), (c) as separate commits, each measured on enormous.pdf with
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
- `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-archive-memory` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
