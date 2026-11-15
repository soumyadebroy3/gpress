_Last updated 2026-11-15, `main` at [b404ec4](https://github.com/soumyadebroy3/gpress/commit/b404ec4)._

Every pull request merged into `main` is listed in the [Changelog](Changelog). This page says what each recent one gives, what is being built now and what comes next.

## Done: the build of 2026-11-14 and 2026-11-15

All figures are from each pull request's description, on the 907 public test PDFs (130.5 MB) with one thread unless it says otherwise. "Byte-identical" means every output is the same bytes as `main` gave before, at the efforts named.

| Pull request | Merged (UTC) | What it gives |
| --- | --- | --- |
| [#47](https://github.com/soumyadebroy3/gpress/pull/47) Web module limit 1 MB | 2026-11-15 | CI's limit on the web module goes from 400,000 to 1,000,000 bytes of gzip. The check that fails a growth of more than 5% over the recorded size stays. No engine code changes. |
| [#43](https://github.com/soumyadebroy3/gpress/pull/43) Effort levels and a target size | 2026-11-15 | Six effort levels: 0, 20 and 40 lossless, 60, 80 and 100 lossy (images only), default 40. A new lossy image step with its own JPEG encoder and decoder. `--target-size` picks the lowest effort whose output fits. All outputs together: 86.88% of the input at effort 40 and 85.26% at effort 100; on a 23-file photo set, 90.29% and 77.94%. The lossy efforts change 4 of the 907 files. The web module grew by 46,794 bytes of gzip, to 359,089. See [Effort levels and settings](Effort-levels-and-settings). |
| [#46](https://github.com/soumyadebroy3/gpress/pull/46) Archive settings | 2026-11-15 | Every tuning value of the archive (76 rules) is a setting, from Rust for now. With the defaults the archive is version 9, as before, and byte-identical at efforts 0, 40 and 100. When a rule the model shares with `expand` changes, the archive is version 10 and carries the changed values in its header. |
| [#42](https://github.com/soumyadebroy3/gpress/pull/42) Earlier revisions | 2026-11-14 | A PDF that keeps earlier saves (incremental updates) was left untouched; now it is compressed with every save kept, each one checked. 35 files that were untouched before come out smaller: 13,416,799 to 11,181,318 bytes at effort 0. `--drop-history` writes only the newest save: 29 of them went from 13,323,674 to 3,542,487 bytes. |
| [#45](https://github.com/soumyadebroy3/gpress/pull/45) Flate settings | 2026-11-14 | The Flate encoders' 181 tuning values are named settings (group `flate_`), from Rust for now. Byte-identical at the defaults, at efforts 0, 40 and 100. |
| [#44](https://github.com/soumyadebroy3/gpress/pull/44) Safety gate | 2026-11-14 | A missing object's header is found in more spellings, and a trailer value nested 16 deep is refused; both leave such a file untouched. Byte-identical on the 907 files at effort 0. |
| [#41](https://github.com/soumyadebroy3/gpress/pull/41) Lossless images | 2026-11-14 | 1-bit images become JBIG2 when that is at least 5% smaller, and 8-bit grey or RGB images of few colours become Indexed (new option `palette`). At effort 0: 11 outputs changed, none larger, 52,367 bytes less in all. |
| [#40](https://github.com/soumyadebroy3/gpress/pull/40) Safety gate | 2026-11-14 | A file whose trailer does not reach a catalog is left untouched, and so is one whose output qpdf 12 could not read (its highest object number at or above a third of the new file's length). Byte-identical on the 907 files at effort 0. |
| [#38](https://github.com/soumyadebroy3/gpress/pull/38) Release workflow | 2026-11-14 | A pushed `v*` tag builds the tool for Linux (x86-64, arm64), macOS (x86-64, arm64) and Windows (x86-64), the C library for Linux x86-64 and the npm tarball, with `SHA256SUMS`, as a draft release. It has not run for a real tag yet. |
| [#37](https://github.com/soumyadebroy3/gpress/pull/37) aes 0.9.4 | 2026-11-14 | Dependency update. Byte-identical on the 907 files at effort 0. |
| [#39](https://github.com/soumyadebroy3/gpress/pull/39) Clippy fix | 2026-11-14 | A test loop that clippy of Rust 1.100 rejected, so CI passes again. |

Earlier pull requests are in the [Changelog](Changelog).

## In progress

| Work | What it will give |
| --- | --- |
| The settings mechanism, draft [#49](https://github.com/soumyadebroy3/gpress/pull/49) | Any setting by name on every surface: `--rule name=value` on the command line, `Options::set_rule` in Rust, a text field in C, calls in WebAssembly and a `rules` option in TypeScript, with one table per group in the reference. It brings the `flate_` and `archive_` groups to the command line, C and WebAssembly, and makes the lossy step's 15 values the group `lossy_`. |
| The rewrite's settings, branch `feat/settings-rewrite` | The normal-PDF rewrite's tuning values as the group `rewrite_` (88 rules). It lands after the mechanism. |
| Recognising real photos, draft [#52](https://github.com/soumyadebroy3/gpress/pull/52) | The lossy step counts an image in doubt as line art, and none of the three photographs of the photo set is taken for a photo today. This work adds a "sharp photo" answer to the photo test, so sharp photographs such as the set's astronaut and coffee cup can become JPEGs, and makes the test's 14 values rules of `lossy_`. |
| Dropping earlier saves always, draft [#51](https://github.com/soumyadebroy3/gpress/pull/51) | A file that keeps earlier saves will always be written as its newest save alone. Keeping every save, which #42 added, goes away, and so does the `--drop-history` option. |
| Code scanning, draft [#50](https://github.com/soumyadebroy3/gpress/pull/50) | CodeQL reports 17 critical results on `main`. Three come from arrays it reads as hard-coded but that are filled from the data; #50 rewrites those, and the others are to be dismissed with a written reason. Draft [#48](https://github.com/soumyadebroy3/gpress/pull/48) is temporary and only lists the results. |

Paused: the "Deep check" workflow, draft [#36](https://github.com/soumyadebroy3/gpress/pull/36), which would run both builds over the test files and the readers on every pull request.

## Next

- A fresh head-to-head on all 907 files against qpdf, MuPDF, cpdf, pdfsizeopt and Ghostscript, on the final `main` once the work above has landed.
- A security pass over the new code.
- Checks of the WebAssembly module in Firefox, Safari and on phones.
- The first release, v0.1.0: push the tag, then publish the draft release that the workflow makes.

## Known limits today

- The lossy step's photo test is cautious: an image in doubt counts as line art. Black 6 and 8 point labels on a dark part of a photograph may not be seen, so such a picture can be taken for a photo and its labels blurred.
- A progressive JPEG is left alone at every effort, and so is a baseline JPEG whose scan header is not as the standard writes it.
- CI builds and tests on Linux (x86-64) only.
