# Parallel engine: each thread gets only what it needs, and sees the others' work (P1 to P5)

Branch: the session's branch (this brief is session 4's third). The rules and the procedure in full:
`docs/tasks/README.md`. The owner: "it would be better if the threads could get only the information they
need. All threads should be interconnected so that each can see each other's work; by that, repetitive tasks
can be avoided."

## What exists today

- One pool per file (`src/par.rs`, `Pool`), streams ordered biggest first and taken by whichever thread is
  free; results used in object order, so the output never depends on the thread count.
- Identical raw streams are found before any work starts (`identical_streams` in `src/pdf/rewrite.rs`) and
  encoded once.
- `par::Memo`: a single-flight store (a second thread asking for a key being computed waits for it instead of
  repeating it; a remembered result is checked against its input before it is used), but it lives for one
  stream only, so repeats across streams are not seen.
- A job closure can reach the whole object table (`objs`) through `resolve`, and gets more than it needs.

## The work

**P1. Plan first, then work, and never twice.** A cheap planning pass (one thread, hashes only) finds every
piece of work that repeats, before any thread starts: identical raw streams (as today), and also streams whose
decoded content is identical though their filters or compression differ (the same font program or image
stored twice in different ways), and identical image rows or segments where a job is cut into pieces. Each
unique piece of work becomes one job; every occurrence points to it. Jobs are ordered largest first. Because
this is decided before the threads run, it is deterministic.

**P2. Each job gets only what it needs.** A job receives an immutable view of its own input bytes (shared,
not copied: a borrowed slice under scoped threads or an `Arc<[u8]>`), the parameters already resolved during
planning (filter, decode parameters, colour space, effort settings) and, for a segment, its 32 KiB
dictionary; nothing else. No job can read or change another job's data or the object table. Results go back
through the ordered collector.

**P3. Threads see each other's work, safely.** One store per file (replacing the per-stream `Memo` life
span), keyed by a strong hash of the input bytes plus every parameter that changes the output, for work found
to repeat only while running (a decoded stream needed by both the encoder and the self-check, an image's
decoded rows needed by every predictor candidate, an encode another thread already finished). Single flight
as today, the fit check as today. Only exact repeats are shared, so the output is the same whichever thread
gets there first. Hints that are not exact repeats (one image's best predictor guiding a similar image) are
allowed only when the planning pass computes them in a fixed order, never at run time, since run-time hints
would make the output depend on timing.

**P4. Light and safe.** The store has a byte budget (an option, with a sensible default) and drops entries
in a fixed way when full; a miss only means recomputing, never a different output. The store lives for one
request. A store shared between requests (useful on a server that sees the same fonts in many files) is an
explicit option that is off by default and kept per tenant, because a cache shared between users is a timing
side channel: a fast answer would reveal that someone else processed the same content. SECURITY.md says so.

**P5. Measure it.**
- Repeated work avoided: jobs saved and CPU seconds saved on the sample.
- Wall time and parallel efficiency with 1, 2 and all cores: wall time times threads, over CPU time.
- Idle time per thread (under 10% on files with enough work).
- Peak memory and the store's peak.
- Output byte-identical for every thread count.
- Hostile cases: a file with 100,000 identical streams (the store stays within its budget) and one with
  100,000 distinct tiny streams (the planning and scheduling overhead stays bounded).
- The security suite at the default thread count.

## Where it lives

`src/par.rs` (the pool, the store), the planning and the job set-up in `src/pdf/rewrite.rs`. Keep the pool's
public API (`map`, `spawn`, `Memo::get` or its successor) compatible, because session 5 (fast tier) uses it for
its segments at the same time. The archive (`src/pack/`, session 3) can adopt the same planning in wave 2.

## Acceptance

- every output byte-identical to the baseline at efforts 0, 50 and 100 with 1, 2 and all cores;
- CPU time with all cores at most 1.15 times the one-thread CPU time, and lower than before where repeats
  were found;
- the P5 numbers in the pull request;
- both test suites, clippy on both feature sets, `cargo fmt --all --check`,
  `python3 -I bench/security.py run --fuzz-seconds 120` with 0 failures.
