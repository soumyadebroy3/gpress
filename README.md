# gpress

Lossless PDF compression as a library. Two outputs, both verified before they are returned:

- **Normal PDF** (`optimize`): a smaller PDF that any PDF reader opens, with every object's value preserved.
  On the public test set: 16.0% smaller on the files it can safely touch, against 9.3% for qpdf.
- **gpress archive** (`compress` / `expand`): any file, a PDF above all, restored byte for byte.
  On the public test set: 56.5% of the original size, against 76.3% for the best of xz, Brotli and
  Zstandard.

The engine is our own: Flate encoder and decoder (smaller output than Zopfli, in less time), a bit-exact
re-creation of zlib's output, a strict PDF reader and writer with a safety gate, and a context-mixing
model with PDF-aware contexts. The numbers, the test set and how every output was checked by tools we
didn't write (qpdf, pikepdf, PDFium, Poppler, pdf.js) are in [bench/RESULTS.md](bench/RESULTS.md).

Private while in development. The repository name, the crate, the C library and the npm package are all
`gpress`.

## Using it

### Rust

```toml
[dependencies]
gpress = { git = "https://github.com/soumyadebroy3/gpress" }
```

```rust
use gpress::{Options, Outcome};

let opt = Options::new().effort(80).threads(4);

// A smaller PDF that any reader opens.
let r = gpress::optimize(&pdf_bytes, &opt)?;
match r.outcome {
    Outcome::Smaller(report) => std::fs::write("out.pdf", &r.data)?,
    Outcome::Untouched(why) => println!("left as it was: {why}"),
}

// The gpress archive, and the original back from it.
let archive = gpress::compress(&pdf_bytes, &opt)?;
let original = gpress::expand(&archive.data, &opt)?;
assert_eq!(original, pdf_bytes);
```

`Options` holds everything a caller can tune: `effort` (0 quick to 100 maximum), `max_input`,
`max_stream` (the largest decoded stream held in memory), `threads` (output never depends on it),
`time_budget`, `respect_conformance` (leave PDF/A, PDF/X, signed and certified files untouched; the safe
default), `flate_over_jpeg`, `use_existing_libraries`. Errors are a plain `Error` enum with a stable
numeric code; the library never panics across its boundary (every call runs inside a panic guard and a
bug comes back as `Error::Internal`).

### C (and everything that binds to C)

`crates/gpress-c` builds `libgpress_c` (shared and static) with the header
[`include/gpress.h`](crates/gpress-c/include/gpress.h):

```c
gpress_options opt; gpress_options_default(&opt); opt.effort = 70;
gpress_result r = gpress_optimize(pdf, pdf_len, &opt);
if (r.code == GPRESS_OK) fwrite(r.output.data, 1, r.output.len, out);
else fprintf(stderr, "%s\n", r.message);
gpress_result_free(&r);
```

Every function is thread-safe, never aborts the process, never reads outside the buffers it is given, and
every buffer it hands out is freed with `gpress_result_free` (or `gpress_free` / `gpress_free_message`).
[`examples/roundtrip.c`](crates/gpress-c/examples/roundtrip.c) is a complete program.

### JavaScript and TypeScript (browser and Node)

`crates/gpress-wasm` is a plain WebAssembly module (no bindgen layer) with a typed wrapper in
[`js/gpress.ts`](crates/gpress-wasm/js/gpress.ts):

```ts
import { Gpress } from "gpress";
const g = await Gpress.load(fetch("/gpress.wasm"));
const smaller = g.optimize(bytes, { effort: 70 });   // { data, outcome, reason? }
const archive = g.compress(bytes);
const original = g.expand(archive.data);             // throws GpressError on a damaged archive
```

The module is deterministic (the same bytes as the native library), single-threaded, and never traps. Run
it in a Web Worker to keep a page responsive. Pages that load it need `'wasm-unsafe-eval'` in their
Content Security Policy's `script-src`.

### Command line

```
cargo build --release
target/release/gpress optimize in.pdf out.pdf [--effort N | --quick]
target/release/gpress pack in.pdf out.gpz
target/release/gpress unpack out.gpz back.pdf
```

## Building

```
cargo build --release --workspace                 # core, C library, CLI (and the wasm crate as a native rlib)
cargo build --release --features existing         # also preflate-rs and lepton_jpeg (Apache-2.0)
cargo test --release --workspace
cargo build --release -p gpress-wasm --target wasm32-unknown-unknown   # needs: rustup target add wasm32-unknown-unknown
cd crates/gpress-wasm/js && npm ci && npm run build && npm test        # the npm package into js/dist/
```

CI (`.github/workflows/ci.yml`) runs formatting, clippy with warnings as errors, both test suites, the C
example against the shared library, the WebAssembly package and its tests, and a short fuzzing run of the
decoders (`fuzz/`, cargo-fuzz).

## Layout

| Path | What |
| --- | --- |
| `src/api.rs` | The public API: `Options`, `optimize`, `compress`, `expand`, `Error`. |
| `src/flate/` | Our Flate decoder and encoder, and the bit-exact zlib re-creation. |
| `src/pdf/` | PDF objects, strict parser, filters, images, the normal-PDF writer and its safety gate. |
| `src/cm/` | The context-mixing model and arithmetic coder. |
| `src/pack/` | The gpress archive format (version 4) and SHA-256. |
| `src/bin/gpress.rs` | The command-line tool. |
| `crates/gpress-c/` | C API, header, example. |
| `crates/gpress-wasm/` | WebAssembly module and the TypeScript package. |
| `fuzz/` | Fuzz targets for the archive decoder, the Flate decoder and the PDF path. |
| `bench/` | Benchmarks against existing tools and the independent checks. |
| `tests/` | Integration tests (zlib vectors, the archive format). |

## Safety rules the code follows

- **No data loss.** Every transformation is verified exactly before it is used; anything uncertain leaves
  that stream or file untouched. A damaged archive is refused with an error, never restored wrongly.
- **Untrusted input.** Lengths are bounded by `Options`, decoding stops when data runs out, allocations
  are capped, and no code path panics across the library boundary.
- **Deterministic.** Integer arithmetic only in the model; the format's constants are fixed in code and
  never read from the environment; output is identical whatever the thread count.
- **Licences.** Our code, plus optional Apache-2.0 libraries. Nothing GPL, AGPL or LGPL.

## Existing libraries (optional, `--features existing`)

- [preflate-rs](https://crates.io/crates/preflate-rs) (Apache-2.0, Microsoft): rebuilds Flate streams made by
  encoders other than zlib, in the archive.
- [lepton_jpeg](https://crates.io/crates/lepton_jpeg) (Apache-2.0, Microsoft): recompresses JPEG streams in the
  archive and restores them bit for bit.

The engine keeps whichever verified result is smaller, so these can only help. Apache-2.0 requires
shipping its licence and notices with a product.
