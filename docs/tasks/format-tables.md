# Archive: smaller tables for small inputs and grouped decisions (D7, D8; a format change)

Branch `opt-format-tables`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

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
full sample. Work on a branch named `opt-format-tables`.

## The change

Two archive changes from `docs/optimisation-plan.md` section 8 that change the format, so they need a new
format version (`pack::VERSION`, the decoder refuses the old one, `tests/data/format-v<N>.*` regenerated with
the new build and the same bytes with one thread and two) and the whole-set size proof before any of it is
merged. Nothing has shipped, so there is no compatibility code; the version number changes whenever the
bytes change meaning.

**D7. Smaller tables for small inputs.** `table_bits` (`src/pack/mod.rs`) follows the file length, so a
10 MB file with 1 MB of modelled bytes still gets 160 MiB of tables, and a 30-byte damaged archive declaring
16 GiB costs 160 MiB of tables before it is refused (the security suite's notes). Size them by the modelled
byte count, written in the header as a new field (bounded like every other declared length): compression is
equal or better on files where the tables were oversized (fewer cold misses, same contexts), and the fixed
memory drops to what the data needs (a browser constraint: 160 MiB of tables plus histories is a quarter of
a 32-bit WebAssembly heap). Expected: time 10 to 20% on files whose tables shrink, memory from 160 MiB of
tables to what the modelled bytes need. Loss: none expected (the same contexts in smaller tables are a loss
only when they collide, which the sizing by modelled bytes avoids); allowed only with equal or better
compression on the set. Proof: the space benchmark's whole-set size and peak RSS against the stored result,
and the format fixture.

**D8. Group the predictable bits.** The long-repeat path codes a byte in one decision when the match is at
least 128 bytes (`LONG_MIN`); the threshold was chosen by size on a sample. Lowering it, or adding a
one-decision path for "the byte equals the prediction" in images, trades bits for time: up to 8x on the
bytes that take the one-decision path; on the set the images are 46 of geotopo's 77 s encode, so tens of
percent on image-heavy archives. Loss: bits where the grouped decision mispredicts. Proof: the whole-set
size proof of the space benchmark before any version bump; the format fixture.

Do D7 first (a single header field and the table sizing), measure, and only then D8; both are decided by
the owner on the numbers. The decoder's bounds (`MAX_PART`, `MAX_FILE`, the decode budget, the fax work
budget) must hold for the new field exactly as for the declared length: a damaged header must not size
anything beyond what the file's length allows.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- no output larger than the baseline on any file of the sample (`compare` fails on one that is), the changed files listed with their sizes;
- peak memory not up (`compare` fails over 10%);
- on the whole set (`--set all`, archive mode at effort 100, several hours; run it with `nohup` and let `time` resume), the total archive size not larger than the baseline's and no file larger except where the owner has accepted the trade in writing, peak memory down on the sample, and the format tests (`tests/pack_format.rs`, `tests/type1_fonts.rs`'s version test) updated to the new version;
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-format-tables` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
