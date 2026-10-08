# Flate: the fixed costs per block (A1, A3, A4)

Branch `opt-flate-fixed-costs`. Plan: `docs/optimisation-plan.md`; the rules and the procedure in full: `docs/tasks/README.md`.

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
full sample. Work on a branch named `opt-flate-fixed-costs`.

## The change

Three pure speed-ups of the Flate encoder (`src/flate/`), from `docs/optimisation-plan.md` section 8, group A.
The output bytes must not change: the same algorithms and the same tie rules, done with less work.

**A1. Block-split estimates without the sort and without allocation.** `estimate_dynamic_bits` is called up
to 10,900 times per segment by the split DP (`split()` in `deflate.rs`) and is 42 to 84% of the instructions
of streams between 18 KB and 216 KB at effort 0, 19 to 34% of a 65 KB content stream and a font at effort 100.
Keep the same algorithm and tie rules (so the lengths, and therefore the estimates, are identical) but: sort
the (count, symbol) keys with a counting or radix sort instead of quicksort (half of `fast_lengths`), keep the
previous sorted order between neighbouring DP pairs and fix it with an insertion pass (the DP walks i for a
fixed j, so the histogram changes by one candidate block at a time), zero only the `n` used entries instead
of the 320-entry arrays (`memset` is 8 to 10% of these streams), and replace the package-merge's 15 `Vec`s by
fixed arrays. Expected: 2 to 3x on this function, 25 to 45% off every stream under 200 KB at effort 0 and 10
to 20% at effort 100. Memory: none. Format: none. Proof: `compare` (identical outputs) plus a unit test that
the new `fast_lengths` and package-merge give the same lengths as the old on 100,000 random histograms (the
ties are the risk; the test pins them).

**A3. Header search without allocation and without repeated work.** `header_search` (`block.rs`) runs
`dp_tokens` (4 `Vec`s, a `VecDeque`) up to 32 times per `best_dynamic`; the search from different starting
strategies usually meets after one round (the `seen` list already exploits that). Rewrite `dp_tokens` on fixed
stack arrays of 316 entries with u32 costs, compute `cl_lengths` once per distinct token list, and stop a
strategy early when its first round already equals a list seen. Expected: 3 to 5x on the header search, 60 to
70% off a tiny stream (76% of a 61-byte stream is this), 10 to 15% off a 65 KB content stream. Memory: none.
Format: none. Proof: `compare`; extend `header_search_is_unchanged_by_the_memory_of_tried_tokens` to compare the
old and new searches on random lengths.

**A4. Matcher tables reused and not zeroed.** `find_matches` (`matcher.rs`) allocates and zeroes `head`
(256 KiB) and `son` (512 KiB) per call; `son` never needs zeroing (every reached node has both children
written before they are read), `head` can be invalidated by an epoch counter (an entry from an earlier call is
ignored when its epoch differs; positions are already checked against the window). Keep one scratch per
thread. Expected: 1 to 1.5 M instructions per call: 10% of a 61-byte stream, five times per image for the
candidates; for tracemonkey's 377 streams plus 900 candidate encodes about 1.5 s of 10. Memory: the scratch
lives as long as the thread (768 KiB per thread) instead of being paged in and out. Format: none. Proof:
`compare`; the matcher's brute-force test run twice on the same scratch.

Together the plan predicts typical files 1.8 to 2.5x faster at effort 0 and 1.3x at effort 100 (the 86
random files of the sample: 10.2 s to about 5 s at effort 0). Do them in the order A1, A3, A4, each a commit
with its test; `tests/zlib_vectors.rs`, `tests/bounds.rs` and the Flate unit tests must stay green after each.

## Acceptance

    cargo build --release --features existing
    python3 -I bench/benchmark.py time new --set sample120 --who ours --efforts 0,50,100 --threads 1
    python3 -I bench/benchmark.py compare base new

All of these must hold:

- every output byte-identical to the baseline (the `compare` line must count all 120 files in every setting as byte-identical);
- peak memory not up (`compare` fails over 10%);
- the random files of the sample (the 80 drawn at random, listed in `bench/sets/sample120.txt`) at least 1.5x faster in CPU time at effort 0 in total, and nothing slower than the baseline by more than the 10% `compare` allows;
- `cargo test --release --workspace` with and without `--features existing`;
- `cargo clippy --workspace --all-targets -- -D warnings` on both feature sets, and `cargo fmt --all --check`;
- `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.

The owner's rules (`docs/tasks/README.md`) apply throughout: no data loss, no worse compression,
deterministic output for any thread count, no panics, bounded memory and time, no new dependency, every
choice through options, plain English.

## Report

A pull request against `main` from `opt-flate-fixed-costs` whose description carries the whole output of
`benchmark.py compare base new`, the calibration lines of both `meta.json` files, and the test, clippy and
security lines; commit messages in plain short English saying what changed and why. The run directories
(`bench/results/base`, `bench/results/new`) stay out of the pull request.
