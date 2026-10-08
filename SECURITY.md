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
| No panic crosses the library boundary | Every public call runs inside `catch_unwind`; a panic becomes `Error::Internal` with a message that names no source path. The C and WebAssembly layers add their own guards. | `src/api.rs`, `crates/gpress-c`, `crates/gpress-wasm` |
| Every error has a stable code | `Error::code()`: 1 too large, 2 damaged, 3 unsupported version, 4 internal, 5 invalid options or argument, 6 time budget, 7 declared length over the limit, 8 needs the existing library. The C header and the TypeScript wrapper carry the same numbers. | `src/api.rs`, `crates/gpress-c/include/gpress.h`, `crates/gpress-wasm/js/gpress.ts` |
| Bounded memory | `Options::max_input` is checked before anything is read; `max_stream` and `max_stream_archive` cap decoded streams; the archive decoder bounds every declared length (2 GiB per part, 1032 decoded bytes per byte of the archive in all, 16 GiB per file, `max_len` below that) and reserves at most 4 MiB up front, growing with what is actually decoded. Nothing a PDF claims (object counts, /Size, image sizes, predictor parameters) sizes memory on its own. | `src/api.rs`, `src/pack/mod.rs`, `src/pdf/` |
| Bounded time | Decoding stops when the arithmetic decoder reads past its input or a part restores nothing; the fax work per byte and the model's work follow the declared length; the quick settings work within a budget proportional to the file; `Options::time_budget` stops optimize and compress with `Error::TimeBudget` and no output. | `src/pack/mod.rs`, `src/fax/part.rs`, `src/pdf/rewrite.rs`, `src/clock.rs` |
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
- The security suite (`bench/security.py`): twenty archives damaged every way, hostile PDFs (deep nesting,
  reference loops, Flate, LZW and RunLength bombs, a million objects), mutation fuzzing of the three
  operations under a hard 512 MiB address space, the licence of every dependency, every `unsafe` block,
  and the same inputs through the C library (`bench/gpress_harness.c`). Results: `bench/RESULTS.md`.
- Fuzzing (`fuzz/`): the archive decoder, the Flate decoder and the PDF path run under libFuzzer with a
  memory limit; CI runs a short pass on every push.
- Independent verification in `bench/`: every normal-PDF output compared with its input by qpdf, pikepdf,
  PDFium, Poppler and pdf.js; every archive restored by a separate process.

## Reporting a problem

Open a private security advisory on the repository, or write to the maintainer. Please include the file
that triggers the problem.
