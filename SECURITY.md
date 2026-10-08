# Security

gpress reads files that may be hostile. These are the rules the code is held to, and how they are checked.

## Threat model

- Input files (PDFs, archives) are untrusted. A file may be crafted to crash the library, exhaust memory
  or time, make the decoder return wrong bytes, or make the normal-PDF writer change a document's meaning.
- The library runs inside other programs (C hosts, browsers through WebAssembly, servers). A failure in
  gpress must stay inside gpress: an error value, never an abort, a hang or a wrong result.
- The dependency tree is part of the product: nothing under a copyleft licence, nothing with a known
  advisory.

## Guarantees

| Rule | How it is kept | Where |
| --- | --- | --- |
| No panic crosses the library boundary | Every public call runs inside `catch_unwind`; a panic becomes `Error::Internal` with a message that names no source path, and the command-line tool and the C harness exit with that code (4), so the security suite and the fuzz targets count a caught panic as the bug it is, never as a refusal. The C layer adds its own guard. WebAssembly is built with `panic = "abort"`, where nothing unwinds: a bug traps the module, and the TypeScript wrapper turns the trap into `GpressError` code 4 and marks the instance dead. | `src/api.rs`, `crates/gpress-c`, `crates/gpress-wasm` |
| Every error has a stable code | `Error::code()`: 1 too large, 2 damaged, 3 unsupported version, 4 internal, 5 invalid options or argument, 6 time budget, 7 declared length over the limit, 8 needs the existing library, 9 declared decoded total over the limit. The C header and the TypeScript wrapper carry the same numbers. | `src/api.rs`, `crates/gpress-c/include/gpress.h`, `crates/gpress-wasm/js/gpress.ts` |
| Bounded memory | `Options::max_input` is checked before anything is read; `max_stream` and `max_stream_archive` cap decoded streams; `max_decoded` caps what the parts of one archive may claim in all, on both sides: compress stores streams past it as they are and declares the total in the header, expand refuses an archive declaring more before anything is decoded and holds every part to what the header declared (2 GiB per part either way), with `max_len` (1 GiB by default, 16 GiB the format's limit) doing the same for the restored length. Memory while expanding is then about `max_len` + (`max_len` + `max_decoded`) for the model's history + twice the largest part + the model's tables (README, "Options"); at most 4 MiB is reserved up front, the rest grows with what is actually decoded. Nothing a PDF claims (object counts, /Size, image sizes, predictor parameters) sizes memory on its own. | `src/api.rs`, `src/pack/mod.rs`, `src/pdf/` |
| Bounded time | Decoding stops when the arithmetic decoder reads past its input or a part restores nothing; the fax work per byte follows the declared length and the model's work the declared length and decoded total; the quick settings work within a budget proportional to the file; `Options::time_budget` stops optimize, compress and expand with `Error::TimeBudget` and no output. | `src/pack/mod.rs`, `src/fax/part.rs`, `src/pdf/rewrite.rs`, `src/clock.rs` |
| A wrong file is never returned | Archive: length and a 128-bit SHA-256 prefix of the restored bytes are checked against the header, and every archive is expanded and compared before it is returned. Normal PDF: the output is re-read and every reachable object compared with the input before it is returned; when in doubt the input comes back untouched. | `src/pack/mod.rs` (`unpack_with`, `pack_pass`), `src/pdf/rewrite.rs` (`self_check`, `gate`) |
| Strict parsing | The Flate decoder accepts exactly what zlib accepts, nothing damaged is silently repaired; the PDF parser rejects ambiguous syntax; the JPEG, CCITT and JBIG2 codecs read back and compare before anything they wrote is used. | `src/flate/inflate.rs`, `src/pdf/parse.rs`, `src/jpeg/`, `src/fax/` |
| Deterministic | Integer arithmetic in the model; format constants fixed in code; nothing read from the environment but the `PDFE_DEBUG` print switch; the same input and options give the same bytes on every platform and thread count, with or without a time budget (a budget that runs out gives an error, not a different file). | `src/cm/`, `src/pack/mod.rs`, `src/par.rs`, `src/clock.rs` |
| Memory safety at the C boundary | Null and length checks on every pointer; buffers are only freed with the library's own functions, which tolerate null and repeated calls; every `unsafe` block carries its reason (`bench/security.py supply` counts them). | `crates/gpress-c/src/lib.rs` |
| 32-bit targets | Sizes are `u64` in the format and clamped, never wrapped, in the WebAssembly module; the TypeScript wrapper refuses inputs of 2 GiB and more before they reach it. | `crates/gpress-wasm` |
| Encrypted and signed files | Files that need a password, signed or certified files, PDF/A and PDF/X are left untouched by the normal-PDF writer (`respect_conformance` governs the conformance ones; files the empty password opens are rewritten with their encryption kept exactly). | `src/pdf/rewrite.rs` (`gate`), `src/pdf/crypt.rs` |
| Licences and advisories | Our own code plus libraries under MIT, Apache-2.0, BSD, ISC, Zlib, Unicode or MPL-2.0 only; `deny.toml` holds the rule and `cargo deny check licenses advisories` runs in CI, for every feature and target. | `deny.toml`, `.github/workflows/ci.yml` |

## How it is checked

- Unit and integration tests, including negative tests: damaged archives (bit flips, truncation, garbage,
  impossible sizes, declared lengths over the limit) must be refused; the normal-PDF self-check must catch
  one-character changes; every option is shown to reach the engine (`tests/api.rs`).
- The security suite (`bench/security.py`): twenty archives damaged every way, with crafted headers (declared
  length and decoded total over the limit) and a crafted archive whose one part claims the whole decoded
  budget; hostile PDFs (deep nesting, reference loops, Flate, LZW and RunLength bombs, a file of several
  Flate streams decoding to 480 MiB in all, a million objects), mutation fuzzing of the three operations
  under a hard 512 MiB address space, the licence and source of every dependency, every `unsafe` block, and
  the same inputs through the C library (`bench/gpress_harness.c`). Every pack and unpack runs with
  `max_decoded` at 64 MiB, the setting a service makes. Results: `bench/RESULTS.md`.
- Fuzzing (`fuzz/`): the archive decoder, the Flate decoder and the PDF path run under libFuzzer with a
  memory limit; CI runs a short pass on every push.
- Independent verification in `bench/`: every normal-PDF output compared with its input by qpdf, pikepdf,
  PDFium, Poppler and pdf.js; every archive restored by a separate process.

## Reporting a problem

Open a private security advisory on the repository, or write to the maintainer. Please include the file
that triggers the problem.
