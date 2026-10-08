# Archive: the model's loops (D1, D2)

Branch `opt-model-loops`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

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
full sample. Work on a branch named `opt-model-loops`.

## The change

Two pure speed-ups of the context-mixing model (`src/cm/`), from `docs/optimisation-plan.md` section 8,
group D. The model is part of the archive format: a faster model has to compute exactly the same thing, and
`tests/pack_format.rs` holds a stored archive (`tests/data/format-v7.*`) that must still pack to the same
bytes and restore.

**D1. The model without bounds checks and with a vector mixer.** About 30% of the model's 1,400
instructions per coded bit are index checks and iterator overhead; the mixer's 13-input dot product and
weight update are scalar. Fixed-size arrays with masked indices, the ten contexts' state lookups unrolled,
the mixer on 16 lanes of i32 (two AVX2 halves, or a plain loop on `[i32; 16]` that the compiler vectorises;
the sums are integer, so the result is bit-identical, including on WebAssembly without SIMD). Expected: 1.4
to 1.7x on the model, 30 to 40% of every pack and unpack. Memory: none. Format: none (same arithmetic).
Proof: the format fixture and `compare` on the archive outputs (identical bytes).

**D2. Prefetch the next nibble's buckets.** At the nibble boundary the ten slots are computed and read at
once; issue prefetches for all ten before the first prediction, and compute the next byte's `contexts()`
hashes before coding the last bit of the current byte where they do not depend on it (the image contexts
depend only on neighbours already known). Expected: nothing at 2.5 MiB of tables, up to 20 to 30% at 160 MiB
(DRAM latency is about a third of the time there). Loss: none. Memory: none. Format: none. Proof: identical
outputs; timing on `pdfjs/geotopo.pdf` and `ocrmypdf/tests__resources__multipage.pdf` (`files.tsv` of the
two runs).

Integer arithmetic only, as now; no floating point anywhere in the model. The decoder and the encoder share
every loop, so a change to one is a change to both. Do D1 first, then D2, each a commit; the archive tests
(`tests/pack_format.rs`, `tests/bounds.rs`, `tests/encryption.rs`, `tests/fax.rs`, `tests/type1_fonts.rs`,
`tests/interaction.rs`) must stay green after each.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- every output byte-identical to the baseline (the `compare` line must count all 120 files in every setting as byte-identical);
- peak memory not up (`compare` fails over 10%);
- the archive settings of the sample at least 1.3x faster in CPU time in total at effort 100 (pack, and the separate unpack the benchmark measures), and nothing slower than the baseline by more than the 10% `compare` allows;
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-model-loops` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
