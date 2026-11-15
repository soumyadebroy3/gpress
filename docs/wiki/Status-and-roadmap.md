_Last updated 2026-11-15, `main` at [ff930b8](https://github.com/soumyadebroy3/gpress/commit/ff930b8)._

Every pull request merged into `main` is listed in the [Changelog](Changelog). This page says what each recent one gives, what is being built now and what comes next.

## Done: the build of 2026-11-14 and 2026-11-15

All figures are from each pull request's description, on the 907 public test PDFs (130.5 MB) with one thread unless it says otherwise. "Byte-identical" means every output is the same bytes as `main` gave before, at the efforts named.

| Pull request | Merged (UTC) | What it gives |
| --- | --- | --- |
| [#49](https://github.com/soumyadebroy3/gpress/pull/49) Settings by name | 2026-11-15 | Any rule is set by name on every surface: `--rule name=value` on the command line, `Options::set_rule` in Rust, a `name=value,...` text in the C options struct (which grows from 80 to 16,464 bytes), calls in WebAssembly and a `rules` option in TypeScript. The reference has one table per group, held equal to the code by a test. 272 rules in three groups: `lossy_` (the lossy step's 15 values, new here), `flate_` (181) and `archive_` (76). `lossy_walk_operators` and `lossy_walk_draws` can be lowered but not raised, because they bound what a hostile file can make the step do. A file whose lossy output is too short for its object numbers now gets the output of the strongest lossy level below that is long enough, and only then the lossless one (at once when `--photo-ppi`, `--art-ppi` or `--jpeg-quality` make the settings none of the levels'). With no rule set, byte-identical on the 907 files at efforts 0, 40 and 100 (`optimize` and `pack`), and at 60, 80 and 100 on the 26 files with lossy candidates. The web module grew by 8,097 bytes of gzip (2.4%), to 347,887. |
| [#51](https://github.com/soumyadebroy3/gpress/pull/51) Earlier saves dropped always | 2026-11-15 | A file that keeps earlier saves (incremental updates) is written as its newest save alone, each save read and checked on its own first. Keeping every save (#42) and the `--drop-history` option are gone (the command-line tool ignores flags it doesn't know, so it still accepts that one). At effort 0, 35 files change: 29 come out smaller (13,323,674 bytes in, 11,098,675 out with every save kept, 3,542,487 now), and 6 small ones that keeping every save shrank are now left untouched. A file with a signature, certification, usage rights, PDF/A or PDF/X marker in any save stays untouched. The web module got 19,299 bytes of gzip smaller. |
| [#50](https://github.com/soumyadebroy3/gpress/pull/50) Code scanning | 2026-11-15 | CodeQL reported 17 critical results, all "hard-coded cryptographic value". Three were arrays filled from the data, which CodeQL read as constants; they are now read straight from their bytes, so those three close. None of the other 14 is a vulnerability: 4 are false positives (the standard's empty password and zero vector, and a placeholder) and 10 are values used only in tests, each with a written reason in the pull request. Byte-identical on the 907 files at efforts 0, 40 and 100 (`optimize` and `pack`). |
| [#53](https://github.com/soumyadebroy3/gpress/pull/53) Wiki | 2026-11-15 | This wiki. Its pages live in [docs/wiki/](https://github.com/soumyadebroy3/gpress/tree/main/docs/wiki) and change through pull requests; a workflow copies them here on each push to `main` that changes them. |
| [#47](https://github.com/soumyadebroy3/gpress/pull/47) Web module limit 1 MB | 2026-11-15 | CI's limit on the web module goes from 400,000 to 1,000,000 bytes of gzip. The check that fails a growth of more than 5% over the recorded size stays. No engine code changes. |
| [#43](https://github.com/soumyadebroy3/gpress/pull/43) Effort levels and a target size | 2026-11-15 | Six effort levels: 0, 20 and 40 lossless, 60, 80 and 100 lossy (images only), default 40. A new lossy image step with its own JPEG encoder and decoder. `--target-size` picks the lowest effort whose output fits. All outputs together: 86.88% of the input at effort 40 and 85.26% at effort 100; on a 23-file photo set, 90.29% and 77.94%. The lossy efforts change 4 of the 907 files. The web module grew by 46,794 bytes of gzip, to 359,089. See [Effort levels and settings](Effort-levels-and-settings). |
| [#46](https://github.com/soumyadebroy3/gpress/pull/46) Archive settings | 2026-11-15 | Every tuning value of the archive (76 rules) is a setting, by name on every surface since #49. With the defaults the archive is version 9, as before, and byte-identical at efforts 0, 40 and 100. When a rule the model shares with `expand` changes, the archive is version 10 and carries the changed values in its header. |
| [#42](https://github.com/soumyadebroy3/gpress/pull/42) Earlier revisions | 2026-11-14 | A PDF that keeps earlier saves (incremental updates) was left untouched; now it is compressed with every save kept, each one checked. 35 files that were untouched before come out smaller: 13,416,799 to 11,181,318 bytes at effort 0. `--drop-history` writes only the newest save: 29 of them went from 13,323,674 to 3,542,487 bytes. #51 then made the newest save alone the only behaviour. |
| [#45](https://github.com/soumyadebroy3/gpress/pull/45) Flate settings | 2026-11-14 | The Flate encoders' 181 tuning values are named settings (group `flate_`), by name on every surface since #49. Byte-identical at the defaults, at efforts 0, 40 and 100. |
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
| The rewrite's settings, branch `feat/settings-rewrite` | The normal-PDF rewrite's tuning values as the group `rewrite_` (88 rules). The mechanism it needed is on `main`. |
| Recognising real photos, draft [#52](https://github.com/soumyadebroy3/gpress/pull/52) | The lossy step counts an image in doubt as line art, and none of the three photographs of the photo set is taken for a photo today. This work adds a "sharp photo" answer to the photo test, so sharp photographs such as the set's astronaut and coffee cup can become JPEGs, and makes the test's 14 values rules of `lossy_`. |

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
