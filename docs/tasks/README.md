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
  AGPL or LGPL); `cargo deny check` and `bench/check_dep_sources.py` pass. The plan's items need no new dependency.
- Everything configurable goes through options; nothing is read from the environment except the
  `PDFE_DEBUG` print switch.
- Plain short English in code comments, documents and commit messages.

## Every session: fast, light, parallel, safe

The owner's standard for every branch, in his words: "it has to be not a resource heavy tool, yet the best
tool out there in terms of speed and compression level; security should not be compromised; it should use
the CPU's threads as parallel cores for all the work." In practice:

- **Parallel everywhere it can be.** Every stage whose pieces are independent runs on the thread pool
  (`src/par.rs`): decoding, encoding, the self-check, image candidates, the archive's independent parts. A
  large single stream is cut into segments at fixed input offsets (never at offsets that depend on the thread
  count), each segment primed with the previous 32 KiB as its dictionary (as pigz does), so splitting costs
  almost nothing in size. The default thread count becomes every core the machine offers
  (`available_parallelism`, no cap of 8); `Options::threads` still sets it, and 1 still runs without any
  thread machinery.
- **Deterministic.** Identical output bytes for any thread count, any machine, with or without a time budget.
- **Light.** Threads must not multiply the cost: total CPU time with N threads at most 1.15 times the one-thread
  CPU time; peak memory at most the one-thread peak plus a bounded working set per thread (state the bound);
  no thread holds a whole decoded file.
- **Measured both ways.** CPU time (the work) and wall time (what a person waits), one thread and all cores;
  report the speed-up and the parallel efficiency (wall time times threads, over CPU time).
- **Safe.** Every security rule holds with threads: a hostile file with thousands of streams cannot make the
  pool hold more than its bound, and the security suite runs with the default thread count too.

## The procedure every brief follows

1. Clone and build. The release profile in `Cargo.toml` (LTO, one codegen unit) is what the benchmark
   measures; the same build serves the baseline and the change.

       git clone <the repository> gpress && cd gpress
       cargo build --release --features existing
       cargo test --release --workspace && cargo test --release --workspace --features existing

