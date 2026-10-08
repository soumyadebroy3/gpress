# Security: fuzzing at length, Miri, sanitizers and an unsafe gate (S1 to S5)

Branch `opt-security-hardening`. The rules and the procedure in full: `docs/tasks/README.md`. This brief is
not from the optimisation plan: it makes "best by security" provable, the third of the owner's goals (best by
compression, by time and by security).

## Setup (a fresh clone)

    git clone <the repository> gpress && cd gpress
    cargo build --release --features existing
    cargo test --release --workspace && cargo test --release --workspace --features existing
    bench/fetch_corpus.sh && export CORPUS=$PWD/bench/corpus      # 907 public PDFs, pinned in bench/sets/corpus.lock
    export GPRESS=$PWD/target/release/gpress
    # plus the checking tools (qpdf, xz, brotli, zstd, a C compiler, pikepdf, pypdfium2, Pillow, libjpeg-turbo's
    # jpegtran, pdf.js with PDFJS_MODULES): the install lines are in docs/tasks/README.md, step 2. benchmark.py
    # warns about each missing one; a baseline made without them does not run the "no data loss" checks.
    rustup toolchain install nightly --component miri rust-src     # for S2 to S4
    cargo install cargo-fuzz --locked

The corpus files are untrusted: never open or run them; the scripts run under `python3 -I`. If the session
cannot download the nightly toolchain, say so in the report and do S1 and S5 only.

## What this branch owns

`fuzz/`, `tests/`, `.github/workflows/`, `bench/gpress_harness.c`, the crate attributes in `src/lib.rs`, and
the smallest possible engine fixes for what the fuzzers find. Other sessions are changing `src/flate/`,
`src/cm/`, `src/pack/` and `src/pdf/` at the same time: keep each fix minimal, give it its own commit with a
regression test, and name it in the pull request so the merge is easy.

## The work

**S1. An unsafe gate.** `#![deny(unsafe_code)]` at the top of `src/lib.rs`, with `#[allow(unsafe_code)]`
only on the one place that needs it: the AVX2 dispatch in `src/flate/parse.rs` (a `#[target_feature]`
function called after the run-time check, the standard pattern), with its reason on the line above. Any new
`unsafe` anywhere else then fails the build. `bench/security.py supply` still counts 2 blocks, both with a
reason. The C and WebAssembly crates are exempt (a foreign interface needs `unsafe`), but every `unsafe`
block there gets its reason too.

**S2. Fuzzing at length.** Run every target in `fuzz/` (expand, inflate, pdf_optimize) for 30 minutes each
with `-rss_limit_mb=2048 -timeout=60`, and add three targets: `pack_roundtrip` (any input: pack, then unpack,
must give the input back exactly, or pack must refuse it cleanly), `jpeg_tables` (a JPEG through the
Huffman-table step: the output must decode to the same coefficients, or the step must leave it untouched) and
`fax` (CCITT and JBIG2 decoders on arbitrary bytes: an error or a bitmap, never a panic or a long loop). Run the
new ones 30 minutes each too. For every crash, hang or out-of-memory: the minimised input under
`tests/data/fuzz/<target>/`, a regression test that fails before the fix, then the fix. Keep a minimised
corpus per target under `fuzz/corpus/` only if it stays under 5 MB in all; otherwise leave it out.

**S3. Miri.** `cargo +nightly miri test` on the tests that can run under Miri in reasonable time: the Flate
decoder, the archive reader on the format fixture and on damaged copies of it, the PDF parser on tiny
documents. Mark the heavy tests `#[cfg_attr(miri, ignore)]`. Any undefined behaviour Miri reports is a
blocker: fix it.

**S4. Sanitizers.** Build the C library with AddressSanitizer (`RUSTFLAGS=-Zsanitizer=address cargo
+nightly build -p gpress-c --target x86_64-unknown-linux-gnu`), the C harness with
`clang -fsanitize=address,undefined`, and run `python3 -I bench/security.py charness` against it: 0 reports.
If `-Zsanitizer` is unavailable, use the plain build with the harness under ASan and say so.

**S5. CI.** Keep the main CI fast. Add a weekly scheduled workflow (and `workflow_dispatch`) on nightly that
runs each fuzz target for 10 minutes, Miri on the fast tests, and the sanitizer harness; it uploads crashing
inputs as artifacts.

No new dependency (the generators for any new test are a few lines of our own). Every choice stays an option
or a test setting; nothing reads the environment.

## Acceptance

- every fuzz target 30 minutes with 0 crashes, hangs or out-of-memory after the fixes (give the iteration
  counts);
- Miri clean on the tests it runs (give the count);
- the sanitizer harness: 0 reports on the 1,506 damaged archives and 28 hostile PDFs;
- `python3 -I bench/security.py run new-security --fuzz-seconds 120`: 0 failures; `supply`: 0 advisories, 0 disallowed
  licences;
- if any engine code changed: `benchmark.py compare base new` on the sample with every output
  byte-identical and no setting slower by more than 2%;
- both test suites, clippy on both feature sets, `cargo fmt --all --check`.

## Report

A pull request against `main` from `opt-security-hardening` with the iteration counts per target, every bug
found (input, cause, fix, regression test), the Miri and sanitizer lines, the security suite lines and, if
engine code changed, the whole `compare` output.
