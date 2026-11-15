gpress is a PDF compressor written in Rust, with C and WebAssembly bindings and a command-line tool. It is lossless by default: it makes smaller PDFs that standard readers open, and an archive that restores the original byte for byte.

This wiki follows the project's progress. The [README](https://github.com/soumyadebroy3/gpress#readme) and the [reference](https://github.com/soumyadebroy3/gpress/blob/main/docs/reference.md) on `main` stay the authority for how gpress works today. The pages are kept in [docs/wiki/](https://github.com/soumyadebroy3/gpress/tree/main/docs/wiki) and change through pull requests; each merge that changes them copies them here.

## Pages

- [Status and roadmap](Status-and-roadmap): what is done, what is being built now, and what comes next.
- [Effort levels and settings](Effort-levels-and-settings): the six effort levels, the lossy image step, the target size and the settings.
- [Changelog](Changelog): every pull request merged into `main`, newest first.

## The six goals

In the owner's order:

1. **The best PDF compressor.**
2. **The smallest compressed PDF.** The lossless levels stay lossless, and the output is always a standard PDF that every reader opens.
3. **World-class security.** Untrusted input is bounded, outputs are checked before they are returned, and a PDF gpress can't safely touch is left as it was.
4. **World-class customisation.** Every tuning value becomes a setting, with today's value as its default.
5. **Works on any device, any website and any file.** The same output from the native library and from the WebAssembly module.
6. **Very lightweight.** A small WebAssembly module for the web, with a size check in CI.

## Two outputs

| | Normal PDF | gpress archive (`.gpz`) |
| --- | --- | --- |
| Call | `optimize` | `compress` and `expand` (`pack` and `unpack` on the command line) |
| Takes | a PDF | any file, a PDF above all |
| Gives | a smaller PDF that standard readers open. Efforts 0, 20 and 40 are lossless; at 60, 80 and 100 images may change, within floors | an archive that restores the original byte for byte |
| Checked | the output is read back and compared with the input before it is returned | the archive is restored and compared before it is returned; if that check ever fails, a simpler model-only archive is written instead (not checked again) |

A file `optimize` can't make smaller, or can't safely touch (a signed file, PDF/A, an encrypted file that needs a password, a damaged one), comes back unchanged with the reason.

## Where it stands

Version 0.1.0, in active development. Nothing is published to crates.io or npm yet, so gpress builds from the repository. The archive format is version 9 (version 10 when one of the model's archive rules is changed from its default). CI builds and tests on Linux (x86-64). The licence is MIT OR Apache-2.0.

Figures from the README and `bench/` on `main` (907 public test PDFs, 130.5 MB, from the pdf.js, pypdf, OCRmyPDF and pikepdf test suites):

| What | gpress | Compared with |
| --- | --- | --- |
| Archive, 907 files, effort 100, built with Lepton | 60.9% of the original size | the best of xz -9e, Brotli 11 and Zstandard 19 --long, file by file: 76.3% |
| Normal PDF, the 90 files gpress rewrites from a hard 120-file sample, default effort (build 37bd3f3, before #41 and #42) | 85.01% of the input | qpdf 11.9.0 at its strongest lossless setting: 87.23% |
| Normal PDF, all 907 files, default effort 40 (lossless) | 86.9% of the input | |
| Normal PDF, all 907 files, effort 100 (lossy) | 85.3% of the input | |
| Web module (normal PDFs only), gzip | 353,062 bytes | CI fails it when it grows more than 5% over the recorded size, or passes 1,000,000 bytes ([#47](https://github.com/soumyadebroy3/gpress/pull/47)) |

The normal-PDF rows on all 907 files are one thread; the 171 files gpress leaves untouched count at their own size, and the lossy efforts change 4 of the 907. Those rows and the 171 were measured before [#51](https://github.com/soumyadebroy3/gpress/pull/51), which writes a file that keeps earlier saves as its newest save alone. At effort 0 that makes the total about 7.5 MB (about 5.8 points of the input) smaller: 29 files shrink from 11,098,675 to 3,542,487 bytes, and 6 small ones are now left untouched (177 in all). The other efforts have not been measured again yet. The qpdf row counts only the files gpress rewrites: on the 118 files qpdf can open, counting the files gpress leaves untouched as they are, qpdf comes out smaller (74.7% against 90.2% for gpress at effort 40, an earlier build), because it drops earlier revisions, unreferenced objects and whatever it cannot read; the README's Engine levels section has the details. The web module figure is from `bench/sizes.json`, recorded on 2026-11-15. A fresh head-to-head against qpdf, MuPDF, cpdf, pdfsizeopt and Ghostscript is running on `main` at 2d3bf64 (see [Status and roadmap](Status-and-roadmap)).

## Quick start

```sh
cargo install --locked --git https://github.com/soumyadebroy3/gpress gpress

gpress optimize in.pdf out.pdf                       # a smaller PDF that standard readers open
gpress optimize in.pdf out.pdf --quick               # the same at effort 0, the fast path
gpress optimize in.pdf out.pdf --target-size 500k    # the lowest effort that gives at most 500 KiB (60 and above are lossy)
gpress pack in.pdf in.gpz                            # the gpress archive (works on any file)
gpress unpack in.gpz back.pdf                        # the original, byte for byte
```

## Links

- [README](https://github.com/soumyadebroy3/gpress#readme): install, use from Rust, C and JavaScript, benchmarks
- [Reference](https://github.com/soumyadebroy3/gpress/blob/main/docs/reference.md): every option and every part of the engine
- [SECURITY.md](https://github.com/soumyadebroy3/gpress/blob/main/SECURITY.md): the threat model, the guarantees and how to report a problem privately
- [bench/RESULTS.md](https://github.com/soumyadebroy3/gpress/blob/main/bench/RESULTS.md): every measurement, with its build and machine
