# Optimisation tasks

One brief per branch of the optimisation work that `docs/optimisation-plan.md` recommends (its section 9
gives the order). Each brief is written for a fresh Claude Code cloud session on a clone of this
repository: it says how to get the test corpus, how to build, which baseline to measure first, what to
change, what must hold before the work is done, and how to report. Everything a brief needs is in the
repository; the raw profile data behind the plan is not (it stays on the machine it was made on) and no
brief needs it.

The rules every branch is held to, from the owner:

- Data loss is never allowed: every normal-PDF output still reads back with every object's value kept, and
  every archive still restores byte for byte. The engine's own self-checks stay on.
- Compression must not get worse: no output larger than before, on any file of the set. A "pure" change
  gives byte-identical outputs; a "trade" may change bytes but never upward.
- Deterministic output: identical bytes for any thread count and on any machine, with or without a time
  budget.
- No panics across the library boundary; bounded memory and time on hostile input.
- Only our own code plus libraries under MIT, Apache-2.0, BSD, ISC, Zlib, Unicode or MPL-2.0 (never GPL,
  AGPL or LGPL); `cargo deny check licenses advisories` passes. The plan's items need no new dependency.
- Everything configurable goes through options; nothing is read from the environment except the
  `PDFE_DEBUG` print switch.
- Plain short English in code comments, documents and commit messages.

## The procedure every brief follows

1. Clone and build. The release profile in `Cargo.toml` (LTO, one codegen unit) is what the benchmark
   measures; the same build serves the baseline and the change.

       git clone <the repository> gpress && cd gpress
       cargo build --release --features existing
       cargo test --release --workspace && cargo test --release --workspace --features existing

2. Get the corpus: 907 public PDFs (130 MB) from four public sources, pinned by commit in
   `bench/sets/corpus.lock` with the sha256 of every file. `bench/fetch_corpus.sh` fetches them with curl
   (about two minutes, resumable; `--check` proves a copy has exactly those bytes) into `bench/corpus/`
   (git-ignored), or the directory named by `$CORPUS`:

       bench/fetch_corpus.sh && export CORPUS=$PWD/bench/corpus

   Nothing in the corpus is ever executed; the benchmark scripts run under `python3 -I` and only read it.

