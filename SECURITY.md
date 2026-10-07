# Security

gpress reads files that may be hostile. These are the rules the code is held to, and how they are checked.

## Threat model

- Input files (PDFs, archives) are untrusted. A file may be crafted to crash the library, exhaust memory
  or time, make the decoder return wrong bytes, or make the normal-PDF writer change a document's meaning.
- The library runs inside other programs (C hosts, browsers through WebAssembly, servers). A failure in
  gpress must stay inside gpress: an error value, never an abort, a hang or a wrong result.

## Guarantees

| Rule | How it is kept | Where |
| --- | --- | --- |
| No panic crosses the library boundary | Every public call runs inside `catch_unwind`; a panic becomes `Error::Internal`. The C and WebAssembly layers add their own guards. | `src/api.rs`, `crates/gpress-c`, `crates/gpress-wasm` |
| Bounded memory | `Options::max_input` is checked before anything is read; `max_stream` caps decoded streams; the archive decoder bounds every declared length (`MAX_PART`, `MAX_FILE`) and reserves at most 4 MiB up front, growing with what is actually decoded. | `src/api.rs`, `src/pack/mod.rs` |
| Bounded time on damaged archives | Decoding stops when the arithmetic decoder reads past its input; a hostile archive is refused in seconds, not hours. | `src/pack/mod.rs` (`Io::bytes`, `damaged`) |
| A wrong file is never returned | Archive: length and a 128-bit SHA-256 prefix of the restored bytes are checked against the header. Normal PDF: the output is re-read and every reachable object compared with the input before it is returned; when in doubt the input comes back untouched. | `src/pack/mod.rs` (`unpack`), `src/pdf/rewrite.rs` (`self_check`, `gate`) |
| Strict parsing | The Flate decoder accepts exactly what zlib accepts, nothing damaged is silently repaired; the PDF parser rejects ambiguous syntax (duplicate keys, bad escapes, lengths that don't land on `endstream`). | `src/flate/inflate.rs`, `src/pdf/parse.rs` |
| Deterministic | Integer arithmetic only in the model; format constants are fixed in code, never read from the environment; the same input and options give the same bytes on every platform and thread count. | `src/cm/`, `src/pack/mod.rs` |
| Memory safety at the C boundary | Null and length checks on every pointer; buffers are only freed with the library's own functions, which tolerate null and double calls. | `crates/gpress-c/src/lib.rs` |
| Encrypted and signed files | Files that need a password, signed or certified files, PDF/A and PDF/X are left untouched by the normal-PDF writer (configurable only for the conformance ones). | `src/pdf/rewrite.rs` (`gate`) |

## How it is checked

- Unit and integration tests, including negative tests: damaged archives (bit flips, truncation, garbage,
  impossible sizes) must be refused; the normal-PDF self-check must catch one-character changes.
- Fuzzing (`fuzz/`): the archive decoder, the Flate decoder and the PDF path run under libFuzzer with a
  memory limit; CI runs a short pass on every push, longer runs locally.
- Independent verification in `bench/`: every normal-PDF output compared with its input by qpdf, pikepdf,
  PDFium, Poppler and pdf.js; every archive restored by a separate process.

## Reporting a problem

Open a private security advisory on the repository, or write to the maintainer. Please include the file
that triggers the problem.
