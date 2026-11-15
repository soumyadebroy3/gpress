_As on `main` at [b404ec4](https://github.com/soumyadebroy3/gpress/commit/b404ec4) (2026-11-15). The README's [Effort levels](https://github.com/soumyadebroy3/gpress#effort-levels) and [Options](https://github.com/soumyadebroy3/gpress#options), and the [reference](https://github.com/soumyadebroy3/gpress/blob/main/docs/reference.md#options), have every detail._

## Effort levels

`--effort N` (the `effort` option) goes from 0 to 100 in steps of 20; a value in between counts as the step below it (59 is 40). The default is 40.

- **0, 20 and 40 are lossless.** They differ only in how hard the engine searches, and their outputs are checked the same way.
- **60, 80 and 100 are lossy.** They do the work of 40, after a step that changes images only.
- The archive is lossless at every effort; there 40 and above all mean its strongest setting.
- The output is the same for any thread count.

| Effort | | 907 test files | Photo set (23 files) | What it does to a normal PDF |
| ---: | --- | ---: | ---: | --- |
| the input | | 130,469,001 | 22,715,581 | |
| 0 (`--quick`) | lossless | 115,335,908 (88.4%) | 20,900,294 (92.0%) | The fast path: gpress's fast Flate encoder and the cheap steps. |
| 20 | lossless | 113,692,951 (87.1%) | 20,585,289 (90.6%) | The strong encoder's first step. |
| 40 (default) | lossless | 113,347,672 (86.9%) | 20,510,774 (90.3%) | The smallest lossless output: everything the engine knows how to do. |
| 60 | lossy | 111,788,415 (85.7%) | 18,255,139 (80.4%) | As 40, then an image shown at 375 pixels per inch or more is resampled down to 300, and a photo that was resampled, or is not a JPEG, is written as a JPEG at quality 85. Line art never becomes a JPEG: it stays samples behind Flate (still resampled down to 300 when shown at 375 or more). |
| 80 | lossy | 111,270,471 (85.3%) | 17,737,195 (78.1%) | As 60, with photos down to 150 pixels per inch (those shown at 187.5 or more). |
| 100 | lossy | 111,237,201 (85.3%) | 17,703,925 (77.9%) | As 80 at JPEG quality 75, and an existing JPEG photo is written again at that quality when that is at least a tenth smaller. |

The sizes are all outputs together in bytes and their share of the input, one thread (`bench/effort_table.py`). A file gpress leaves untouched counts with its own size: 171 of the 907 test files. The lossy efforts change 4 of the 907. The photo set is 23 files that `bench/photo_set.py` puts together to show the lossy step; bench/RESULTS.md, "Sizes at the six efforts", has a row per file of the photo set.

### Floors that hold at every lossy level

- Photos are never taken below 150 pixels per inch at their displayed size, and line art (screenshots, charts, diagrams) and scans never below 300. An image is resampled only when it is shown at 5/4 of its floor or more, and line art never becomes a JPEG.
- Only 8-bit grey and RGB images of more than 65,536 pixels are touched. 1-bit, indexed, CMYK and 16-bit images, image masks, small images (logos, icons) and a few other kinds are never changed.
- A lossy result is kept only when it is at most nine tenths of the image's lossless size and, for a JPEG, only when it decodes close to the samples it was made from. Otherwise the image goes through the lossless path.
- A file the safety gate leaves untouched (signed, certified, PDF/A, PDF/X, needing a password...) stays untouched, and an encrypted file keeps its encryption.
- A file that keeps earlier saves keeps them all, written losslessly (with `--drop-history` the step works on the newest save, which then goes on alone).
- A file whose object numbers are too high for the shorter lossy output (qpdf 12 reads an object only when its number is below a third of the file's length) gets the lossless one.

## Target size

`--target-size V` (the call `optimize_to_size`) runs the efforts 0, 20, 40, 60, 80 and 100 in turn and returns the first output of at most V: bytes, `k` or `m` for KiB or MiB, or `N%` of the input. The output line then ends with `effort=E target=reached` or `missed`. Efforts 60 and above are lossy, so a target can turn a lossless run into a lossy one. A target no effort reaches within the floors is missed rather than forced: the smallest of the six comes back, marked as missed.

```sh
gpress optimize in.pdf out.pdf --target-size 500k
gpress optimize in.pdf out.pdf --target-size 50%
```

From Rust it is `gpress::optimize_to_size`, from C `gpress_optimize_to_size`, and from TypeScript `optimizeToSize`.

## The lossy step's settings

| Flag | What it changes |
| --- | --- |
| `--photo-ppi N` | The pixels per inch a photo is resampled down to (never below 150; 0: what the effort says). Alone it changes nothing below effort 60: a photo is resampled only on its way to a JPEG, so it needs a JPEG quality, the effort's or `--jpeg-quality`. |
| `--art-ppi N` | The same for line art and scans (never below 300). Set, it makes the output lossy at any effort. |
| `--jpeg-quality N` | The JPEG quality, kept within 50 to 95. Set, it makes the output lossy at any effort. |

The library takes all of them at once as `lossy` (`Lossy::level(1)`, `(2)` and `(3)` are what efforts 60, 80 and 100 mean, and `Lossy::OFF` keeps any effort lossless). In TypeScript they are `photoPpi`, `artPpi` and `jpegQuality`.

## Other options

`lossy` and `--target-size` are above.

| Option | Flag | Default | What it does |
| --- | --- | --- | --- |
| `effort` | `--effort N`, `--quick` | 40 | See above. |
| `threads` | `--threads N` | 0 | 0: every core the machine offers, at most 8; at most 64. The output is the same for any count. |
| `time_budget` | `--time-budget MS` | none | Give up after this much wall time: an error and no output, the input unchanged. |
| `max_input` | `--max-input BYTES` | 1 GiB | Larger inputs are refused at once. |
| `max_len` | `--max-len BYTES` | 1 GiB | Expand: an archive declaring a larger original is refused before anything is decoded. |
| `max_decoded` | `--max-decoded BYTES` | 256 MiB | Archive: the most decoded bytes the parts of one archive may claim, both ways. |
| `respect_conformance` | `--ignore-conformance` | on | Normal PDF: leave PDF/A, PDF/X, signed, certified and usage-rights files untouched. |
| `drop_history` | `--drop-history` | off | Normal PDF: write only the newest save of a file that keeps earlier ones. Off, every save is kept. Planned: only the newest save, always, and no option (see [Status and roadmap](Status-and-roadmap)). |
| `jpeg_tables`, `jbig2`, `palette`, `flate_over_jpeg` | `--no-jpeg-tables`, `--no-jbig2`, `--no-palette`, `--no-flate-over-jpeg` | on | Normal PDF: turn single steps off. |

For measuring there is also `engine_effort` (`--engine-effort N`): the engine's own lossless level, 0 to 100. Efforts 0 and 20 are its levels 0 and 20, and 40 and above its level 100.

## Settings: every tuning value by name

The project's aim is no hard-coded tuning values: each value that decides whether or how a file is changed is becoming a setting (a "rule"), with today's value as its default and a lowest and a highest value. A rule's value out of range is refused, never clamped. Format facts, safety bounds on hostile input, limits that mirror what other PDF readers accept, and choices that exist in one variant only (listed in each group's pull request) stay fixed.

| Group | Part of the engine | Rules | State |
| --- | --- | ---: | --- |
| `flate_` | the Flate encoders | 181 | on `main` ([#45](https://github.com/soumyadebroy3/gpress/pull/45)), from Rust: `Options::flate_rules` |
| `archive_` | the archive | 76 | on `main` ([#46](https://github.com/soumyadebroy3/gpress/pull/46)), from Rust: `Options::archive_rules`. A change to one of the 51 rules the model shares with `expand` writes a version-10 archive that carries the changed values. |
| `lossy_` | the lossy image step | 15 | being built, with the mechanism (draft [#49](https://github.com/soumyadebroy3/gpress/pull/49)); the photo test's 14 more come with draft [#52](https://github.com/soumyadebroy3/gpress/pull/52) |
| `rewrite_` | the normal-PDF rewrite | 88 | being built, lands after the mechanism |

The mechanism being built in draft [#49](https://github.com/soumyadebroy3/gpress/pull/49) sets any rule by name on every surface: `--rule name=value` (repeatable) on the command line, `Options::set_rule(name, value)` in Rust, a `name=value,...` text in the C options struct, calls in WebAssembly, and a `rules` option by name in TypeScript. The reference will get one table per group, held equal to the code by a test.
