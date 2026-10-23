# gpress

[![CI](https://github.com/soumyadebroy3/gpress/actions/workflows/ci.yml/badge.svg)](https://github.com/soumyadebroy3/gpress/actions/workflows/ci.yml)
[![CodeQL](https://github.com/soumyadebroy3/gpress/actions/workflows/codeql.yml/badge.svg)](https://github.com/soumyadebroy3/gpress/actions/workflows/codeql.yml)
[![Licence: MIT OR Apache-2.0](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue.svg)](#licence)

Lossless PDF compression as a library, written in Rust, with C and WebAssembly bindings and a command-line
tool. It makes two kinds of output, and checks each one before returning it:

- **Normal PDF** (`optimize`): a smaller PDF that any PDF reader opens, with every object's value preserved.
  Streams are compressed again with our own Flate encoder, images get a better PNG predictor, JPEGs get
  optimal Huffman tables (the same coefficients), CCITT fax images become JBIG2 (the same pixels), and objects
  go into compressed object streams. The output is read back and compared with the input before it is
  returned; anything uncertain is left as it was.
- **gpress archive** (`compress` / `expand`, `.gpz`): any file, a PDF above all, packed with a PDF-aware
  context-mixing model and restored byte for byte. Every archive is restored and checked before it is
  returned, and a damaged archive is refused with an error, never restored wrongly.

Most of the engine is our own code; the parts that follow another project's code (zlib, libdeflate, pdf.js,
the LZMA SDK) say so and keep its licence notice, as listed in
[Code from other projects](docs/reference.md#code-from-other-projects). A default build's only outside
libraries are RustCrypto's AES, MD5 and SHA-2, for encrypted PDFs.

## Status

Version 0.1.0, in active development: the API may still change, and nothing is published to crates.io or npm
yet, so everything below builds from this repository. The archive format is version 8; archives from earlier
development versions are refused. CI builds and tests on Linux (x86-64).

## Install

You need a recent stable Rust ([rustup](https://rustup.rs)); CI uses the latest stable release.

### The command-line tool

```sh
cargo install --locked --git https://github.com/soumyadebroy3/gpress gpress
```

Or from a clone:

```sh
git clone https://github.com/soumyadebroy3/gpress
cd gpress
cargo build --release            # the tool is target/release/gpress
```

Add `--features existing` to either command to also use [Lepton](https://crates.io/crates/lepton_jpeg)
(Apache-2.0) for JPEG streams in the archive.

### As a Rust library

```toml
[dependencies]
gpress = { git = "https://github.com/soumyadebroy3/gpress" }
```

### As a C library

```sh
cargo build --release -p gpress-c
```

This builds `libgpress_c` as a shared and a static library in `target/release/` (`libgpress_c.so` and
`libgpress_c.a` on Linux), with the header [`crates/gpress-c/include/gpress.h`](crates/gpress-c/include/gpress.h).
On Linux:

```sh
cc -Icrates/gpress-c/include app.c target/release/libgpress_c.a -lpthread -ldl -lm -o app   # static
cc -Icrates/gpress-c/include app.c -Ltarget/release -lgpress_c -o app                      # shared: run it with
LD_LIBRARY_PATH=target/release ./app                                                         # the library on its path
```

### As a WebAssembly module (browser and Node)

```sh
rustup target add wasm32-unknown-unknown
cd crates/gpress-wasm/js
npm ci
npm run build                    # dist/: gpress.js, gpress.d.ts, gpress.wasm, gpress-web.wasm
```

The build runs binaryen's `wasm-opt` on the modules when it is on `PATH`; install binaryen for the smallest
modules.

## Quick start

```sh
gpress optimize in.pdf out.pdf            # a smaller PDF that any reader opens
gpress optimize in.pdf out.pdf --quick    # the same at effort 0, the fast path
gpress pack in.pdf in.gpz                 # the gpress archive (works on any file)
gpress unpack in.gpz back.pdf             # the original, byte for byte
```

`optimize` prints one tab-separated line: the input, `SMALLER` or `UNTOUCHED`, bytes in, bytes out, seconds,
and the details. `pack` prints the input, bytes in, bytes out, seconds, then `PACKED` or `STORED` with the
archive's statistics:

```
in.pdf	SMALLER	8791	4955	0.15	objects=6 streams=2 reencoded=2 kept=0 objstms=1 dropped=1 jpegs_retabled=0 fax_to_jbig2=0
in.pdf	8791	6975	0.10	PACKED streams=3 rebuilt=3 rebuilt_bytes=7909 lepton=0 type1=0 fax=0 decrypted=0 decoded_bytes=54664
```

A file `optimize` can't make smaller or can't safely touch (a signed file, PDF/A, a file with earlier
revisions, an encrypted file that needs a password, a damaged one) is written to `out.pdf` unchanged, with
`UNTOUCHED` and the reason. `gpress` without arguments lists every option, and the engine's measuring
commands (`deflate`, `inflate`, `jpeg`, `fax-decode`, `expand`, ...) that the benchmarks use; the comments at
the top of [`src/bin/gpress.rs`](src/bin/gpress.rs) describe them. The exit status is 0 when the call worked,
2 for a usage or file error, and otherwise the library's error code (for example 2 for a damaged archive,
6 when `--time-budget` ran out; the full list is in the same comments).

## Effort levels

`--effort N` (the `effort` option) runs from 0, the fastest, to 100, the smallest; the default is 100.
**Every level is lossless**: they differ only in how hard the engine searches, and every level's output is
checked the same way. The output is the same for any thread count.

| Effort | What runs (normal PDF) | CPU time, first measured | CPU time, current code | Output |
| --- | --- | ---: | ---: | ---: |
| 0 (`--quick`) | The fast path: our fast Flate encoder, image predictors chosen from an estimate, and the cheap steps (object streams, the cross-reference stream, JPEG tables, JBIG2) | 16.1 s | 16.8 s | 87.02% |
| 5 | The bridge: closer parses for images | 23.0 s | | 86.70% |
| 10 | The bridge, plus the strong encoder's quick settings for the streams that take the most room, within a budget that doubles every point | 40.7 s | | 86.11% |
| 15 | | 111 s | | 85.74% |
| 19 | | 183 s | | 85.59% |
| 20 | The strong encoder, its settings in steps of 20, the strongest that fits a work budget proportional to the file's size | 244 s | | 85.43% |
| 50 | | 334 s | 176 s | 85.17% |
| 100 (default) | The strong encoder at its strongest | 983 s | 512 s | 85.01% |
| qpdf, strongest lossless setting | For comparison | 25.3 s | | 87.23% |

CPU time is for the whole 120-file sample (`bench/sets/sample120.txt`, 78.3 MB, a deliberately hard mix of the
slowest, the largest and random files) on one thread; output is the total size as a share of the input on the
90 files the engine rewrites. For qpdf it counts the 85 outputs `bench/verify_pdf.py` finds the same as their
input apart from the trailer's /ID, and the other 5 files at their input size. The first measurement was made
in October 2026 for the change that added the fast path, on a 4-core 2.1 GHz Xeon virtual machine. Later
changes made efforts 20, 50 and 100 faster without changing a byte of their output, and the current code was
measured again at efforts 0, 50 and 100 on the same kind of machine (with another build timed beside it), its
outputs byte for byte the same; the other levels were not measured again. In the archive, data that already
looks compressed is coded plainly below effort 50. The [reference](docs/reference.md#options) has the details.

## Using it from code

### Rust

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

Errors are a plain `Error` enum with a stable numeric code (1 input too large, 2 damaged archive,
3 unsupported archive version, 4 internal, 5 invalid options, 6 time budget, 7 declared length over the
limit, 8 needs the existing library, 9 declared decoded total over the limit, 10 needs a part this build
leaves out; see [Building only what a product needs](docs/reference.md#building-only-what-a-product-needs))
and a message without internal paths. The library never panics across its boundary: every call
runs inside a panic guard, and a bug comes back as `Error::Internal`.
`Optimized.outcome` says what happened (`Smaller` with a `Report`: objects, streams re-encoded and kept,
object streams, JPEGs given new tables, fax images written as JBIG2, how the file is encrypted; or
`Untouched` with the reason), and `Compressed` carries the archive's statistics (streams rebuilt, through
Lepton, Type 1 fonts opened, fax images, streams stored decrypted, decoded bytes, or `stored` when nothing
could be made smaller).

### C (and everything that binds to C)

```c
gpress_options opt; gpress_options_default(&opt); opt.effort = 70;
gpress_result r = gpress_optimize(pdf, pdf_len, &opt);
if (r.code == GPRESS_OK) fwrite(r.output.data, 1, r.output.len, out);
else fprintf(stderr, "%s\n", r.message);
gpress_result_free(&r);
```

Every function is thread-safe, never aborts the process, never reads outside the buffers it is given, and
every buffer it hands out is freed with `gpress_result_free` (or `gpress_free` / `gpress_free_message`, each
given the field's address), which are safe on null and on a result already freed: a free clears what it
freed, so a double free or a part freed before the whole result is harmless by construction. An empty output
is a null pointer with length 0. The options struct has the same fields as the Rust `Options` but
`store_budget`, which stays at its default (fill it with `gpress_options_default` first; a size left at 0
means its default).
[`examples/roundtrip.c`](crates/gpress-c/examples/roundtrip.c) is a complete program, and
`bench/gpress_harness.c` sends the security suite's hostile inputs through this API.

### JavaScript and TypeScript (browser and Node)

`crates/gpress-wasm` is a plain WebAssembly module (no bindgen layer) with a typed wrapper in
[`js/gpress.ts`](crates/gpress-wasm/js/gpress.ts):

```ts
import { Gpress } from "./dist/gpress.js";                         // built in crates/gpress-wasm/js; not on npm yet
const g = await Gpress.load(fetch("/gpress.wasm"));                 // or gpress-web.wasm: no archive, smaller
const smaller = g.optimize(bytes, { effort: 70, jbig2: false });   // { data, outcome, reason? }
const archive = g.compress(bytes);
const original = g.expand(archive.data, { maxLenMiB: 512 });       // throws GpressError on a damaged archive
```

The module gives the same bytes as the native library (the web module too, for `optimize`; see
[Building only what a product needs](docs/reference.md#building-only-what-a-product-needs)), is
single-threaded and has no clock (so no time budget: run it in a Web Worker to keep a page responsive and to
end it when it has taken too long). It is built with `panic = "abort"` (the only choice on
`wasm32-unknown-unknown`), so the panic guard that turns a bug into `Error::Internal` natively catches nothing
there: a bug, like a failed memory growth, traps the module, and the wrapper turns the trap into a
`GpressError` with code 4 and marks the instance dead (load a fresh one). Sizes are 32-bit inside it: the
wrapper refuses inputs of 2 GiB and more. Pages that load it need `'wasm-unsafe-eval'` in their Content
Security Policy's `script-src`.

A first load compiles the module (4 to 9 ms in Node, `bench/RESULTS.md`, Start-up). `Gpress.compile(source)`
returns the compiled module, and `Gpress.load(module)` makes an instance from it without compiling again:
compile once, then make one instance per worker (a module can be posted to a Web Worker) or a fresh one
after a trap. Serve the file as `application/wasm`, with no parameters, so that it compiles while it downloads
(with any other type it is compiled once it has arrived).

## Options

Everything a caller can tune is a field of `Options`, and every field is a flag of the command-line tool. The
ones most callers need:

| Option | Flag | Default | What it does |
| --- | --- | --- | --- |
| `effort` | `--effort N`, `--quick` | 100 | 0 (fastest) to 100 (smallest), see above. |
| `threads` | `--threads N` | 0 | 0: every core the machine offers, at most 8; at most 64. The output is the same for any count. |
| `time_budget` | `--time-budget MS` | none | Give up after this much wall time: an error and no output, the input unchanged. |
| `max_input` | `--max-input BYTES` | 1 GiB | Larger inputs are refused at once. |
| `max_len` | `--max-len BYTES` | 1 GiB | Expand: an archive declaring a larger original is refused before anything is decoded. |
| `max_decoded` | `--max-decoded BYTES` | 256 MiB | Archive: the most decoded bytes the parts of one archive may claim, both ways. |
| `respect_conformance` | `--ignore-conformance` | on | Normal PDF: leave PDF/A, PDF/X, signed, certified and usage-rights files untouched. |
| `jpeg_tables`, `jbig2`, `flate_over_jpeg` | `--no-jpeg-tables`, `--no-jbig2`, `--no-flate-over-jpeg` | on | Normal PDF: turn single steps off. |

Every other option, the memory each limit bounds, the parts of the engine a build can leave out
(`--no-default-features --features ...`, for example the smaller `web` build without the archive), how
encrypted PDFs are handled and what each part of the engine does are in the
[reference](docs/reference.md).

## Benchmarks

Measured on 907 public test PDFs (130 MB: the pdf.js, pypdf, OCRmyPDF and pikepdf test suites, many of them
deliberately broken or unusual), fetched and pinned by `bench/fetch_corpus.sh`:

| What | gpress | Compared with |
| --- | --- | --- |
| Normal PDF, maximum effort | 16.0% smaller on the 697 files it rewrites | qpdf (object streams, Flate at level 9): 9.3% smaller on the same files |
| Every Flate stream on its own (7,280 streams) | 41.05 MB | Zopfli (15 iterations) 0.67% larger, libdeflate 12 1.40% larger, zlib 9 6.95% larger |
| Archive, with Lepton (`--features existing`) | 56.5% of the original size | the best of xz -9e, Brotli 11 and Zstandard 19, file by file: 76.3% |

Every normal-PDF output was compared with its original by tools we didn't write (qpdf, pikepdf, PDFium,
Poppler and pdf.js) and every archive was restored by a separate process and compared byte for byte.

These figures were measured on earlier builds, and [bench/RESULTS.md](bench/RESULTS.md) says which build and
machine each comes from. The archive figure in particular predates the removal of preflate-rs (its dependency
`cabac` is LGPL): this build stores Flate streams that zlib did not write as they are, so files with such
streams make larger archives (on ten files of the sample, 55.8% of the input became 75.1% at effort 0).
`bench/benchmark.py` measures any build.

## Safety

- **No data loss.** Every transformation is verified exactly before it is used; anything uncertain leaves
  that stream or file untouched. A damaged archive is refused with an error, never restored wrongly.
- **Untrusted input.** Lengths are bounded by `Options`, decoding stops when data runs out, allocations are
  capped and grow with what is actually decoded, nothing a file claims sizes memory on its own, and no code
  path panics across the library boundary.
- **Deterministic.** Integer arithmetic only in the model; the format's constants are fixed in code and never
  read from the environment; output is identical whatever the thread count, the machine, or the time budget.
- **Licences.** Permissive libraries only ([the licence rule](docs/reference.md#the-licence-rule)):
  `cargo deny` checks every dependency's licence and the RustSec advisories in CI.

[SECURITY.md](SECURITY.md) has the threat model, the guarantees with where each is kept, how they are
checked, and how to report a vulnerability privately.

## Build and test

```
cargo build --release --workspace                 # the library, the C library and the tool (and the wasm crate as a native rlib)
cargo build --release --features existing         # also lepton_jpeg (Apache-2.0)
cargo test --release --workspace
cargo test --release --workspace --features existing
cargo test --release --workspace --no-default-features --features web   # or any other set of features
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check                                  # and python3 -I bench/check_dep_sources.py
cargo build --profile wasm -p gpress-wasm --target wasm32-unknown-unknown   # needs: rustup target add wasm32-unknown-unknown
cd crates/gpress-wasm/js && npm ci && npm run build && npm test        # the npm package into js/dist/ (with wasm-opt if on PATH)
npm run test:web                                  # (in crates/gpress-wasm/js) the same tests on the web module
python3 -I bench/sizes.py check                   # the web module within 5% of its recorded size (bench/sizes.json); needs wasm-opt
cd fuzz && cargo +nightly fuzz run expand -- -max_total_time=600      # needs cargo-fuzz; also inflate, pdf_optimize
```

CI (`.github/workflows/ci.yml`) runs on every pull request and every push to `main`: formatting, clippy with
warnings as errors on both feature sets, both test suites, clippy and the tests again for the builds that
leave parts out (none of them, the web set, and every part but one), the C example against the shared
library, the licence and advisory gate, the WebAssembly package with the tests on both modules and the web
module's size budget, a short fuzzing run of the three decoders, a secret scan of every commit (gitleaks)
and a security audit of the workflows themselves (zizmor). Its last job, "All checks passed", passes only
when every other job did. CodeQL (`.github/workflows/codeql.yml`) scans the Rust, TypeScript, Python and
workflow code on every pull request and weekly. `tests/features.rs` checks each part's absence in the builds
without it, and the other test files run in every build each test whose parts it has (`tests/bounds.rs`,
`speed.rs` and `memory.rs` test the normal PDF in the web build too); `tests/api.rs` proves
that every option changes what the engine does; `tests/interaction.rs` runs every feature on one encrypted
file (a JPEG, a fax image, a Type 1 font and a zlib stream) in both outputs; `tests/pack_format.rs` keeps an
archive made by the first build of the format version (`tests/data/format-v8.*`), which must still restore and
pack to the same bytes.

The benchmark suite, the security suite and the independent checks are in `bench/` (described in the
[reference](docs/reference.md#benchmarks-bench)); [docs/optimisation-plan.md](docs/optimisation-plan.md)
and [docs/tasks/](docs/tasks/README.md) hold the plan and the briefs for the work in progress.

## Contributing

Pull requests are welcome. Every pull request runs the CI and CodeQL above and is merged only once they
pass; the pull request template lists what to run locally first. A change must keep
outputs exact (the archive restores byte for byte, a normal PDF keeps every object's value), and any speed,
size or memory figure in a pull request must be measured, saying how and on which files. New dependencies
must be permissively licensed and come from crates.io (`deny.toml`). Dependabot proposes dependency and action
updates every week. To report a vulnerability, see [SECURITY.md](SECURITY.md) rather than opening an issue.

## Licence

gpress is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT licence ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option. The files listed in [Code from other projects](docs/reference.md#code-from-other-projects)
also carry the licence notices of the projects they follow, which apply to those parts too and must be kept.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in gpress by
you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or
conditions.
