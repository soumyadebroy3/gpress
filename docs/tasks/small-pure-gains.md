# The smaller pure gains (A7, A8, B2, D5)

Branch `opt-small-pure-gains`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

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
full sample. Work on a branch named `opt-small-pure-gains`.

## The change

Four small pure items from `docs/optimisation-plan.md` section 8, each its own commit on this branch, each
proven by `compare` (identical bytes). None changes a format.

**A7. Remember the parse when the prices did not change** (`src/flate/deflate.rs`, `parse.rs`). The
polish step re-runs `exact` on merged and split blocks whose symbol lists are unchanged when the merge is
rejected, and `refine` re-parses with `Costs::entropy` of statistics it has already parsed with when `blend`
returns to an earlier histogram. Keep the last few (costs fingerprint, parse) pairs per block; a repeated
fingerprint returns the stored parse. Expected: small on big streams (2 to 5%), up to 10 to 20% on small
blocks in polish. Memory: a few symbol lists per block. Proof: `compare`.

**A8. The remembered plain encode checked without a full inflate** (`src/flate/deflate.rs`, `deflate_zlib`
and `Memo`). A memo hit verifies the stored output by inflating it and comparing; a stored (hash, length,
SHA-256 of the input) checked against the input is enough to make a wrong reuse impossible and costs one hash
of the input instead of an inflate plus a compare. Use the engine's own SHA-256 (`src/pack/sha256.rs`).
Expected: 20 ms per big image; negligible overall, but it removes a decode from the hot path of every image.
Proof: `compare`; the memo's existing test that a wrong reuse is impossible, kept and made to plant a hash
collision with a different input.

**B2. Decode once for the self-check** (`src/pdf/rewrite.rs`, `self_check`). The self-check decodes every
stream of the output; the encoder has just produced those bytes from known content. Keep the content hash
(SHA-256 of the decoded data and of the dictionary without the encoding keys) for every re-encoded stream
and compare hashes, decoding only the streams that were kept as they were. Expected: the self-check from 1.1
to 1.5 s to about 0.3 s on geotopo (0.5 to 2% of a run). Memory: 32 bytes per stream. Proof: `compare` plus
`bench/check_the_checkers.py` still catching a planted wrong stream (the hash of a wrong stream must differ).

**D5. Fail the zlib parameter search early** (`src/flate/zlibexact.rs`, `src/flate/inflate.rs`). zlib
flushes its first block after exactly 2^(mem_level + 6) - 1 symbols (our `Tally` limit). Count the symbols
of the stream's first deflate block while inflating (one counter in the inflater); unless the block is the
last, its count pins the memory level to one value (or proves the stream is not zlib at all when the count
+ 1 is not a power of two), so the search drops from up to 72 trials to at most 8. Every trial is still
verified bit for bit, so the result is identical. Expected: the 2.6 s of failed trials on multipage's 450 KB
stream down to under 0.1 s; a few percent of the archive's planning overall, seconds on the files with
non-zlib streams (which, with preflate-rs gone, are stored as they are after the search). Proof: `compare`
on the archives; a test that every stream of `tests/data` is still rebuilt with the same parameters.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- every output byte-identical to the baseline (the `compare` line must count all 120 files in every setting as byte-identical);
- peak memory not up (`compare` fails over 10%);
- nothing slower than the baseline by more than the 10% `compare` allows, and for D5 the archive of `ocrmypdf/tests__resources__multipage.pdf` at least 2 s of CPU faster at effort 0 (`files.tsv` of the two runs);
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run new-security --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-small-pure-gains` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
