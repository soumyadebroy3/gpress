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
| Normal PDF, the 730 files gpress rewrites, default effort 40 (lossless) | 73.3% of the input | qpdf 12.4.2 with zopfli: 77.7%, the smallest of the other tools |
| Normal PDF, all 907 files, default effort 40 (lossless) | 81.4% of the input | qpdf 12.4.2 with zopfli: 79.6% |
| Normal PDF, all 907 files, effort 100 (lossy) | 79.7% of the input | |
| Web module (normal PDFs only), gzip | 352,596 bytes | CI fails it when it grows more than 5% over the recorded size, or passes 1,000,000 bytes ([#47](https://github.com/soumyadebroy3/gpress/pull/47)) |

The normal-PDF rows are one thread, measured on `main` at 2d3bf64; a file gpress leaves untouched counts at its own size, and the lossy efforts change 4 of the 907. The comparison ran against qpdf 12.4.2, MuPDF's mutool 1.28.6, cpdf 2.9.3, pdfsizeopt 2023-04-18 and Ghostscript 10.08.0, and an output counted only when PDFium, Poppler and pdf.js draw every page the same as the original's (a few edge pixels allowed in one renderer) and nothing is lost. The 730 files are all 907 but the 173 gpress declines (damaged, PDF/A or signed, encrypted and so on) and the 4 where it keeps a stream that decodes to more than 64 MiB as it is. On them gpress gives the smallest total of any tool; on all 907 it comes second, because qpdf with zopfli also shrinks those files. Raising the stream limit (`--max-stream`) gives 78.3% on all 907; the default limit is 64 MiB. The [README's Benchmarks section](https://github.com/soumyadebroy3/gpress#benchmarks) has the speed and memory figures. The web module figure is from `bench/sizes.json`, recorded on 2026-11-15.

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