3. Measure the baseline before changing anything: the 120-file sample (`bench/sets/sample120.txt`: the 20
   slowest files, the 20 largest and 80 at random, 60% of the set's bytes) at efforts 0, 50 and 100 with one
   thread, normal PDF and archive. `time` calibrates the machine first (a fixed xz job, stored in
   `meta.json`) so a later `compare` can tell a slower build from a slower machine.

       export GPRESS=$PWD/target/release/gpress
       python3 -I bench/benchmark.py time base --set sample120 --who ours --efforts 0,50,100 --threads 1

   This takes about an hour on two CPUs (effort 100 on the slowest files is most of it). While working,
   `--limit 20` or `--efforts 0` give a quick look; the acceptance run is the full sample.

4. Make the change on a branch named after the brief (`opt-<name>`), with the tests the brief asks for.

5. Build again, measure the change the same way (`... time new ...`), and compare:

       python3 -I bench/benchmark.py compare base new

   `compare` fails when a setting got more than 10% slower in total (CPU time), a file more than 2x slower,
   any output larger, or the peak memory more than 10% higher; it prints how many outputs are
   byte-identical. The acceptance rule of a brief says which of these must hold and what else (every
   output byte-identical for a pure change; a memory target; a time target).

6. Before reporting, all of these pass:

       cargo test --release --workspace
       cargo test --release --workspace --features existing
       cargo clippy --workspace --all-targets -- -D warnings
       cargo clippy --workspace --all-targets --features existing -- -D warnings
       cargo fmt --all --check
       python3 -I bench/security.py run --fuzz-seconds 120

   The security suite's full fuzz run (600 s per entry point) is for the merge, not every iteration.

7. Report: a pull request against `main` with the branch, carrying in its description the whole output of
   `benchmark.py compare base new` (and of `--metric wall` when wall time is the point, as for threads),
   the `meta.json` calibration of both runs, the test, clippy and security lines, and for a trade the
   per-file size changes. The commit messages say what changed and why, in plain English. The run
   directories (`bench/results/base`, `bench/results/new`) stay out of the pull request; the stored runs
   under `bench/results/` are the owner's baselines.

## The briefs

In the plan's order of gain per unit of risk, pure changes first:

| Brief | Plan items | Kind | What it buys |
| --- | --- | --- | --- |
| `flate-fixed-costs.md` | A1, A3, A4 | pure | typical PDFs 1.8 to 2.5x at effort 0, 1.3x at 100 |
| `flate-inner-loops.md` | A5, A6 | pure | streams above 100 KB 1.2 to 1.4x at effort 100 |
| `model-loops.md` | D1, D2 | pure | the archive 1.4 to 1.7x |
| `archive-memory.md` | D4 | pure | the archive's peak from 4 bytes per decoded byte to 1 |
| `pdf-memory.md` | B3 | pure | the normal-PDF peak by a quarter to a third at the top efforts |
| `small-pure-gains.md` | A7, A8, B2, D5 | pure | a few percent each; seconds on the files that hit them |
| `image-candidates-parallel.md` | B1 | pure | wall time of the predictor search on machines with spare threads |
| `predictor-sample.md` | C1 | pure at 4 MiB, a trade below | 7% of the set at effort 100 (more by the owner's decision) |
| `flate-trades.md` | C3, C4, C5 | trade | up to 2x at effort 0 and 2 to 3x on big images at 100, if the per-file rule passes |
| `format-tables.md` | D7, D8 | format change | smaller tables for small inputs; grouped decisions |

Items the plan tried and rejected (A2) or that no longer apply (D6, preflate-rs is gone) have no brief.

One more brief is not from the plan: `security-hardening.md` (S1 to S5: an unsafe gate, fuzzing at length,
Miri, sanitizers, a weekly CI job), for the owner's third goal, best by security.

## Cloud sessions, wave 1: five at once

Each session is a Claude Code cloud session on this repository, started with the prompt in the table. The
five own different parts of the code so their pull requests merge cleanly. A session that has two briefs does
them in order on one branch, named after its first brief, each brief's work in its own commits, and opens one
pull request.

| Session | Prompt | Plan items | Owns |
| --- | --- | --- | --- |
| 1 | `ultracode: do the task in docs/tasks/flate-fixed-costs.md` | A1, A3, A4 | `src/flate/block.rs`, `huffman.rs`, the table set-up in `matcher.rs` |
| 2 | `ultracode: do the task in docs/tasks/flate-inner-loops.md, then docs/tasks/pdf-memory.md on the same branch` | A6, A5, B3 | `src/flate/parse.rs`, the search loop and the lists in `matcher.rs` |
| 3 | `ultracode: do the task in docs/tasks/model-loops.md, then docs/tasks/archive-memory.md on the same branch` | D1, D2, D4 | `src/cm/`, `src/pack/` |
| 4 | `ultracode: do the task in docs/tasks/small-pure-gains.md, then docs/tasks/image-candidates-parallel.md on the same branch` | A7, A8, B2, D5, B1 | `src/flate/deflate.rs`, `zlibexact.rs`, `inflate.rs`, `src/pdf/`, `src/par.rs` |
| 5 | `ultracode: do the task in docs/tasks/security-hardening.md` | S1 to S5 | `fuzz/`, `tests/`, `.github/workflows/`, `bench/gpress_harness.c`, `src/lib.rs` attributes |

Rules while the five run at once:

- Change only what your briefs own. When a change elsewhere can't be avoided, keep it minimal, give it its
  own commit and name it in the pull request. Sessions 1 and 2 both touch `matcher.rs`: session 1 only the
  table set-up (A4), session 2 only the search loop and the lists (A5, B3).
- No new `unsafe` code anywhere (session 5 makes the build refuse it).
- Measure the baseline on the same machine and build settings as the change; give `compare` the calibration
  of both runs.
- A pure brief that turns out to change any output byte stops and reports what changed instead of opening a
  pull request.

## Wave 2, after wave 1 is merged

`predictor-sample.md` (C1: pure at a 4 MiB budget; a lower budget is the owner's decision),
`flate-trades.md` (C3 to C5), `format-tables.md` (D7, D8, a format version), and the briefs written from
`docs/best-of-best.md` once the comparison with pdfsizeopt, mutool, cpdf, precomp and paq8px is done: a fast
setting that beats each competitor on size and time at once, and the files where a competitor is still
smaller. The last session of the wave merges everything and makes the full 907-file run with every check.