2. Install the tools the checks need. Without them a baseline still measures time and size, but the
   "no data loss" checks behind `exact` silently don't run: without `jpegtran` every output with a changed
   JPEG is reported as not identical, without pikepdf every normal-PDF row says `opens-check died`, without
   a C compiler peak memory readings have a 20 MB floor, without xz there is no calibration. `benchmark.py
   time` prints a warning for each missing tool and records them in the run's `meta.json`
   (`missing_tools`), and `compare` repeats it; a baseline made without them is not an acceptance run.

       sudo apt-get install -y qpdf xz-utils brotli zstd build-essential libjpeg-turbo-progs poppler-utils   # jpegtran, pdfimages
       pip install pikepdf pypdfium2 Pillow
       npm install pdfjs-dist && export PDFJS_MODULES=$PWD/node_modules                                     # pdf.js, node 20 or newer

   (`jpegtran` must be libjpeg-turbo's; a copy elsewhere is named with `JPEGTRAN=path`.)

3. Get the corpus: 907 public PDFs (130 MB) from four public sources, pinned by commit in
   `bench/sets/corpus.lock` with the sha256 of every file. `bench/fetch_corpus.sh` fetches them with curl
   (about two minutes, resumable; `--check` proves a copy has exactly those bytes) into `bench/corpus/`
   (git-ignored, and where the scripts look unless `$CORPUS` names another directory):

       bench/fetch_corpus.sh && export CORPUS=$PWD/bench/corpus

   Nothing in the corpus is ever executed; the benchmark scripts run under `python3 -I` and only read it.

4. Measure the baseline before changing anything: the 120-file sample (`bench/sets/sample120.txt`: the 20
   slowest files and the 20 largest, 34 files with 6 in both lists, then 86 at random; 60% of the set's
   bytes) at efforts 0, 50 and 100 with one thread, normal PDF and archive. `time` calibrates the machine
   first (a fixed xz job, stored in `meta.json`) so a later `compare` can tell a slower build from a slower
   machine.

       export GPRESS=$PWD/target/release/gpress
       python3 -I bench/benchmark.py time base --set sample120 --who ours --efforts 0,50,100 --threads 1

   This takes about an hour on two CPUs (effort 100 on the slowest files is most of it). While working,
   `--limit 20` or `--efforts 0` give a quick look; the acceptance run is the full sample. The two parts of
   the sample are sections of the list (`## slowest-and-largest`, `## random`): `--set sample120:random`
   measures or compares the 86 random files alone, for the rules that name them.

5. Make the change on a branch named after the brief (`opt-<name>`), with the tests the brief asks for.

6. Build again, measure the change the same way (`... time new ...`), and compare:

       python3 -I bench/benchmark.py compare base new

   `compare` fails when a setting got more than 10% slower in total (CPU time), a file more than 2x slower,
   any output larger, or the peak memory more than 10% higher; it prints how many outputs are
   byte-identical. The acceptance rule of a brief says which of these must hold and what else (every
   output byte-identical for a pure change; a memory target; a time target).

7. Before reporting, all of these pass:

       cargo test --release --workspace
       cargo test --release --workspace --features existing
       cargo clippy --workspace --all-targets -- -D warnings
       cargo clippy --workspace --all-targets --features existing -- -D warnings
       cargo fmt --all --check
       cargo deny check
       python3 -I bench/security.py run new-security --fuzz-seconds 120

   The security suite writes into the run it is given (`bench/results/new-security/`, git-ignored apart
   from its small result files; it refuses to write into a stored run such as `bench/results/security/`
   unless told `--overwrite`); `python3 -I bench/security.py report new-security` writes the run's Security
   section into `bench/RESULTS.md`, which stays out of the pull request too (the stored section is the
   owner's; `git checkout bench/RESULTS.md` puts it back). The full fuzz run (600 s per entry point) is for
   the merge, not every iteration.

8. Report: a pull request against `main` with the branch, carrying in its description the whole output of
   `benchmark.py compare base new` (and of `--metric wall` when wall time is the point, as for threads),
   the `meta.json` calibration of both runs, the test, clippy and security lines, and for a trade the
   per-file size changes. The commit messages say what changed and why, in plain English. The run
   directories (`bench/results/base`, `bench/results/new`, `bench/results/new-security`) stay out of the pull
   request; the stored runs under `bench/results/` are the owner's baselines.

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

Three briefs are not from the plan: `fast-tier.md` (F1 to F5: a real fast path for effort 0, the owner's
first priority), `lightweight.md` (L1 to L5: features, size, memory, start-up) and `security-hardening.md`
(S1 to S5: an unsafe gate, fuzzing at length, Miri, sanitizers, a weekly CI job).

## Cloud sessions, wave 1: six at once, time first

The owner's priority: "we need to seriously work on time first. No users will wait this much time for a PDF
compression. Also it must be lightweight." Wave 1 is speed and weight only; the security brief moves to wave 2.

Each session is a Claude Code cloud session on this repository, started with the prompt in the table. The
six own different parts of the code so their pull requests merge cleanly. A session that has two briefs does
them in order on one branch, named after its first brief, each brief's work in its own commits, and opens one
pull request.

| Session | Prompt | Plan items | Owns |
| --- | --- | --- | --- |
| 1 | `ultracode: do the task in docs/tasks/flate-fixed-costs.md` | A1, A3, A4 | `src/flate/block.rs`, `huffman.rs`, the table set-up in `matcher.rs` |
| 2 | `ultracode: do the task in docs/tasks/flate-inner-loops.md, then docs/tasks/pdf-memory.md on the same branch` | A6, A5, B3 | `src/flate/parse.rs`, the search loop and the lists in `matcher.rs` |
| 3 | `ultracode: do the task in docs/tasks/model-loops.md, then docs/tasks/archive-memory.md on the same branch` | D1, D2, D4 | `src/cm/`, `src/pack/` |
| 4 | `ultracode: do the task in docs/tasks/small-pure-gains.md, then docs/tasks/image-candidates-parallel.md on the same branch` | A7, A8, B2, D5, B1 | `src/flate/deflate.rs`, `zlibexact.rs`, `inflate.rs`, `src/pdf/`, `src/par.rs` |
| 5 | `ultracode: do the task in docs/tasks/fast-tier.md` | F1 to F5 | `src/flate/fast.rs` (new), the effort plan in `src/pdf/rewrite.rs`, the predictor choice in `src/pdf/images.rs` |
| 6 | `ultracode: do the task in docs/tasks/lightweight.md` | L1 to L5 | `Cargo.toml` features and profiles, `crates/`, CI size jobs, memory fixes in their own commits |

Session 5 matters most: today's effort 0 is the slowest setting of a strong encoder, not a fast encoder (a
1 MB paper takes 12 s), and the fast tier brings it to qpdf's time with output still smaller than qpdf's.

Rules while the six run at once:

- Change only what your briefs own. When a change elsewhere can't be avoided, keep it minimal, give it its
  own commit and name it in the pull request. Sessions 1 and 2 both touch `matcher.rs`: session 1 only the
  table set-up (A4), session 2 only the search loop and the lists (A5, B3). Sessions 4 and 5 both touch
  `src/pdf/rewrite.rs`: session 4 the self-check and the image candidates (B2, B1), session 5 the effort plan.
- No new `unsafe` code anywhere.
- Measure the baseline on the same machine and build settings as the change; give `compare` the calibration
  of both runs.
- A pure brief that turns out to change any output byte stops and reports what changed instead of opening a
  pull request.

## Wave 2, after wave 1 is merged

`security-hardening.md` (S1 to S5), `predictor-sample.md` (C1: pure at a 4 MiB budget; a lower budget is the
owner's decision), `flate-trades.md` (C3 to C5), `format-tables.md` (D7, D8, a format version), and the briefs written from
`docs/best-of-best.md` once the comparison with pdfsizeopt, mutool, cpdf, precomp and paq8px is done: a fast
setting that beats each competitor on size and time at once, and the files where a competitor is still
smaller. The last session of the wave merges everything and makes the full 907-file run with every check.
