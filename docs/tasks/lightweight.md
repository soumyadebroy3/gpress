# Lightweight: a small module, little memory, a fast start (L1 to L5)

Branch `opt-lightweight`. The rules and the procedure in full: `docs/tasks/README.md`. The owner: "it must be
lightweight". The compressor will run in a browser tab as WebAssembly and on a server for many people at once:
the download, the memory and the start-up cost are part of the time a person waits.

## Setup (a fresh clone)

    git clone <the repository> gpress && cd gpress
    rustup target add wasm32-unknown-unknown
    cargo build --release --features existing
    cargo test --release --workspace && cargo test --release --workspace --features existing
    bench/fetch_corpus.sh && export CORPUS=$PWD/bench/corpus      # 907 public PDFs, pinned in bench/sets/corpus.lock
    export GPRESS=$PWD/target/release/gpress

If the session cannot download the wasm32 target, say so and do L1, L3 and L5 natively; CI builds the module.

## Where things stand

The native `gpress` tool is 2.8 MB and the C library 1.8 MB (release, LTO). The WebAssembly module's size is
not measured anywhere yet. Normal-PDF peak memory on the 120-file sample: 95 MB at effort 0 and 174 MB at
effort 100 (one thread); the archive's peak is 950 MB on one file (`ocrmypdf/tests__resources__enormous.pdf`).
Every part is always built in: the archive and its model, the JPEG table step, the fax and JBIG2 codecs, the
encryption code and its RustCrypto dependencies.

## The work

**L1. Build only what a product needs.** Cargo features, all on by default so nothing changes for today's
users: `archive` (pack, unpack and the model), `jpeg` (the Huffman-table step), `fax` (CCITT and JBIG2),
`encryption` (decrypt and re-encrypt, and its dependencies), with `existing` (Lepton) needing `archive`. With a
feature off, the code is not compiled and the API says so with a typed error (`Error::NeedsFeature` or the
like), never a panic. The web compressor's build is the normal-PDF essentials. CI builds and tests every
combination that matters (all on, all off, the web set).

**L2. A size budget.** A release profile for the module (`opt-level = "z"` or `"s"`, whichever is smaller and
not slower than 10% on the sample, LTO, one codegen unit, `panic = "abort"`, symbols stripped), `wasm-opt -Oz`
when it is available. Record the sizes (raw and gzip) of the native tool, the C library and the module for each
feature set in `bench/RESULTS.md`, and add a CI step that fails when the web module grows more than 5% over the
recorded size. Target: the web module at most 400 KB gzip; report what it is.

**L3. Memory that follows the input.** Measure the normal-PDF peak against input size on the sample at efforts
0 and 100 (`benchmark.py space`) and fit it as fixed cost plus bytes per input byte. Find the big transient
copies (whole decoded streams held twice, output buffers grown by doubling, match-finder tables sized for the
largest window instead of the stream, per-stream allocations not reused) and remove them without changing any
output byte. Target at effort 0: at most 32 MB plus twice the input. (The archive's memory belongs to the
archive briefs.)

**L4. A fast start.** Instantiating the module and optimising a 1-page PDF of a few KB in Node: at most 50 ms in
all, measured in CI's Node test. No large table built at start-up; tables built on first use, sized to the
input.

**L5. Report it.** A "Lightweight" section in `bench/RESULTS.md`: sizes per build, the memory fit, the start-up
time, and what each feature costs in bytes.

## What this branch owns

`Cargo.toml` features and profiles, `#[cfg(feature = ...)]` attributes, `crates/gpress-wasm`, `crates/gpress-c`
feature plumbing, `.github/workflows/ci.yml` (size and feature jobs), the memory fixes (small, their own commits,
named in the pull request: other sessions change `src/flate/` and `src/pdf/` at the same time).

## Acceptance

- every output byte-identical to the baseline at efforts 0, 50 and 100 with all features on
  (`benchmark.py compare base new`), and no setting slower by more than 2%;
- with a feature off, its API calls return the typed error, tests prove it, and the rest works;
- the sizes, memory fit and start-up time recorded, with the targets met or the distance stated;
- both test suites, clippy on every feature set CI builds, `cargo fmt --all --check`,
  `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

## Report

A pull request against `main` from `opt-lightweight` with the size table, the memory fit before and after,
the start-up time, the `compare` output and the test, clippy and security lines.
