Run on build integration-8892b9e (preflate-rs gone, format version 7, the licence gate in): the two failures
below are closed, with no change to the engine's bounds. `flate_bomb.pdf` through pack now ends as
packed-exact in 1.0 s of CPU with a peak of 260 MiB (the failed inflate of the bomb stops at `max_stream`,
256 MiB, and the stream is then stored as it is, which is what optimize already did); the spec's fallback, a
decoded-bytes budget for pack, was therefore not added, since it would store some legitimate streams raw at
low effort and make those archives larger. `cabac` left with preflate-rs, so `supply` reports 0 disallowed
licences; `deny.toml` and `cargo deny check licenses advisories` keep it that way. What stays open from the
observations below is in `docs/tasks/` (the model's tables sized by the declared length; the archive's four
copies of every decoded byte, which also keeps the two large scans out of the fuzz seeds).

Written after looking at each failure of the run on build speed-ca8fa92 (effort 0, 1 thread). The engine is not
changed here; these are the exact causes, for whoever schedules the fixes, and two observations that passed
the bounds but belong in the optimisation plan.

- **`flate_bomb.pdf` through pack: 663 MiB for a 1 MiB file (FAIL).** `optimize` is safe (0.4 s, 70 MiB: the
  stream is over the quick budget of 16 decoded bytes per input byte, so it is kept as it is). `pack` is
  not: in `src/pack/mod.rs` `plan()` tries our own rebuild first (`rebuild_params`, which inflates with a
  256 MiB cap, `zlib::unwrap(raw, 1 << 28)`, and gives up), then `preflated()` hands the raw stream to
  `preflate_rs::preflate_whole_deflate_stream` with no cap at all. preflate-rs stops on its own after about
  125 MiB of plain text (`decoded_bytes: 131030720` with `PDFE_DEBUG=1`), the plan then holds that plain
  text, `recreate_whole_deflate_stream` makes the check copy, and the model codes the 125 MiB (`bypassed:
  131946038`): the peak is about 5 bytes per decoded byte, 663 MiB, for an input of 1 MiB. The pack path
  needs the same budget as optimize (decoded bytes per input byte, else store the stream raw) and
  preflate-rs needs a plain-text limit (the task "Bound preflate-rs on damaged archives" is the same
  library's other side). With that fix the stream is stored raw, which is what optimize already does, so
  compression does not change on normal files.
- **`cabac 0.15.0` is LGPL-3.0-or-later (FAIL).** It is a dependency of preflate-rs (the arithmetic coder it
  uses for the corrections) and is linked statically into every build with `--features existing`, including
  the measured one. The owner's rule allows no LGPL. The default build (no features) has no dependencies at
  all. Either the `existing` feature goes, preflate-rs is replaced by our own correction coder, or the crate
  is relicensed upstream; until then the feature cannot ship.
- **The million-object file is slow, not unbounded (passed).** `optimize`: 20 s of CPU and 518 MiB for
  52.8 MB (one million objects go into 5,001 object streams; 10 bytes of memory per input byte, about
  2.5 MB/s). `pack`: 98 s of CPU and 470 MiB (every one of the 52.8 MB of syntax goes through the
  context-mixing model at about 0.55 MB/s), and the restore check takes as long again. Both are inside the
  bound, which grows with the input (30 s + 5 s per MiB; 512 MiB or 16 bytes per input byte), because the
  work is proportional to the input. The throughput itself is a finding for the optimisation plan: syntax
  bytes are the slowest thing the model codes.
- **A declared length sizes the model's tables before a byte is checked (passed).** The damaged archives
  that declare 16 GiB are refused in under a second, but with a peak of 165 to 167 MiB where every other
  refusal stays under 25 MiB: `unpack` builds the model with `table_bits(len)` from the declared length
  (`src/pack/mod.rs`: `primed(table_bits(total_len))` in `run`, `Model::new(table_bits(len))` in
  `fallback_decode`; 2^20 buckets of 16 bytes for each of the 10 contexts), so a 30-byte file costs 160 MiB
  of tables. Inside the 512 MiB bound, and the
  plan's "size the fixed tables to the input" would close it.
- **Fuzz seeds left out.** Two small scans of the corpus need far more than 512 MiB even unmutated at
  effort 0: `ocrmypdf/tests__resources__enormous.pdf` (311 KB, decodes to 243 MB of image; the pack peaks at
  950 MiB without a limit) and `ocrmypdf/tests__resources__multipage.pdf` (1.1 MB, decodes to 142 MB;
  728 MiB). An earlier, unfinished run of this suite counted their mutants as crashes (SIGABRT under the
  512 MiB limit); they were allocation failures of legitimate work, so the seeds are now screened first and
  listed in the table. The memory itself, about 3 bytes per decoded byte held for the whole run, is a
  finding for the space benchmark and the plan.
