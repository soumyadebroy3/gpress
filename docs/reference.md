# gpress reference

Everything the [README](../README.md) leaves out: every option, the parts a build can leave out, what each
part of the engine does, how encrypted PDFs are handled, the dependency licence rule, the repository layout and
the benchmark and check scripts.

## Options

Everything a caller can tune is a field of `Options` (the C struct has every field but `store_budget`, with
`engine_effort` and `lossy` in the shapes their rows give and the groups of rules as one text, `rules`; the
TypeScript options have most of them, the rules by name too; `store_budget` stays at its default in both), with
`Options::check` saying whether the engine accepts a set; nothing is read from the environment except
`PDFE_DEBUG`, which only prints timings and figures to standard error. For a normal PDF that includes one
summary line per file, which `bench/p5.py` reads:

    plan: S streams, J jobs (I identical to an earlier one, H sharing their input); store: budget B, peak P bytes, K kept, X hits, Y computed, about Z s saved; segments planned once: N

the streams, the jobs that encode them (a stream identical to an earlier one is no job of its own; H jobs
have the encoder input of another under another dictionary), what the store held at most and answered (with
the encoding time its answers saved, roughly), and the segments written from an earlier segment's plans.

| Option | Default | What it does |
| --- | --- | --- |
| `effort` | 40 | 0 to 100 in steps of 20, a value in between counting as the step below it, both outputs: 0, 20 and 40 are lossless (the engine's levels 0, 20 and 100, next row), and 60, 80 and 100 are the work of 40 after the lossy image step, which the archive never takes (see [Effort levels](#effort-levels)). |
| `engine_effort` | none | For measuring: the engine's own lossless level, 0 (fastest) to 100 (smallest), in place of the one the effort means, both outputs (`--engine-effort N`; C: `effort_is_engine_level` set to 1, with the level in `effort`; TypeScript: `engineEffort`). Set, the effort decides nothing, and nothing is lossy unless `lossy` is set too. Normal PDF: 0 is the fast path, our fast Flate encoder (`src/flate/fast.rs`) with its near-optimal level for page content, fonts and the rest (one pass of it for a stream that decodes to more than 4 MiB) and a lazy level for images, image predictors chosen from an estimate instead of trial encodes, and only the cheap steps (object streams, the cross-reference stream, JPEG Huffman tables, JBIG2 for fax images); on the 120-file sample it took 16.1 s of CPU when first measured, against 25.3 s for qpdf's strongest lossless setting and 195.5 s for the strong encoder that effort 0 used before (one thread; files of 1 to 10 MB: median 0.20 s, p90 1.14 s, slowest 3.9 s). 1 to 19 bridge to the strong encoder: images get closer parses, from 10 the streams that take the most room get the strong encoder's quick settings within a budget that doubles every point (and so do the object streams and the cross-reference stream when they fit), except a Flate stream that effort 0's settings can't shrink, which is kept as it is. 20 to 100 are the strong encoder: the Flate settings go in steps of 20, the streams that take the most room get the strongest step that fits a work budget proportional to the file's size, and image predictors are ranked on sampled rows (all as before the fast path). Below 100 the decoded bytes have a budget too (16 per byte at effort 0, twice as much every 10 points, at least 4 MiB), so the time follows the file's size even when its streams decode to a hundred times their size. On the sample, as first measured for the fast path in October 2026 (CPU s, output as a share of the input on the 90 files it rewrites): 0: 16.1, 87.02%; 5: 23.0, 86.70%; 10: 40.7, 86.11%; 15: 111, 85.74%; 19: 183, 85.59%; 20: 244, 85.43%; 50: 334, 85.17%; 100: 983, 85.01% (qpdf's strongest lossless setting: 87.23% counting only the outputs bench/verify_pdf.py finds the same apart from the trailer's /ID). Later changes made 20, 50 and 100 faster without changing their output: measured again on a machine about 1.2 times slower, the current code takes 16.8 s at 0, 176 s at 50 and 512 s at 100 (bench/RESULTS.md, "Effort levels"). In the archive, data that looks compressed already is coded plainly below 50. Decided from sizes only, never from time. |
| `optimize_to_size` (a call, not a field) | none | Normal PDF, a target size in place of an effort: `optimize_to_size(input, target, &options)` runs `optimize` at efforts 0, 20, 40, 60, 80 and 100 in turn, so the engine up to six times, and returns the first result of at most `target` bytes, with the effort that made it (`--target-size V`, where V is bytes, with `k` or `m` for KiB or MiB, or `N%` of the input, and the output line then ends with `effort=E target=reached` or `missed`; C: `gpress_optimize_to_size`; TypeScript: `optimizeToSize`). Efforts 60 and above are lossy, so a target can turn a lossless run into a lossy one. A target no effort reaches within the floors is missed rather than forced: the smallest of the six comes back, marked as missed (`reached` false). The options' own effort is not used, `engine_effort` and `lossy` must not be set (`Error::InvalidOptions`), and one `time_budget` covers the whole call. |
| `threads` | 0 | 0: every core the machine offers, at most 8 (each thread holds a stream of its own, so memory grows with them, until a budget for what they hold at once exists); at most 64. The output is identical for any count (the pieces of work are the same and are put together in order); a thread the system refuses is no loss. No more stream jobs or encoders run at once than there are threads (`SECURITY.md`, "Memory with threads"). |
| `store_budget` | 2 MiB | Normal PDF: bytes of encoded outputs one file keeps, so that an input of at most 256 KiB that another stream also encodes (the same data under another dictionary, say) is encoded once; a larger repeat is encoded again. The oldest go first, and each kept output counts 512 bytes besides its own; 0 keeps none, at most 1 GiB. A kept output is used only after it decodes to the same input. Never changes the output; nothing is kept from one call to the next (`SECURITY.md` says why). The `PDFE_DEBUG` summary gives its peak and hits. |
| `max_input` | 1 GiB | Larger inputs are refused at once. |
| `max_stream` | 64 MiB | Normal PDF: a stream that decodes to more is kept as it is. Object streams are decoded once each (in an encrypted file whose metadata is not encrypted, the catalog's once more) and let go once their objects are read; each may decode to this much or 256 MiB, whichever is less, at each filter stage; the ones held at once to four times that, each header entry kept counted as 16 bytes; and all of them, their stored bytes and every stage counted, to four times that or 64 bytes per byte of the file, whichever is more. A file past any of these is left as it is (a `max_stream` of 0 leaves every file that has an object read from an object stream; the C and TypeScript options read 0 as the default). These count decoded bytes, not the objects parsed from them, which take more: an array of small integers about 35 times its bytes (48 MiB of them, in a file of 50 KB within every default limit, took 1.7 GB). |
| `max_stream_archive` | 256 MiB | Archive: a stream that decodes to more is stored as it is. The object streams read to open an encrypted file are held to it as `max_stream` holds them; past it, the file's encrypted streams are stored as they are. |
| `max_decoded` | 256 MiB | Archive, both ways. Compress: the most decoded bytes the parts of one archive may claim in all; in file order, a stream whose decoded form (plus any Lepton data for it) does not fit in what is left is stored as it is, decided from sizes alone. The archive's header declares the total. Expand: an archive declaring more than this is refused before anything is decoded (`Error::DecodedTooLarge`), and while decoding every part is held to what the header declared, so a damaged body can claim no more. The test set's largest file needs 232 MiB (`enormous.pdf`, 310 KB). |
| `max_len` | 1 GiB | Expand: an archive declaring more is refused before anything is decoded or reserved (`Error::DeclaredTooLarge`). The format's own limit is 16 GiB. |
| `objects_per_stream` | 200 | Normal PDF: most objects per compressed object stream. |
| `respect_conformance` | on | Normal PDF: leave PDF/A, PDF/X, signed, certified and usage-rights files untouched. Off, every object's value is still kept (at efforts 0 to 40), but those guarantees are given up. |
| `flate_over_jpeg` | on | Normal PDF: also try our Flate in front of a JPEG (never in front of JPEG 2000 or JBIG2, whose filter chain keeps its shape for readers such as pdf.js). |
| `jpeg_tables` | on | Normal PDF: JPEGs get optimal Huffman tables (the same coefficients). |
| `jbig2` | on | Normal PDF: CCITT fax images become JBIG2 when that is at least 5% smaller (the same pixels), and so do 1-bit images stored without an image codec (a DeviceGray image or a stencil mask under Flate, LZW, RunLength, the ASCII filters or none) when JBIG2 is at least 5% smaller than the best other encoding of the stream. |
| `palette` | on | Normal PDF: an 8-bit DeviceGray or DeviceRGB image that uses few colours becomes an Indexed image over its own colour space (the same colour for every pixel) when the whole stream, its colour table counted, comes out smaller. |
| `rewrite_encrypted` | on | Normal PDF: files the empty user password opens are rewritten with their encryption kept exactly. Off, every encrypted file is left untouched. |
| `lossy` | none | Normal PDF: the lossy image step. `None`: what the effort means (efforts 60, 80 and 100 turn it on). `Some`: exactly these settings whatever the effort, and `Some(Lossy::OFF)` keeps any effort lossless. The settings are `photo_ppi` and `art_ppi` (photos, and line art and scans, shown at 5/4 of this many pixels per inch or more (the rule `lossy_resample_margin`) are resampled down to it; never below the rules `lossy_photo_floor` and `lossy_art_floor`, 150 and 300 unless they are set; 0: never resampled; a photo is resampled only on its way to a JPEG, so `photo_ppi` does nothing while `jpeg_quality` is 0), `jpeg_quality` (kept within the rules `lossy_quality_min` and `lossy_quality_max`, 50 to 95, for a photo that is resampled or stored without JPEG; 0: no image becomes a JPEG) and `rejpeg` (an existing JPEG photo is written again at that quality when that is at least a tenth smaller); `Lossy::level(1)`, `(2)` and `(3)` are what efforts 60, 80 and 100 mean. Command line: `--photo-ppi N`, `--art-ppi N` and `--jpeg-quality N` change one setting each, over the effort's (0: as it is); a run that changed an image adds `resampled=`, `to_jpeg=` and `rejpeg=` to its output line. `Report` counts the same (`images_resampled`, `images_to_jpeg`, `jpegs_rewritten`). A build without the `jpeg` feature resamples line art only. C (`photo_ppi`, `art_ppi`, `jpeg_quality`) and TypeScript (`photoPpi`, `artPpi`, `jpegQuality`) have those three overrides, where 0 or absent means what the effort says. On every surface `art_ppi` or `jpeg_quality` set makes the output lossy at any effort, and `photo_ppi` alone changes nothing below effort 60, where no JPEG quality is set. |
| rules (`set_rule`, `set_rules`, `rule`, `Options::rules()`; the fields `lossy_rules`, `flate_rules` and `archive_rules` hold the groups) | each rule's own | The engine's tuning values by name, each with a default, a lowest and a highest value: `set_rule("lossy_photo_floor", 100)`, `set_rule("flate_e100_depth", 4096)`. A rule that is not set has its default; a name that is no rule or a value outside the range is an error. See [Rules](#rules) for the tables and for the command line, C and TypeScript. |
| `decrypt` | on | Archive: the streams of such files are stored decrypted (so they get the zlib rebuild, Lepton and the model) and encrypted again on expand. |
| `use_existing_libraries` | on | Use Lepton for JPEG streams in the archive when the build has it (`--features existing`). An archive with parts Lepton made expands only with such a build (the error says so). |
| `time_budget` | none | Give up after this much wall time: `Error::TimeBudget` and no output, the input unchanged. Checked between streams, Flate segments and archive parts (every 64 KiB of model work) in all three operations, so it can be overshot by a little (on expand by one part's zlib re-creation). A call that finishes in time gives the same bytes as one without a budget. |
| `archive_rules` | defaults | Archive: the archive's tuning values (`ArchiveRules`, `src/pack/rules.rs`), each with a name, a default (what the archive did when it was a constant), a lowest and a highest value; `set` refuses a name it does not know or a value out of range. Most are choices of compress alone. The first 51 are the model's, which expand must share: with any of them changed the archive is version 10 and carries them in its header (see the gpress archive below), and expand takes them from there, never from its caller (of these options it reads `deadline_step` only). They are the rules `archive_`, set by name everywhere (see [Rules](#rules)). |

Memory while expanding follows these three: at most about `max_len` for the output, `max_len + max_decoded` for
the model's history (one byte per byte it codes, and while an image part is coded a ring of its residuals: about a
row and a pixel, never more than the part), and the model's tables (up to 164 MiB for files over 4 MiB; a
version-10 archive's profile can make them 176 MiB, and the fax model's hashed tables and correction maps 41.1 MiB
when it has a fax part, whatever its declared length: expanding a 4 KB file with a fax image, packed with the
largest tables, peaked at 147 MiB, against 20.5 MiB with the defaults). A decoded stream is not kept beside the
history: its zlib stream is made again from there, with one copy of the part only for PNG or TIFF predictor rows
or a JPEG, and up to three while a Type 1 font is closed again (each at most `max_decoded` and 2 GiB). Compress
holds the input and its archive. Its encoder holds the
same history and tables and no copy of the file: a stream is inflated again as it is coded, a Deflate block at
a time (a Type 1 font whole; for PNG predictor rows also their row types, a byte a row, and about 64 KiB of
rows, or one row if longer). The planner keeps no decoded Flate data, only each stream's zlib settings and
Lepton data, a fax stream's image and an encrypted stream's decrypted bytes. Every pack ends with a restore
check, which is an expand as above: after the encoder with one thread, beside it with two (two histories and
two sets of tables at once). The security suite runs every pack and unpack with `max_decoded` at 64 MiB and
holds them to 512 MiB.

Three kinds of constants stay in the code: the ones that define the file formats (a decoder must agree with
its encoder on those: the model's settings, the part tags, the fax work budget); the ones readers set (JBIG2
images of at most 2^30 pixels, the EOL rules readers disagree on); and measured thresholds (JBIG2 only when
5% smaller, at most 2048 pixels of fax work per byte, at most 4096 JPEG segments, plain coding below engine level
50, fax streams of at least 64 bytes, Flate in front of a JPEG only when it saves 1%, the second Flate pass
only for JPEGs with poor tables, Lepton only when 10% smaller than the Flate bytes, the quick budget's 16
bytes per byte and 4 MiB, 16 cross-reference rows per byte of the file plus 65536). The engine's own types
(`pdf::rewrite::Options`, `pack::PackOptions`, `pack::UnpackOptions`, `flate::Options` with its iterations,
exact rounds, chain length, segment size, splitting, alternatives, multi-start limit, nice length and
polishing) are public too, for measuring and for callers that want a knob the library's `Options` doesn't
carry.

## Effort levels

The effort goes from 0 to 100 in steps of 20; a value in between counts as the step below it (59 is 40), and
the default is 40. Efforts 0, 20 and 40 are lossless. Efforts 60, 80 and 100 are lossy: the work of 40, after
a step that changes images only. The archive is lossless at every effort: it takes the engine's level from the
effort (40 and above all mean its strongest setting) and nothing else.

| Effort | | Engine level | What it does to a normal PDF |
| ---: | --- | ---: | --- |
| 0 | lossless | 0 | The fast path. |
| 20 | lossless | 20 | The strong encoder's first step. |
| 40 (default) | lossless | 100 | The smallest lossless output: everything the engine knows how to do. |
| 60 | lossy | 100 | The work of 40, after the lossy image step at `Lossy::level(1)`: an image shown at 375 pixels per inch or more is resampled down to 300 (an area average), and a photo that was resampled, or is not a JPEG, is written as a baseline JPEG at quality 85 (line art stays samples behind Flate). |
| 80 | lossy | 100 | `Lossy::level(2)`: as 60, with photos down to 150 pixels per inch (those shown at 187.5 or more; a sharp photo stays at 300). |
| 100 | lossy | 100 | `Lossy::level(3)`: as 80 at JPEG quality 75, and an existing JPEG photo is written again at that quality when that is at least a tenth smaller. |

What holds at every lossy level. The numbers are defaults: each is a rule with a name and a range, and a
setting can move it within that range ([Rules](#rules), below):

- photos are never taken below 150 pixels per inch at their displayed size (`lossy_photo_floor`, which can go
  down to 72), and line art (screenshots, charts, diagrams) and scans (an image that covers 70% of a page or
  more, `lossy_scan_cover`) never below 300 (`lossy_art_floor`, down to 150); an image is resampled only when
  it is shown at 5/4 of its floor or more (`lossy_resample_margin`), and line art never becomes a JPEG;
- the step works only on 8-bit grey and RGB images of more than 65,536 pixels (`lossy_min_pixels`) whose every
  use on a page it could measure: 1-bit, indexed, CMYK and 16-bit images, image masks, images with a
  colour-key mask, a `/Decode` array or a soft mask with `/Matte`, and small images (logos, icons) are never
  touched, and neither is a JPEG that looks like line art;
- a lossy result is kept only when it is at most nine tenths of the image's lossless size (`lossy_min_saving`,
  which a setting can raise but not lower) and, for a JPEG, only when it decodes close to the samples it was
  made from (`lossy_min_psnr`, `lossy_min_block_psnr`; one that does not is first coded again at a higher
  quality); otherwise the image goes through the lossless path as before;
- a file the safety gate leaves untouched (signed, PDF/A, PDF/X, needing a password...) stays untouched, and an
  encrypted file keeps its encryption; a file that keeps earlier saves (incremental updates) is written as its
  newest save alone, which the step works on; a file whose object numbers are too high for the shorter lossy
  output (qpdf 12 reads an object only when its number is below a third of the file's length) gets that of the
  strongest lossy level below that is long enough (so 80 and 100 can give a lower level's output), or else the
  lossless one (at once when `photo_ppi`, `art_ppi` or `jpeg_quality` make the lossy settings none of the
  levels'). No setting changes this.

What the six efforts make of the 907 test files and of a set of photographs is in the README's table
("Effort levels") and, with a row per file of the photo set, in bench/RESULTS.md ("Sizes at the six efforts").
What decides that an image is a photo is cautious: an image in doubt counts as line art. On that set two of the
three scikit-image photographs, the astronaut and the coffee cup, are sharp photos, which become JPEGs but are
resampled no lower than the line-art floor, and the Hubble deep field stays line art. A progressive JPEG is left alone at every level.

## Rules

A rule is one of the engine's tuning values as a setting: a whole number with a name, a default (the value
that was fixed in the code before), a lowest and a highest value. A rule that is not set has its default. The
rules come in groups, one for each part of the engine; a rule's full name is its group's prefix and its own name.

| Where | How rules are set |
| --- | --- |
| Rust | `options.set_rule("lossy_photo_floor", 100)?`, or several from text with `options.set_rules("lossy_photo_floor=100,lossy_min_saving=20")?`; `options.rule(name)` reads one, and `Options::rules()` lists every rule with its default, lowest and highest value |
| Command line | `--rule lossy_photo_floor=100`, with commas between for more, or the flag again |
| C | `strcpy(options.rules, "lossy_photo_floor=100,lossy_min_saving=20");` (the text lies in the options themselves, `char rules[GPRESS_RULES_TEXT]`, 16,384 bytes with its closing NUL, which every rule at once fits; empty for none) |
| TypeScript | `{ rules: { lossy_photo_floor: 100, lossy_min_saving: 20 } }` |

A name that is no rule, or a value outside the rule's range, is an error everywhere (`InvalidOptions`, code 5;
the command line exits with 5), with a message that says which values the rule takes. Nothing is moved to the
nearest allowed value silently. `optimize_to_size` takes rules too.

### `lossy_`: the lossy image step

These are the same at every lossy effort, and they change nothing at a lossless one.

| Rule | Default | Lowest | Highest | What it decides |
| --- | ---: | ---: | ---: | --- |
| `lossy_min_pixels` | 65,536 | 10,000 | 4,294,967,295 | An image of at most this many pixels is never touched (logos, icons). |
| `lossy_photo_floor` | 150 | 72 | 65,535 | The least `photo_ppi` can be, in pixels per inch at the displayed size: a lower one is raised to it. The efforts ask for 300 or 150, so a lower floor changes nothing until `photo_ppi` asks for less. |
| `lossy_art_floor` | 300 | 150 | 65,535 | The same for `art_ppi`: line art and scans. |
| `lossy_resample_margin` | 125 | 100 | 1,000 | An image is resampled only when it is shown at this percent of its floor or more (125: at 5/4 of it). An image is never given its own size back. |
| `lossy_min_saving` | 10 | 10 | 99 | A lossy result is kept only when it is this percent smaller than the image's lossless size or more (10: at most nine tenths of it); the same for a JPEG written again. |
| `lossy_quality_min` | 50 | 1 | 100 | The least `jpeg_quality` can be. |
| `lossy_quality_max` | 95 | 1 | 100 | The most it can be, and where the steps of `lossy_quality_step` stop. When it is under `lossy_quality_min`, it wins. |
| `lossy_quality_step` | 10 | 1 | 99 | A new JPEG that fails the two checks below is coded again this much higher. |
| `lossy_quality_tries` | 3 | 1 | 10 | How many qualities are tried, the first one counted (1: only the first). Each is a full encode and decode of the image. |
| `lossy_min_psnr` | 35 | 1 | 99 | The least peak signal-to-noise ratio, in dB, of a new JPEG's luma against the samples it was made from, over the whole image. |
| `lossy_min_block_psnr` | 26 | 1 | 99 | The same in the image's worst block. |
| `lossy_psnr_block` | 16 | 8 | 64 | The side of those blocks, in pixels. |
| `lossy_scan_cover` | 70 | 1 | 100 | An image drawn over this percent of its page or more is a scan: it gets the line-art floor whatever it looks like. |
| `lossy_walk_operators` | 20,000,000 | 1 | 20,000,000 | Content operators the step may read in one file to measure where images are shown (decoding is charged to it too). Over it no image of the file is changed. |
| `lossy_walk_draws` | 1,000,000 | 1 | 1,000,000 | Image and form draws it may follow in one file, a form's counted every time the form is drawn. Over it no image of the file is changed. |
| `lossy_flat_run` | 32 | 8 | 256 | The test that tells a photo from line art, this rule and the thirteen below. A sample that ends a run of at least this many equal samples, along its row or down its column, is flat: screenshots, charts and diagrams have large areas of one value. |
| `lossy_hard_step` | 64 | 2 | 255 | A step of this many levels (of 256) or more between a sample and its left or upper neighbour is a hard edge. |
| `lossy_soft_step` | 32 | 1 | 254 | A step of at least this many levels and fewer than `lossy_hard_step` is a soft edge. |
| `lossy_ground_levels` | 8 | 1 | 64 | This many of an image's most frequent levels are its ground: a scanned page's paper is one. |
| `lossy_photo_flat` | 16 | 1 | 65,536 | A photo has at most one flat sample in this many (16: at most 6.25% of it is flat). |
| `lossy_photo_hard` | 128 | 1 | 65,536 | A photo has at most one hard edge in this many samples. |
| `lossy_photo_ground` | 50 | 1 | 100 | A photo has at most this percent of its samples in its ground. |
| `lossy_sharp_flat` | 8 | 1 | 65,536 | A sharp photo (more hard edges than a photo may have) has at most one flat sample in this many. |
| `lossy_sharp_ground` | 25 | 1 | 100 | A sharp photo has at most this percent of its samples in its ground. |
| `lossy_sharp_edges` | 100 | 1 | 1,000 | A sharp photo has at most this percent as many hard edges as soft ones (100: no more hard than soft). |
| `lossy_sharp_floor` | 0 | 0 | 1 | The floor of a sharp photo: 0 the line-art floor, 1 the photo floor. |
| `lossy_print_tile` | 32 | 8 | 1,024 | Small print is looked for in square tiles of this many samples a side; an image with small print is line art. |
| `lossy_print_share` | 8 | 1 | 65,536 | A tile is small print when more than one sample in this many is a hard edge (1: no tile is) |
| `lossy_print_edges` | 150 | 1 | 1,000 | and its hard edges are more than this percent of its soft ones. |

Why the ranges end where they do:

- `lossy_photo_floor` and `lossy_art_floor` are a guard against images that can no longer be read, so they are
  the defaults, and no setting goes under 72 and 150 pixels per inch.
- `lossy_min_saving` cannot go under its default. The lossless size a result is compared with is a quick
  encode; the strongest lossless encode of the photographs in the photo set is up to 5% smaller, so with a
  saving under that a lossy output could be larger than the lossless one.
- `lossy_min_pixels` cannot go under 10,000: a smaller image has too few samples to tell a photo from line
  art, and a few bytes decide whether its file grows.
- `lossy_walk_operators` and `lossy_walk_draws` bound the memory and time a hostile file can make the step
  take (the walk keeps every form's draws it has read, one for every 17 operators), so their highest values
  are their defaults: a setting can lower them but not raise them.
- `lossy_resample_margin` ends at 1,000 (an image shown at ten times its floor) and `lossy_quality_tries` at
  10 (each try is a full encode and decode of the image): both are choices, not limits of the number.
- The photo test's rules cost one pass over an image's samples whatever their values. `lossy_flat_run` and
  `lossy_print_tile` start at 8, the side of a JPEG's block. `lossy_print_tile` ends at 1,024, longer than most
  images' sides, so that no tile is looked at; `lossy_flat_run` ends at 256: at the default `lossy_min_pixels` an
  image the step changes has more than 65,536 = 256 by 256 pixels, so a run of 256 fits along its longer side.
  `lossy_hard_step` is 2 to 255 levels and `lossy_soft_step` 1 to 254, so that a soft step can be below a hard
  one. `lossy_ground_levels` ends at 64, a quarter of the 256 levels, a choice: a ground is a few levels. A
  share is one sample in 1 to 65,536, and `lossy_sharp_edges` and `lossy_print_edges` end at 1,000 percent, a
  choice: ten times as many hard edges as soft, over five times the least that rendered text has.
- The other lowest and highest values are where the number stops meaning anything (a percent, a JPEG quality,
  a block that is smaller than a JPEG's own or larger than a small image).

The test that tells a photo from line art is the fourteen rules from `lossy_flat_run` on, and their defaults were
measured together on photographs, scans, rendered pages and labels drawn on photographs. It sorts an image's
samples into three kinds. A photo is a clear continuous-tone picture: few flat samples, few hard edges, no ground.
A sharp photo has more hard edges than a photo may have, as many as text has (grass, gravel, a picture made
smaller), but no ground and no more hard edges than soft ones, where rendered text has at least 1.9 times as many.
Everything else, and everything in doubt, is line art, and so is an image with small print in it: a tile dense
with hard edges that outnumber its soft ones half as much again, as labels, a caption or a subtitle on a
photograph are. A page scanned in uneven light, a map on shaded relief or a halftone can pass for a sharp photo,
so a sharp photo keeps the line-art floor, and the two quality checks guard its JPEG. What the test does not
find: a word or two of print, print that is soft itself (a photographed or scanned note), print whose tiles have
at most 1.5 times as many hard edges as soft ones (`lossy_print_edges`; 100 finds more of it, and gives real
photographs such tiles too), and print close in brightness to the photograph around it (black labels on a dark
sky or rock, white ones on pale rock: their edges are under 64 levels); such a photo still becomes a JPEG at the
photo floor.

What is not a rule: which images the step can read at all (8-bit grey and RGB, sides up to 65,535), the limits
of its content reader, which are those other readers have, and what the JPEG format fixes (the standard's
tables, colour at half resolution, baseline coding).

### `flate_`: the Flate encoders

The values the two Flate encoders of the normal PDF work by (`FlateRules`, `src/flate/rules.rs`; the field
`flate_rules`; the archive does not use them). With none set every stream is encoded as it always was, and any
value in a rule's range gives a valid stream, only larger or smaller, sooner or later. The strong encoder has a row of
settings, the rules `flate_quick_` to `flate_e100_`, for its quick settings and for each band of the engine's
lossless levels: 20 to 39, 40 to 59, 60 to 79, 80 to 99 and 100. Efforts 0 and 20 are the engine's levels 0
and 20, efforts 40 and above its level 100, so the efforts use the rows `flate_e20_` and `flate_e100_`, and
`flate_e40_` to `flate_e80_` matter only with `engine_effort`. From level 20 the PDF path ranks an image's
predictor candidates with the quick settings and, below level 100, gives them to the streams outside its
budget. The engine's levels 0 to 19 (effort 0) are the fast encoder:
`flate_fast_level`, its twelve levels `flate_level1_` to `flate_level12_`, and the rules `flate_fast_`.

| Rule | Default | Lowest | Highest | What it decides |
| --- | ---: | ---: | ---: | --- |
| `flate_quick_rounds` | 2 | 0 | 16 | Pricing rounds of the parse (0: none; at most twice the strongest row's default). |
| `flate_quick_exact_rounds` | 1 | 0 | 8 | Exact rounds after them (0: none; at most twice the strongest row's default). |
| `flate_quick_depth` | 128 | 0 | 32,768 | How many earlier positions the match finder compares (0: no matches; 32,768: the whole window). |
| `flate_quick_segment` | 1,048,576 | 4,096 | 1,048,576 | The bytes optimised together (at least 4,096, so that the match finder's tables are not made anew every few bytes; at most the 1 MiB it always was, which bounds the memory of a thread). |
| `flate_quick_split` | 1 | 0 | 1 | Block splitting: 1 on, 0 off. |
| `flate_quick_alternatives` | 0 | 0 | 1 | Farther-match alternatives: 1 on, 0 off. |
| `flate_quick_second_start` | 0 | 0 | 1,048,576 | The size up to which a segment is also started a second way (0: never). |
| `flate_quick_nice` | 64 | 3 | 259 | The length from which a match is taken at once (259: never). |
| `flate_quick_polish` | 0 | 0 | 1 | Block polishing: 1 on, 0 off. |
| `flate_e20_rounds` | 2 | 0 | 16 | As `flate_quick_rounds`, for the engine's levels 20 to 39. |
| `flate_e20_exact_rounds` | 1 | 0 | 8 | As `flate_quick_exact_rounds`, for the engine's levels 20 to 39. |
| `flate_e20_depth` | 1,024 | 0 | 32,768 | As `flate_quick_depth`, for the engine's levels 20 to 39. |
| `flate_e20_segment` | 1,048,576 | 4,096 | 1,048,576 | As `flate_quick_segment`, for the engine's levels 20 to 39. |
| `flate_e20_split` | 1 | 0 | 1 | As `flate_quick_split`, for the engine's levels 20 to 39. |
| `flate_e20_alternatives` | 0 | 0 | 1 | As `flate_quick_alternatives`, for the engine's levels 20 to 39. |
| `flate_e20_second_start` | 0 | 0 | 1,048,576 | As `flate_quick_second_start`, for the engine's levels 20 to 39. |
| `flate_e20_nice` | 259 | 3 | 259 | As `flate_quick_nice`, for the engine's levels 20 to 39. |
| `flate_e20_polish` | 0 | 0 | 1 | As `flate_quick_polish`, for the engine's levels 20 to 39. |
| `flate_e40_rounds` | 3 | 0 | 16 | As `flate_quick_rounds`, for levels 40 to 59. |
| `flate_e40_exact_rounds` | 1 | 0 | 8 | As `flate_quick_exact_rounds`, for levels 40 to 59. |
| `flate_e40_depth` | 1,024 | 0 | 32,768 | As `flate_quick_depth`, for levels 40 to 59. |
| `flate_e40_segment` | 1,048,576 | 4,096 | 1,048,576 | As `flate_quick_segment`, for levels 40 to 59. |
| `flate_e40_split` | 1 | 0 | 1 | As `flate_quick_split`, for levels 40 to 59. |
| `flate_e40_alternatives` | 0 | 0 | 1 | As `flate_quick_alternatives`, for levels 40 to 59. |
| `flate_e40_second_start` | 0 | 0 | 1,048,576 | As `flate_quick_second_start`, for levels 40 to 59. |
| `flate_e40_nice` | 259 | 3 | 259 | As `flate_quick_nice`, for levels 40 to 59. |
| `flate_e40_polish` | 0 | 0 | 1 | As `flate_quick_polish`, for levels 40 to 59. |
| `flate_e60_rounds` | 4 | 0 | 16 | As `flate_quick_rounds`, for levels 60 to 79. |
| `flate_e60_exact_rounds` | 2 | 0 | 8 | As `flate_quick_exact_rounds`, for levels 60 to 79. |
| `flate_e60_depth` | 2,048 | 0 | 32,768 | As `flate_quick_depth`, for levels 60 to 79. |
| `flate_e60_segment` | 1,048,576 | 4,096 | 1,048,576 | As `flate_quick_segment`, for levels 60 to 79. |
| `flate_e60_split` | 1 | 0 | 1 | As `flate_quick_split`, for levels 60 to 79. |
| `flate_e60_alternatives` | 1 | 0 | 1 | As `flate_quick_alternatives`, for levels 60 to 79. |
| `flate_e60_second_start` | 0 | 0 | 1,048,576 | As `flate_quick_second_start`, for levels 60 to 79. |
| `flate_e60_nice` | 259 | 3 | 259 | As `flate_quick_nice`, for levels 60 to 79. |
| `flate_e60_polish` | 1 | 0 | 1 | As `flate_quick_polish`, for levels 60 to 79. |
| `flate_e80_rounds` | 6 | 0 | 16 | As `flate_quick_rounds`, for levels 80 to 99. |
| `flate_e80_exact_rounds` | 3 | 0 | 8 | As `flate_quick_exact_rounds`, for levels 80 to 99. |
| `flate_e80_depth` | 4,096 | 0 | 32,768 | As `flate_quick_depth`, for levels 80 to 99. |
| `flate_e80_segment` | 1,048,576 | 4,096 | 1,048,576 | As `flate_quick_segment`, for levels 80 to 99. |
| `flate_e80_split` | 1 | 0 | 1 | As `flate_quick_split`, for levels 80 to 99. |
| `flate_e80_alternatives` | 1 | 0 | 1 | As `flate_quick_alternatives`, for levels 80 to 99. |
| `flate_e80_second_start` | 0 | 0 | 1,048,576 | As `flate_quick_second_start`, for levels 80 to 99. |
| `flate_e80_nice` | 259 | 3 | 259 | As `flate_quick_nice`, for levels 80 to 99. |
| `flate_e80_polish` | 1 | 0 | 1 | As `flate_quick_polish`, for levels 80 to 99. |
| `flate_e100_rounds` | 8 | 0 | 16 | As `flate_quick_rounds`, for level 100. |
| `flate_e100_exact_rounds` | 4 | 0 | 8 | As `flate_quick_exact_rounds`, for level 100. |
| `flate_e100_depth` | 8,192 | 0 | 32,768 | As `flate_quick_depth`, for level 100. |
| `flate_e100_segment` | 1,048,576 | 4,096 | 1,048,576 | As `flate_quick_segment`, for level 100. |
| `flate_e100_split` | 1 | 0 | 1 | As `flate_quick_split`, for level 100. |
| `flate_e100_alternatives` | 1 | 0 | 1 | As `flate_quick_alternatives`, for level 100. |
| `flate_e100_second_start` | 262,144 | 0 | 1,048,576 | As `flate_quick_second_start`, for level 100. |
| `flate_e100_nice` | 259 | 3 | 259 | As `flate_quick_nice`, for level 100. |
| `flate_e100_polish` | 1 | 0 | 1 | As `flate_quick_polish`, for level 100. |
| `flate_fast_level` | 12 | 1 | 12 | The fast encoder's level at the engine's levels 0 to 19 (effort 0), one of the twelve below; images, and streams that decode to more than 4 MiB, can get faster levels. |
| `flate_level1_mode` | 0 | 0 | 3 | How the level chooses: 0 the longest match at once, 1 looking one position ahead, 2 looking two ahead, 3 the cheapest path. |
| `flate_level1_depth` | 2 | 1 | 400 | How many earlier positions it compares (at most four times the deepest level's default). |
| `flate_level1_nice` | 16 | 3 | 258 | From what length a match is taken at once. |
| `flate_level1_passes` | 0 | 0 | 8 | The parses per block of the cheapest path (0 and 1 are both one parse; at most four times the top level's default). |
| `flate_level1_short` | 0 | 0 | 1 | Whether hash chains also look for 3-byte matches: 1 yes, 0 no. |
| `flate_level2_mode` | 1 | 0 | 3 | As `flate_level1_mode`, for level 2. |
| `flate_level2_depth` | 3 | 1 | 400 | As `flate_level1_depth`, for level 2. |
| `flate_level2_nice` | 16 | 3 | 258 | As `flate_level1_nice`, for level 2. |
| `flate_level2_passes` | 0 | 0 | 8 | As `flate_level1_passes`, for level 2. |
| `flate_level2_short` | 0 | 0 | 1 | As `flate_level1_short`, for level 2. |
| `flate_level3_mode` | 1 | 0 | 3 | As `flate_level1_mode`, for level 3. |
| `flate_level3_depth` | 6 | 1 | 400 | As `flate_level1_depth`, for level 3. |
| `flate_level3_nice` | 16 | 3 | 258 | As `flate_level1_nice`, for level 3. |
| `flate_level3_passes` | 0 | 0 | 8 | As `flate_level1_passes`, for level 3. |
| `flate_level3_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 3. |
| `flate_level4_mode` | 1 | 0 | 3 | As `flate_level1_mode`, for level 4. |
| `flate_level4_depth` | 12 | 1 | 400 | As `flate_level1_depth`, for level 4. |
| `flate_level4_nice` | 32 | 3 | 258 | As `flate_level1_nice`, for level 4. |
| `flate_level4_passes` | 0 | 0 | 8 | As `flate_level1_passes`, for level 4. |
| `flate_level4_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 4. |
| `flate_level5_mode` | 1 | 0 | 3 | As `flate_level1_mode`, for level 5. |
| `flate_level5_depth` | 24 | 1 | 400 | As `flate_level1_depth`, for level 5. |
| `flate_level5_nice` | 48 | 3 | 258 | As `flate_level1_nice`, for level 5. |
| `flate_level5_passes` | 0 | 0 | 8 | As `flate_level1_passes`, for level 5. |
| `flate_level5_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 5. |
| `flate_level6_mode` | 1 | 0 | 3 | As `flate_level1_mode`, for level 6. |
| `flate_level6_depth` | 64 | 1 | 400 | As `flate_level1_depth`, for level 6. |
| `flate_level6_nice` | 96 | 3 | 258 | As `flate_level1_nice`, for level 6. |
| `flate_level6_passes` | 0 | 0 | 8 | As `flate_level1_passes`, for level 6. |
| `flate_level6_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 6. |
| `flate_level7_mode` | 2 | 0 | 3 | As `flate_level1_mode`, for level 7. |
| `flate_level7_depth` | 100 | 1 | 400 | As `flate_level1_depth`, for level 7. |
| `flate_level7_nice` | 258 | 3 | 258 | As `flate_level1_nice`, for level 7. |
| `flate_level7_passes` | 0 | 0 | 8 | As `flate_level1_passes`, for level 7. |
| `flate_level7_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 7. |
| `flate_level8_mode` | 3 | 0 | 3 | As `flate_level1_mode`, for level 8. |
| `flate_level8_depth` | 6 | 1 | 400 | As `flate_level1_depth`, for level 8. |
| `flate_level8_nice` | 24 | 3 | 258 | As `flate_level1_nice`, for level 8. |
| `flate_level8_passes` | 1 | 0 | 8 | As `flate_level1_passes`, for level 8. |
| `flate_level8_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 8. |
| `flate_level9_mode` | 3 | 0 | 3 | As `flate_level1_mode`, for level 9. |
| `flate_level9_depth` | 12 | 1 | 400 | As `flate_level1_depth`, for level 9. |
| `flate_level9_nice` | 32 | 3 | 258 | As `flate_level1_nice`, for level 9. |
| `flate_level9_passes` | 1 | 0 | 8 | As `flate_level1_passes`, for level 9. |
| `flate_level9_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 9. |
| `flate_level10_mode` | 3 | 0 | 3 | As `flate_level1_mode`, for level 10. |
| `flate_level10_depth` | 12 | 1 | 400 | As `flate_level1_depth`, for level 10. |
| `flate_level10_nice` | 32 | 3 | 258 | As `flate_level1_nice`, for level 10. |
| `flate_level10_passes` | 2 | 0 | 8 | As `flate_level1_passes`, for level 10. |
| `flate_level10_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 10. |
| `flate_level11_mode` | 3 | 0 | 3 | As `flate_level1_mode`, for level 11. |
| `flate_level11_depth` | 24 | 1 | 400 | As `flate_level1_depth`, for level 11. |
| `flate_level11_nice` | 64 | 3 | 258 | As `flate_level1_nice`, for level 11. |
| `flate_level11_passes` | 2 | 0 | 8 | As `flate_level1_passes`, for level 11. |
| `flate_level11_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 11. |
| `flate_level12_mode` | 3 | 0 | 3 | As `flate_level1_mode`, for level 12. |
| `flate_level12_depth` | 35 | 1 | 400 | As `flate_level1_depth`, for level 12. |
| `flate_level12_nice` | 100 | 3 | 258 | As `flate_level1_nice`, for level 12. |
| `flate_level12_passes` | 2 | 0 | 8 | As `flate_level1_passes`, for level 12. |
| `flate_level12_short` | 1 | 0 | 1 | As `flate_level1_short`, for level 12. |
| `flate_exact_price_below` | 65,536 | 0 | 1,048,577 | A block below this many bytes is priced with a real code search when parses are compared, a larger one with an estimate (0: always the estimate; 1 MiB and one byte: every block, since a segment is at most 1 MiB). |
| `flate_block_tie` | 0 | 0 | 2 | Both encoders: the block type written when two cost the same. 0: fixed, then dynamic, then stored; 1: dynamic before fixed; 2: stored before both. |
| `flate_polish_passes` | 2 | 0 | 8 | Polishing: its passes (0: none; at most four times the default). |
| `flate_polish_merge_percent` | 2 | 0 | 100 | How much larger the joined estimate of two blocks may be for a merge to be tried, as a percentage of their bits ... |
| `flate_polish_merge_bits` | 400 | 0 | 1,048,576 | ... and at most this many bits. |
| `flate_polish_min_block` | 64 | 0 | 1,048,576 | The smallest block tried for a split, in symbols. |
| `flate_polish_step_div` | 64 | 1 | 256 | The step of the search for a cut: the block's symbols divided by this ... |
| `flate_polish_step_min` | 4 | 1 | 65,536 | ... and at least this many (at least 1, or the search would not move). |
| `flate_polish_min_gain` | 6 | 0 | 1,048,576 | The bits a cut must be estimated to save. |
| `flate_stuck_rounds` | 4 | 1 | 16 | Pricing rounds stop after this many rounds in a row without a gain. |
| `flate_blend_old_div` | 2 | 1 | 1,024 | After a round without a gain the new counts are mixed with the old ones divided by this ... |
| `flate_blend_noise_div` | 8 | 1 | 1,024 | ... and for half the literals and lengths (chosen with a fixed seed) with the new ones divided by this. |
| `flate_exact_tries` | 7 | 1 | 7 | Exact rounds: which of the three tries of a round are made, as bits: 1 the first, 2 the second, 4 the third, which also offers the farther matches. |
| `flate_exact_try1_missing` | 1 | 0 | 16 | The bits the first try charges, over the longest code, for a symbol that has no code. |
| `flate_exact_try2_missing` | 4 | 0 | 16 | The same for the second try. |
| `flate_exact_try3_missing` | 1 | 0 | 16 | The same for the third try. |
| `flate_split_min_symbols` | 64 | 2 | 1,048,576 | Block splitting: the fewest symbols of a segment that is split at all. |
| `flate_split_step_div` | 160 | 1 | 640 | The distance between candidate boundaries: the segment's symbols divided by this ... |
| `flate_split_step_min` | 32 | 1 | 65,536 | ... and at least this many. |
| `flate_split_span` | 96 | 1 | 192 | The most candidates one block may span. |
| `flate_split_stored_extra_bits` | 40 | 0 | 4,096 | The bits an uncompressed block is priced at over 8 a byte. |
| `flate_split_tune_reach` | 1 | 0 | 2 | How far each chosen boundary is fine-tuned, in candidate distances either side (0: not at all). |
| `flate_split_tune_div` | 8 | 1 | 16 | The steps of that tuning: the candidate distance divided by this. |
| `flate_hash_bits` | 16 | 8 | 16 | The match finder's search trees are 2 to this power (at most the 65,536 it always had, which bound its memory). |
| `flate_alt_candidates` | 32 | 1 | 32 | With farther-match alternatives: how many of a position's matches the parse weighs (at most 32, the room the parse has for them). |
| `flate_alt_keep` | 24 | 0 | 24 | How many matches a position may have before no more alternatives are kept (0: none are; at most the 24 it always was, each being three bytes per position). |
| `flate_run_shortcut` | 1 | 0 | 1 | Deep inside a run of one byte the parse takes the longest match without pricing (1) or prices every position (0). |
| `flate_first_literal_bits` | 8 | 1 | 15 | The price, in bits, the first pass over a segment starts from for a literal byte below 144 (the defaults of these five are the fixed code's lengths; a code is at most 15 bits long). |
| `flate_first_high_literal_bits` | 9 | 1 | 15 | The same for a literal byte from 144. |
| `flate_first_length_bits` | 7 | 1 | 15 | The same for a match length up to 114. |
| `flate_first_long_length_bits` | 8 | 1 | 15 | The same for a match length from 115. |
| `flate_first_distance_bits` | 5 | 1 | 15 | The same for a distance. |
| `flate_unused_symbol_bits` | 1 | 0 | 16 | In the pricing rounds a symbol the last parse never used costs the bits of one used once, plus this. |
| `flate_smooth_min_run` | 4 | 1 | 287 | Block codes: count smoothing evens out runs of at least this many symbols (287: never, an alphabet has at most 286). |
| `flate_code_variants` | 63 | 1 | 63 | Which of the six code variants are tried for a block that is written, as bits (1 for the first to 32 for the sixth, tried in that order): plain counts, counts smoothed by a quarter, by a half, codes of at most 12 bits, of at most 10 bits, counts smoothed fully. |
| `flate_price_code_variants` | 3 | 1 | 63 | The same for a block that is only priced. |
| `flate_header_starts` | 255 | 1 | 255 | The search for the cheapest block header of a block that is written: which of its eight starting strategies are used, as bits, tried from the lowest bit up. |
| `flate_header_rounds` | 4 | 1 | 16 | Its rounds from each start (at most four times the default). |
| `flate_price_header_starts` | 192 | 1 | 255 | The same for a block that is only priced, tried from the highest bit down. |
| `flate_price_header_rounds` | 2 | 1 | 16 | Its rounds from each start. |
| `flate_header_unused_bits` | 8 | 1 | 15 | The bits a header symbol costs in that search while the header does not use it (a header code is at most 7 bits long, so the default is one more). |
| `flate_fast_segment` | 1,048,576 | 65,536 | 1,048,576 | The fast encoder cuts the input into segments of this many bytes, each encoded on its own (at most the 1 MiB it always was). |
| `flate_fast_dict` | 32,768 | 0 | 32,768 | Each segment's match finder is primed with this much input before it (more than the 32 KiB window is no use). |
| `flate_fast_batch` | 512 | 64 | 65,536 | Where its blocks end: the symbols gathered before each check (at least 64, so that the check stays rare). |
| `flate_fast_min_block` | 2,000 | 0 | 1,048,576 | The fewest bytes of a block ended on a change of statistics. |
| `flate_fast_max_block` | 300,000 | 1,000 | 300,000 | The most bytes of a block of the levels that do not search for the cheapest path (at most the 300,000 it always was, which bound the symbols held). |
| `flate_fast_end_threshold` | 10 | 0 | 64 | How different the latest symbols must be for a block to end there, in 32nds. |
| `flate_fast_young_block` | 8,192 | 0 | 1,048,576 | The symbols below which a block needs a larger difference. |
| `flate_fast_age_step` | 2,048 | 1 | 1,048,576 | The bytes of a block that each lower the difference needed. |
| `flate_fast_hash4_bits` | 16 | 8 | 16 | Its table of 4-byte hashes has at most 2 to this power entries (at most what it always had). |
| `flate_fast_hash3_bits` | 15 | 8 | 15 | The same for its table of 3-byte hashes. |
| `flate_fast_gain_per_byte` | 4 | 0 | 64 | Lazy matching: the units a later match gains per byte it is longer ... |
| `flate_fast_gain_per_halving` | 1 | 0 | 64 | ... and per halving of the distance. |
| `flate_fast_lazy_gain` | 1 | 0 | 1,024 | The gain over which the match one position ahead replaces the current one. |
| `flate_fast_lazy2_gain` | 4 | 0 | 1,024 | The same for the match two positions ahead. |
| `flate_fast_far3_distance` | 1,024 | 0 | 32,768 | The cheapest-path levels: how far back the first pass still counts a 3-byte match. |
| `flate_fast_max_candidates` | 64 | 1 | 255 | The candidate matches kept per position (their count is kept in a byte). |
| `flate_fast_max_opt_block` | 262,144 | 4,096 | 262,144 | The positions of one block (at most the 262,144 it always was, which bound the memory of a segment). |
| `flate_fast_unused_literal_bits` | 11 | 1 | 30 | The bits charged for a literal the statistics do not have. |
| `flate_fast_unused_length_bits` | 13 | 1 | 30 | The same for a length. |
| `flate_fast_unused_distance_bits` | 10 | 1 | 30 | The same for a distance. |
| `flate_fast_blend_percent` | 100 | 0 | 400 | The weight of the block before in a block's first prices, as a percentage of the block's own. |
| `flate_fast_repeat_distances` | 2 | 0 | 2 | How many of the last distances are tried again at every position. |
| `flate_remember_up_to` | 262,144 | 0 | 1,048,576 | Speed and memory only, never the output: the largest input whose output a store remembers (at most 1 MiB, the bound it always had). |
| `flate_segments_at_once` | 8 | 1 | 8 | Speed and memory only: the segments the strong encoder plans as one group (at most the 8 it always was, which bound the plans held). |
| `flate_fast_group` | 16 | 1 | 16 | Speed and memory only: the segments the fast encoder hands to the threads at once (at most the 16 it always was, which bound the finished output held before it is written). |

What is not a rule: the facts of the format, the seed of the strong encoder's blend (the output must be the
same on every run), the edges of the engine's bands (20, 40, 60, 80, 100), the number of fast levels, the
limits that keep memory bounded, which are the highest values above, and what is code rather than a number.

### `archive_`: the gpress archive

The values the archive works by (`ArchiveRules`, `src/pack/rules.rs`; the field `archive_rules`). Any value in
a rule's range is safe: every archive is restored and compared before it is returned, and memory and time stay
bounded. The first 51, the model's, are shared with `expand`: an archive made with all of them at their
defaults is version 9, and with any of them changed it is version 10, whose header carries the changed values;
`expand` takes them from there, never from its caller.

| Rule | Default | Lowest | Highest | What it decides |
| --- | ---: | ---: | ---: | --- |
| `archive_mixer_rate` | 6 | 1 | 8 | How strongly the mixer changes its weights after each bit. Highest 8: the error times the rate must fit in 16 bits. Lowest 1: at 0 the mixer would never learn. |
| `archive_mixer_shift` | 12 | 4 | 20 | The scale of each weight change (a shift; larger is slower): an input (at most 2047) times the error (at most 4095 times the rate, 8 at most) is below 2^26, shifted right by this, in 1/65536 of a whole weight. Lowest 4: one bit can then move a weight by up to 64 whole; highest 20: by at most 64/65536, where the mixer barely learns. |
| `archive_mixer_start` | 16,384 | 0 | 65,536 | The weight every prediction starts with, in 1/65536: from 0 (the prediction is not used) to 65536 (a whole weight). |
| `archive_mixer_bias` | 256 | 0 | 2,047 | The constant input the mixer can weigh. Lowest 0: none; highest 2047, the largest input a prediction gives. |
| `archive_apm_bits` | 5 | 3 | 5 | The resolution of each final correction map: 2^bits + 1 points (33 at 5). Lowest 3 (9 points), the coarsest map the code builds (it shifts the map's input right by at most 2 bits); highest 5, what a map has room for (more would cost every archive memory and time). |
| `archive_apm_round` | 1 | 1 | 4 | From what part of a step a map's point moves, in quarters of a step. Lowest 1 and highest 4 (a whole step): only within them does a point never pass the end of the scale. |
| `archive_apm1_rate` | 7 | 3 | 16 | How fast the first final correction (by kind and partial byte) learns: steps of 1/2^(rate - 1). Lowest 3, a step of 1/4, the largest that `archive_apm_round` (quarters of a step) works with; highest 16, a step of 1/32768, the smallest that still moves a 16-bit point. |
| `archive_apm2_rate` | 7 | 3 | 16 | The same for the second final correction (by the last byte and partial byte), with the same range. |
| `archive_blend_apm1` | 4 | 0 | 8 | The share of the first correction in the final probability, in sixteenths. Lowest 0: not used; with `archive_blend_apm2` at most the whole (8 each); the mixer's own result gets the rest. |
| `archive_blend_apm2` | 8 | 0 | 8 | The share of the second correction in the final probability, in sixteenths, as `archive_blend_apm1`. |
| `archive_state_count` | 7 | 1 | 7 | The highest count a context's bit history keeps. Lowest 1: at 0 no bit is counted, and a 0 bit sends a context back to state 0, "nothing seen"; highest 7, what a 3-bit count holds (a state holds two of them). |
| `archive_state_discount` | 2 | 0 | 7 | The opposite count is halved when it is above this: 0 (always) to 7 (never). |
| `archive_state_limit` | 1,023 | 0 | 1,023 | How long the per-state probabilities keep slowing down. Lowest 0: every update moves half the way; highest 1023, the most the 10-bit count holds. |
| `archive_match_min` | 5 | 1 | 8 | The shortest repeat the long-match model follows, in bytes. 1 to 8: the model keeps the last 8 bytes. |
| `archive_match_check` | 64 | 1 | 256 | How far back a new match is checked, in bytes. Lowest 1, the byte before; highest 256, so that the work per byte stays bounded. |
| `archive_match_bits` | 20 | 8 | 22 | The match table has 2^bits positions of 4 bytes. Lowest 8: 256 positions, as many as one byte has values; highest 22 (16 MiB), the size of the fax model's largest tables. |
| `archive_match_cap` | 63 | 0 | 63 | The match length up to which every length has its own probability. Lowest 0: one for all lengths; highest 63: the table has 128. |
| `archive_match_long` | 24 | 1 | 2,147,483,648 | From this match length the mixer uses its weights for long matches. Lowest 1: every match is long; highest 2^31 (2 GiB, the largest part): as good as none is. |
| `archive_match_limit` | 1,023 | 0 | 1,023 | The count cap of the match probabilities. Lowest 0: every update moves half the way; highest 1023, the most the 10-bit count holds. |
| `archive_repeat_min` | 128 | 1 | 2,147,483,648 | A match at least this long codes the next byte with one decision instead of eight. Lowest 1: only a running match has a byte to offer. At the highest it never happens within a part. |
| `archive_repeat_limit` | 1,023 | 0 | 1,023 | The count cap of the long-repeat decision, with the range of `archive_match_limit`. |
| `archive_expect_cap` | 15 | 0 | 15 | The number of expected bytes that came true up to which every count has its own probability. Lowest 0: one for all counts; highest 15: the table has 128. |
| `archive_expect_limit` | 1,023 | 0 | 1,023 | The count cap of the expected-text probabilities, with the range of `archive_match_limit`. |
| `archive_table_bits_min` | 14 | 8 | 20 | The model's ten tables have at least 2^bits buckets of 16 bytes each (2.5 MiB in all at 14). Lowest 8: 256 buckets, as many as one byte has values; highest 20, as `archive_table_bits_max`. |
| `archive_table_bits_max` | 20 | 8 | 20 | ... and at most 2^bits buckets (160 MiB in all at 20, the highest: the bound it always had). Lowest 8, as `archive_table_bits_min`. When the least is above the most, the least counts. |
| `archive_table_shift` | 3 | 0 | 16 | Between the two, a table grows until it has the file's length / 2^shift buckets. Lowest 0: a bucket for every byte; highest 16: one for every 64 KiB, so that from the default least (14) the tables grow only for files over 1 GiB, the default `max_len`. |
| `archive_prime_rounds` | 2 | 0 | 8 | How often the two built-in texts run through the model before a file's first byte; 0: not at all. Highest 8: each round costs about 2 ms a model (expanding a 4 KB file at effort 100: 30 ms with none, 34 ms with 2, 45 ms with 8). |
| `archive_hash_bytes` | 16 | 16 | 32 | Bytes of the original's SHA-256 an archive stores and checks. Lowest 16, what it always was (fewer would weaken "never a wrong file"); highest 32, all of it. |
| `archive_font_window` | 4,096 | 0 | 65,536 | How far before a stream stored without a filter the mark of a Type 1 font is looked for, in bytes; 0: such a font is coded as raw data. Highest 65536, as far back as a stream's dictionary is looked for. |
| `archive_fax_bits_min` | 12 | 8 | 22 | The two hashed tables of the fax model have at least 2^bits counters of 4 bytes. Lowest 8, as the general model's tables; highest 22, as `archive_fax_bits_max`. |
| `archive_fax_bits_max` | 22 | 8 | 22 | ... and at most 2^bits (16 MiB each at 22, the highest: the bound it always had). Lowest 8, as `archive_fax_bits_min`. When the least is above the most, the least counts. |
| `archive_fax_bits_add` | 4 | 0 | 16 | Between the two, 2^bits is the smallest power of two above the file's length, times 2^this. Lowest 0: that power alone; highest 16: then a file of 64 bytes, the least a fax stream is coded from by default (`archive_fax_min_bytes`), already gets 2^23, more than the most. |
| `archive_fax_small_limit` | 255 | 0 | 1,023 | The count cap of the small counters for the original encoder's layout choices, with the range of `archive_match_limit`. |
| `archive_fax_mixer_shift` | 15 | 4 | 24 | The learning rate of the fax model's two mixers (a shift; larger is slower): a stretched input (at most 2840) times the error (at most 65535) is below 2^28, shifted right by this, in 1/65536 of a whole weight. Lowest 4: one pixel can then move a weight by up to 177.5 whole (weights stay within 256 either way); highest 24: by at most 11/65536. |
| `archive_fax_mixer_start` | 19,660 | 0 | 65,536 | The weight every prediction of the fax model starts with, in 1/65536, with the range of `archive_mixer_start`. |
| `archive_fax_apm_bits` | 5 | 3 | 6 | The resolution of the fax model's two correction maps: 2^bits + 1 points. Lowest 3 (9 points), as `archive_apm_bits`; highest 6 (65 points: 8.125 MiB for the larger map, 1.016 MiB for the smaller). |
| `archive_fax_apm_shift` | 6 | 1 | 15 | The learning rate of those maps (a shift). Lowest 1: a point moves half the way; highest 15: 1/32768 of it, the smallest step that still moves a 16-bit point. |
| `archive_fax_limit` | 1,023 | 0 | 1,023 | The count cap of the fax model's direct tables, its white-area counters and its same-row counters, with the range of `archive_match_limit`. |
| `archive_fax_hashed_limit` | 1,023 | 0 | 1,023 | The count cap of the fax model's hashed tables, with the range of `archive_match_limit`. |
| `archive_fax_blend_apm1` | 4 | 0 | 8 | The share of the first correction map in a pixel's final probability, in sixteenths (as `archive_blend_apm1`). |
| `archive_fax_blend_apm2` | 8 | 0 | 8 | The share of the second correction map, in sixteenths (as `archive_blend_apm1`). |
| `archive_image_step_1` | 0 | 0 | 255 | Differences between the pixels of a photo-like image are told apart in 8 classes: a difference's class is the number of these seven limits it is above (0, 1, 2, 4, 8, 16, 32). Each from 0 to 255, the sizes a difference between two bytes can have; no order is asked for. |
| `archive_image_step_2` | 1 | 0 | 255 | As `archive_image_step_1`. |
| `archive_image_step_3` | 2 | 0 | 255 | As `archive_image_step_1`. |
| `archive_image_step_4` | 4 | 0 | 255 | As `archive_image_step_1`. |
| `archive_image_step_5` | 8 | 0 | 255 | As `archive_image_step_1`. |
| `archive_image_step_6` | 16 | 0 | 255 | As `archive_image_step_1`. |
| `archive_image_step_7` | 32 | 0 | 255 | As `archive_image_step_1`. |
| `archive_word_bytes` | 0 | 0 | 2 | What counts as a word: 0 letters and digits with the letters lower-cased, 1 letters and digits as they are, 2 letters only, lower-cased. |
| `archive_expect_text` | 1 | 0 | 1 | The expected-text model (the next object header, cross-reference rows, the operand at the same place as in the last group): 1 on, 0 off. |
| `archive_fax_white_shortcut` | 1 | 0 | 1 | A pixel whose whole neighbourhood is white is coded with one small counter instead of the full fax model: 1 on, 0 off. Off, on the 17 files of the test set with a fax image (effort 100, one thread): from 0.37% smaller to 27.2% larger (1.0% larger in all), and compress and expand up to 3.7 times as slow (1.7 times in all). The most time a file can take stays what it was, as a page of noise goes through the full model at every pixel either way. |

The rest are choices of the packing side only: the archive stores their outcome, so they never change its
version, and of them `expand` reads `archive_deadline_step` only.

| Rule | Default | Lowest | Highest | What it decides |
| --- | ---: | ---: | ---: | --- |
| `archive_plain_below` | 50 | 0 | 101 | Below this level of the engine (`PackOptions::effort`: efforts 0 and 20 are its levels 0 and 20, 40 and above its 100), data that looks compressed is coded plainly (faster, about 1% larger). 0: never; 101: at every level. |
| `archive_plain_entropy` | 790 | 0 | 800 | How random bytes must look to be coded plainly, in hundredths of a bit per byte. Lowest 0: not at all; highest 800, the most a byte holds. |
| `archive_plain_min_bytes` | 1,024 | 0 | 2,147,483,648 | Data smaller than this is never coded plainly. Up to the largest part an archive holds (2 GiB). |
| `archive_plain_windows` | 16 | 2 | 65,536 | Larger data is judged on this many windows spread over it. Lowest 2: the first and the last; highest 65536: at the default window of 64 KiB that is 4 GiB, more than a part holds (2 GiB). |
| `archive_plain_window_bytes` | 65,536 | 1 | 2,147,483,648 | The length of each window in bytes: from 1 to 2^31, the largest part. |
| `archive_plain_distances` | 4 | 0 | 8 | The differences between bytes 1 to this many apart must look random too. Lowest 0: the bytes alone decide; highest 8: the test keeps a count table for each of 8 distances (each adds a count per byte). |
| `archive_lepton_min_saving` | 10 | 0 | 100 | A JPEG inside a Flate stream goes to Lepton when Lepton's data is this many percent smaller than the Flate bytes: from 0 (smaller at all) to 100 (never). Only in builds with Lepton, as the next four. |
| `archive_lepton_min_bytes` | 1,024 | 0 | 2,147,483,648 | A JPEG smaller than this is not tried with Lepton: from 0 (every one) to 2^31, the largest part (none). |
| `archive_lepton_min_gain` | 1 | 0 | 2,147,483,648 | Lepton's data is kept when it is at least this many bytes smaller than the JPEG. Lowest 0: it is never larger than the JPEG it restores; highest 2^31, the largest part (it is never kept). |
| `archive_lepton_plain` | 1 | 0 | 1 | Lepton's data that looks compressed is coded plainly: 1 yes, 0 never. |
| `archive_lepton_partitions` | 8 | 1 | 8 | Lepton codes a large JPEG in up to this many pieces (fewer: a little smaller). Lowest 1, one piece; highest 8, the most the Lepton readers before this one take. |
| `archive_decrypt_recheck` | 98 | 0 | 100 | A second pack without decryption is made when the pack is above this many percent of the encrypted bytes, and the smaller kept. 0: always; 100: only when the pack is larger than those bytes. |
| `archive_decrypt_tie` | 0 | 0 | 1 | When the two packs are equally long: 0 keeps the one without decryption, 1 the one with. |
| `archive_min_saving` | 0 | 0 | 2,147,483,648 | A coded archive is kept only when it is at least this many bytes smaller than the file stored as it is. Lowest 0: when it is not larger; highest 2^31: only a file that shrinks by 2 GiB is coded. |
| `archive_fax_min_bytes` | 64 | 0 | 2,147,483,648 | A fax stream smaller than this is stored as it is, not as an image: from 0 to 2^31, the largest part (no fax stream is then coded as an image). |
| `archive_fax_max_leftover` | 25 | 0 | 100 | A fax stream is stored as an image when at most this many percent of it is left undecoded: from 0 (only one decoded to its end) to 100 (any). |
| `archive_content_sample` | 4,096 | 1 | 16,777,216 | A stream with no type is taken for page content when, of its first this many bytes (from 1 to 2^24: at most 16 MiB of each such stream is read for it) ... |
| `archive_content_printable` | 90 | 0 | 100 | ... at least this many percent are printable. |
| `archive_font_min_text` | 32 | 0 | 4,096 | How many decrypted bytes must be text before a font's encrypted part counts as found. A wrong guess costs nothing but time: a font is opened only if it closes back to the same bytes. Highest 4096: each try then decrypts at most 4 KiB and 4 bytes. |
| `archive_font_text_bytes` | 0 | 0 | 1 | The bytes that count as text there: 0 tab, line ends and 32 to 126; 1 also 160 to 255 (Latin-1). |
| `archive_font_tries` | 4 | 0 | 64 | At how many places the word "eexec" appears the encrypted part of a font is looked for. Highest 64, so that the search stays short. |
| `archive_zlib_levels` | 511 | 0 | 511 | The zlib levels tried to rebuild a stream byte for byte, as bits (1 is level 1, 256 is level 9). 0: no stream is rebuilt. |
| `archive_zlib_mem_levels` | 511 | 0 | 511 | The zlib memory levels tried, as bits (1 is memory level 1, 256 is 9). |
| `archive_zlib_filtered` | 1 | 0 | 1 | zlib's "filtered" strategy is tried too: 1 yes, 0 no. |
| `archive_deadline_step` | 65,536 | 1 | 2,147,483,648 | Bytes coded between two looks at the time budget, in `compress` and `expand` (a look comes at the next 4 KiB of a part). Never changes the output. Lowest 1: a look every 4 KiB; highest 2^31 (2 GiB, the largest part): in effect a look only where a run of coded bytes starts. |

## Building only what a product needs

The parts of the engine are Cargo features, all on by default. A product that needs fewer builds with
`--no-default-features --features ...`, and the code of a part it leaves out is not compiled. The C library
and the WebAssembly crate have the same features and pass them on to the engine.

| Feature | The part | Without it |
| --- | --- | --- |
| `archive` | The gpress archive: `compress` and `expand`, the context-mixing model, the exact re-creation of zlib streams. | `compress` and `expand` return `Error::NeedsFeature(Feature::Archive)`. `optimize` is not touched. |
| `jpeg` | Normal PDF: optimal Huffman tables for JPEG images, and the JPEG decoder and encoder of the lossy image step. | The tables step is skipped: the output is the one `jpeg_tables` off gives. The lossy step leaves photos and JPEG images alone and resamples line art only. |
| `fax` | The CCITT fax and JBIG2 codecs: fax images become JBIG2 in a normal PDF and are stored as pixels in the archive. | The JBIG2 step is skipped (the output of `jbig2` off). The archive stores fax streams as they are, and `expand` of an archive with fax parts returns `NeedsFeature(Fax)`. |
| `encryption` | Encrypted PDFs the empty password opens, with RustCrypto's AES, MD5 and SHA-2 (with the small crates they use, the only outside code in a default build). | An encrypted PDF is left untouched, with a reason that names the feature. The archive stores its streams as they are, and `expand` of an archive with decrypted streams returns `NeedsFeature(Encryption)`. |
| `existing` | Lepton for JPEG streams in the archive. Off by default; it needs `archive`. | See "Existing library" below (`Error::NeedsExistingLibrary`). |

`web` is the web compressor's set: `jpeg`, `fax` and `encryption`, without the archive. Its `optimize` gives
the same bytes as a build with every part.

No call panics for a part a build leaves out. `Error::NeedsFeature` has code 10 (`GPRESS_ERR_NEEDS_FEATURE`
in C) and a message that names the feature, and `gpress::Feature::enabled` says what a build has. Without
`encryption` an encrypted PDF is left untouched instead of refused, because that is how `optimize` already
answers for a file it can't rewrite (a password it doesn't have, a handler it doesn't know): the caller gets
the input back with the reason, and a batch goes on. The command-line tool prints the same message and exits
with 10 (`pack` and `unpack` without `archive`, or a measuring command of a part left out). In a build without
`fax` or `encryption`, a damaged archive whose damage reads as a part of that kind gives `NeedsFeature` too:
only a build with the part can tell the two apart, as with Lepton's parts and `NeedsExistingLibrary`. Neither
ever gives a wrong file.

The npm package has two modules: `dist/gpress.wasm` with every part (482,899 bytes of gzip), and
`dist/gpress-web.wasm`, the `web` set (359,089 bytes of gzip, 26% less). Both have the same wrapper; in the web
module `compress` and `expand` throw a `GpressFeatureError` whose `feature` is `"archive"`, and `g.features`
lists the parts a module has.

```
cargo build --release --no-default-features --features web     # the tool without the archive
cd crates/gpress-wasm/js && node build-wasm.mjs --no-default-features --features web --out dist/gpress-web.wasm
```

What each part costs in bytes, and what each saves on the sample: bench/RESULTS.md, "What each feature costs".

## What is ours

Parts that follow another project's code are listed in [Code from other projects](#code-from-other-projects).

| Part | File | What it does |
| --- | --- | --- |
| The public API | `src/api.rs`, `src/clock.rs` | `Options`, `optimize`, `compress`, `expand`, `Error` with stable codes, the panic guard, the deadline behind `time_budget`. |
| Flate decoder | `src/flate/inflate.rs`, `zlib.rs` | Accepts exactly what zlib accepts; anything damaged is an error, never a silent repair. |
| Flate encoder | `src/flate/deflate.rs`, `matcher.rs`, `parse.rs`, `block.rs`, `huffman.rs` | Binary-tree match finder (smallest distance for every length), shortest-path parsing priced first by symbol statistics and then by the exact code lengths, farther matches with cheaper distance codes, block boundaries by dynamic programming plus merge/split polishing, optimised block headers. The parse prices match lengths without branches, so it runs on vector instructions (AVX2 when the processor has it, chosen at run time; the same result either way). Smaller output than Zopfli, in less time. |
| Fast Flate encoder | `src/flate/fast.rs` | Engine levels 0 to 19 (effort 0). Twelve levels: hash chains (4-byte hashes, from level 3 also 3-byte ones) with greedy, one-step or two-step lazy matching for levels 1 to 7; binary trees and a cheapest-path parse under costs from a first pass (two passes from level 10) for levels 8 to 12. Blocks end where the symbol statistics change, counted as the symbols are made (no search); each block gets the shared Huffman code builder, a one-pass header and the cheapest of dynamic, fixed and stored. Inputs are cut at fixed 1 MiB offsets, each segment primed with the 32 KiB before it, worked on in parallel and joined bit for bit, so the output is the same for any thread count. On the sample's 3373 Flate streams (152.5 MB, one core), against zlib -9: level 1 +6.5% at 140 MB/s, level 7 -1.2% at 58 MB/s, level 12 -4.2% at 9 MB/s (0.9% above the strong encoder's quick settings, which run at 1.4 MB/s). |
| zlib rebuild | `src/flate/zlibexact.rs` | zlib's deflate re-created bit for bit, to rebuild streams zlib wrote; it follows zlib's own code. Streams other encoders wrote are stored as they are. |
| PDF reader | `src/pdf/parse.rs`, `xref.rs`, `filters.rs` | Strict parser: objects, xref tables and streams, object streams, LZW, ASCIIHex, ASCII85, RunLength, predictors. |
| PDF writer | `src/pdf/rewrite.rs`, `images.rs` | Keeps every reachable object's value (but for the images the lossy step changes at efforts 60 and above); re-encodes streams with our Flate; picks a better PNG predictor (or none) for images, counting the bytes its parameters take; gives JPEGs optimal Huffman tables; puts Flate in front of a JPEG only when that saves at least 1% (never in front of JPEG 2000 or JBIG2); packs objects into compressed object streams grouped by type; writes a compressed xref stream; reads its own output back and compares before returning it (a changed JPEG must read back with the same coded symbols, an image written as JBIG2 with the same pixels, an image written as Indexed with the same colour for every pixel and no other change to its dictionary). An image with fewer samples than its size needs is kept exactly as it is (readers differ on such images). |
| Safety gate | `src/pdf/rewrite.rs` (`gate`) | Encrypted files the empty password doesn't open, signed, certified, PDF/A, PDF/X, files with earlier revisions (incremental updates, as found below) whose earlier saves can't be dropped safely (next row), files we can't read strictly (including a literal string with a raw carriage return, which qpdf reads as a line feed and pdf.js, PDFium and Poppler keep), objects beyond /Size, objects present but not indexed (the trailer or a reached object refers to a number that the cross-reference data doesn't have in use, and a header of that number is in the file's bytes, whatever its generation, with any white space and comments between its parts: Poppler rebuilds a damaged file's cross-reference data from headers, and takes one with two spaces, with its generation spelt `00`, or with a NUL right after the number and a comment before the line end), no catalog (the trailer's /Root is neither a dictionary nor a reference to one that the cross-reference data has), a trailer value nested 16 arrays or dictionaries deep, the innermost counted even when empty (from 64 dictionaries deep, PDFium opens the file and not what would be written from it), object numbers far apart (beyond the cross-reference rows a file of that size may have, 16 per byte plus 65536, or at or above a third of the length of the file that would be written: qpdf 12 reads an object only when its number is below that), object streams past the bounds `max_stream` sets, or with an object read whose header entry starts where an entry listed after it starts too (among the entries up to the first one past the last the cross-reference data points at), or an object that runs into the next one's bytes (each object of an object stream is read only up to where the next entry the cross-reference data reaches starts, which some readers do and others don't): returned untouched. A file has earlier revisions when it has more cross-reference sections than one save writes, a `startxref` before its first section or more of them than sections (a save appended without a /Prev link; those bytes count wherever they are in the file, so inside a stream, a string or a comment too), or an object that two sections, or one section twice, define differently. One save writes one section, or two: those of a linearised file (/Linearized within its first 1024 bytes, the newer section before the older, and the older trailer saying nothing about the document that the newer doesn't), or those of a hybrid file whose newest section is an empty table that names an /XRefStm (what it names lying before the older section, and the two trailers saying the same about the document). Nothing a file claims (an object stream's /N, the trailer's /Size, an image's /Width, predictor parameters) sizes memory or work on its own; every count is checked against the data it describes, or the file's size, first. |
| Earlier revisions | `src/pdf/rewrite/history.rs`, `src/pdf/xref.rs` (`saves`) | A file that keeps earlier saves (incremental updates) is written as its newest save alone, as a file of one save: the earlier saves are gone for good. Where each save ends is found first: every `startxref` has its `%%EOF` before the next one, and the newest save's /Prev chain links each save's cross-reference section, in file order, one section a save (two for a linearised first save); a save the newest doesn't link (a whole file appended after another) is an earlier save too. Each save is then read on its own, the file cut at its end, and must open, load and pass the gate as a file of one save. Left untouched, with the reason: a file where any save is signed, certified, PDF/A or PDF/X or has usage rights, whatever `respect_conformance` says (the bytes are searched for /ByteRange, /DocMDP, /FieldMDP, /UR3, GTS_PDFX, GTS_PDFA and `pdfaid:` too, so an object no save reaches counts); an /XRefStm in any save (hybrid files); a reference to another generation of an object (a stream's /Length counts, and so does the object stream a row names), or to a number the save doesn't have in use (readers differ on what it names, and Poppler may then look through the bytes and take the last object of every number); a row that doesn't point at the header of its own object (pypdf then moves rows to other numbers or looks through the bytes); a /Root that names a dictionary other than a catalog (PDFium opens no such file, and pypdf shows an earlier save's catalog); a /Prev of 0, where qpdf and PDFium end the chain; a `startxref` or /Prev that names white space or a comment before its section (pypdf may give the chain up there and look through the bytes); a save whose trailer lacks a key that the save before it has (some readers fill it in from the older trailer), or whose trailer nests arrays and dictionaries 16 levels deep; a cross-reference row with a lower generation than an older row of its number (PDFium keeps the higher), with a generation beyond 16 bits or of an unknown type; a row in use for object 0 (qpdf 11 reads the object it points at); a free row of generation 65535 for object 1 (Poppler takes a table that starts with one to start at object 0); a linearisation dictionary whose /L is the whole file's length or a real number (with the whole length, Poppler and PDFium's progressive loader then start from the first-page section and never see a later save); more than 64 saves or 1001 sections; saves whose lengths add up to more than 16 times the file, or that take more cross-reference rows or object-stream bytes in all than one read of the file may. Each dropped save is listed, oldest first, with its bytes in the input and how many objects it reaches with a value the output doesn't hold (`Report::history_dropped`; the command-line tool prints one line for each on standard error). |
| JPEG tables | `src/jpeg/` | Baseline and extended sequential JPEG (8-bit) written again with optimal Huffman tables, the same coefficients (like `jpegtran -optimize -copy all`): every scan is read to its coded symbols, which are written back with the best code of at most 16 bits that leaves the all-ones code word free (package-merge); of three orders of the symbols within a code length, the one with the fewest stuffed 0xFF bytes is kept. Every other byte stays (ICC and Adobe markers, comments, restart markers, bytes after the end); one table segment goes before each scan. The new file is read back and compared symbol by symbol before it is used. Progressive, arithmetic-coded, lossless, hierarchical, 12-bit, fractionally sampled and anything unusual or damaged is left as it is, and so are layouts readers could take differently once the tables move (fill bytes before a marker; every segment's contents must fill it exactly), data bound to the file's bytes (multi-picture offsets, APP11 boxes such as content credentials), and files of more than 4096 segments. |
| Lossy image step | `src/pdf/lossy.rs`, `place.rs`, `src/jpeg/decode.rs`, `encode.rs` | Efforts 60, 80 and 100 (see [Effort levels](#effort-levels)): the page walk finds the largest size each image is drawn at; an 8-bit grey or RGB image shown at 5/4 of its floor or more is resampled down to the floor by an area average, and a photo is written as a baseline JPEG (4:2:0 for colour) by our own decoder and encoder, with a transform in integers so that every machine writes the same bytes. A result is kept only when it is clearly smaller and, for a JPEG, decodes close to its samples. |
| CCITT fax codec | `src/fax/ccitt.rs`, `tables.rs` | Decoder and encoder for T.4 (1-D, 2-D) and T.6 as CCITTFaxDecode uses them. The decoder records every choice of the original encoder (fill bits, EOLs, tag bits, end codes, padding, bytes after the end, and Adobe's way of choosing between pass and horizontal mode) and keeps a row only if writing it again gives the same bits. Checked against libtiff both ways and against libtiff and Poppler on all 183 fax streams of the test files. |
| JBIG2 | `src/fax/jbig2.rs` | MQ coder; generic-region encoder and decoder (T.88, template 0, nominal adaptive pixels, typical prediction); the embedded stream PDF reads. jbig2dec decodes our output exactly. |
| Fax images, normal PDF | `src/fax/pdf.rs` | A CCITT image becomes JBIG2 when that is at least 5% smaller and nothing is uncertain: one CCITT filter with direct parameters, a DeviceGray image or a mask, the width and height the data holds, at most 2^30 pixels (width rounded up to 32; PDFium's JBIG2 decoder, which pdf.js also uses, shows nothing above 2^31), no byte alignment, EOLs where every reader expects them, every row written back bit for bit, end codes or 0 bits at the end, coding work of at most 2048 pixels per byte of data, and our JBIG2 decoder giving the same pixels. JBIG2Decode's output is the same bits as CCITTFaxDecode's (BlackIs1 handled), so Decode, ImageMask and masks keep their meaning. A 1-bit image stored without an image codec (general filters only, or none) is held to the same rules where they apply: a DeviceGray image or a stencil mask, direct width and height that the data fills exactly (rows padded to a byte; the padding bits are not pixels and are not compared), the same size and work bounds, at most 65,536 changing elements and 8 more for each stored byte (general filters can store millions in a few bytes; fax data spends a bit or more on each), an encoding every reader reads our way (see Indexed images), JBIG2 at least 5% smaller than the best other encoding of the stream and smaller with its dictionary counted, and our decoder giving the same pixels. |
| Indexed images | `src/pdf/indexed.rs` | An 8-bit image becomes an Indexed image over its own colour space when nothing is uncertain: DeviceGray or DeviceRGB, named in the image itself, in a file that names no default colour space (DefaultGray, DefaultRGB) and is not encrypted (the table would be sealed after the sizes are compared); general filters only, with an encoding every reader reads our way: no reference in Filter or DecodeParms, no DP (Poppler 24.02 reads it as DecodeParms, PDFium 7999 does not), and a predictor only as a number with the image's own Colors, BitsPerComponent and Columns, a TIFF one at 8 bits only (PDFium 7999 undoes a TIFF predictor with the image's components, whatever its parameters say); direct width and height that the samples fill exactly; no Decode, colour-key Mask, ImageMask, Interpolate or Alternates; not the SMask or Mask of another object, named directly or through an object that is only a reference; no soft mask with a Matte colour; and at most 256 colours of DeviceRGB or 16 of DeviceGray (an index must take fewer bits than the samples it stands for). CalGray, CalRGB and ICCBased images are left alone: readers take an image's samples and a table's colours through such a colour space by routes that can round differently (Poppler 24.02 over ICCBased, PDFium 7999 over CalRGB). Indexes take 8, 4 or 2 bits for at most 256, 16 or 4 colours, rows padded to a byte (never 1 bit: PDFium 7999 draws a 1-bit image that it scales another way than the 8-bit image it was made of); the table holds the colours in ascending order of their bytes. The Indexed image gets the Flate and predictor search of any other image and is kept only when the stream, the table in its dictionary counted, is smaller than the best other encoding. A grey image stored as RGB stays RGB. |
| Encrypted PDFs | `src/pdf/crypt.rs`, `src/pack/sealed.rs` | The standard security handler with the empty user password (see below). RC4 and the key derivations are ours; AES, MD5 and SHA-2 come from RustCrypto. |
| gpress archive | `src/pack/`, `src/cm/` | Any PDF, packed with our context-mixing model (bit-history states, mixers, SSE) with PDF-aware contexts: syntax, predicted object numbers and xref offsets, drawing instructions, image samples coded as residuals. Streams are recognised by their bytes (zlib, JPEG), not only by their dictionaries. Format version 9: the model's logistic tables are worked out from their formulas, and its secondary estimation moves both knots around the input at each bit; a long repeat is coded as one 16-bit-precision decision instead of eight model decisions (huge flat images, duplicates); palette and packed-pixel images are modelled by their exact neighbours; an image's row length is confirmed from its data length; a JPEG behind Flate goes through Lepton when that beats the Flate bytes. Restored byte for byte; every archive is restored and checked before it is returned (with a second thread, the restore decodes the bytes while the encoder writes them). A damaged archive is refused with an error, never restored wrong: the header declares the restored length and the decoded bytes the parts claim in all (`max_len` and `max_decoded` refuse a file declaring more than the caller can afford, before anything is decoded); what a part restores is bounded by what is left of the declared length and what it claims by what is left of the declared decoded total, so the decoder's work and memory follow the two numbers the caller allowed, not what a damaged body says; decoding stops when the data runs out; and the file's checksum is checked at the end. The part kinds and how each is coded are constants of the format version; the model's numbers are the shared archive rules (`archive_rules`). An archive made with every one of them at its default is version 9 (and with every archive rule at its default, byte for byte as before); with any of them changed it is version 10, whose header carries the changed values (the profile: their count in one byte, then each as its row in one byte and its value as a base-128 number, rows rising) between the decoded total and the checksum, which is then `hash_bytes` long (16 to 32 bytes of the SHA-256). Expand checks every profile value against its rule's range before anything is sized by it, and refuses a profile that is cut, repeats or reorders a row, names no shared rule, or gives a default. Nothing earlier shipped, so versions before 9 are refused. The fallback form (version 0: the whole file coded by the model alone, written only when the archive fails its own restore check) uses the current model too, so a fallback file from a build before version 9 is refused as damaged. |
| Type 1 fonts | `src/pack/type1.rs`, `src/cm/model.rs` | Type 1 font programs (FontFile with /Length1 /Length2 /Length3, typical of TeX) lose both layers of encryption before coding: the eexec layer (binary, PFB, and hex when its lines are regular) and the charstring layer inside it (lenIV honoured). Both ciphers are reversible byte for byte and are put back on restore; a font that would not come back exactly is coded as it is. On restore, a layout that doesn't fit inside the font's length is refused before anything is decoded for it. The charstrings get their own model kind, which follows their tokens. |
| Fax images, archive | `src/fax/bilevel.rs`, `part.rs` | A CCITT stream is stored as its image, coded by a bilevel context-mixing model (templates up to four rows high, vertical runs, two mixers, two probability maps, 16-bit precision, typical prediction, a fast path for white areas), plus the encoder's choices; on restore our CCITT encoder writes the original bytes back. Streams under 64 bytes stay plain. The model's work is bounded by the file's size (2^24 plus 512 pixels per byte up to any point of the file); streams past it are coded plainly, and a restore that would go past it is refused. |
| SHA-256 | `src/pack/sha256.rs` | For the restore check. |
| Threads and effort | `src/par.rs`, `flate::Options::effort`, `pdf::rewrite::Options`, `pack::PackOptions` | Standard-library threads only: the segments of a stream, the streams of a PDF and the streams the archive looks at are worked on in parallel and put together in their original order, so the output is the same for any thread count (one thread uses no thread machinery at all, as in a browser). Identical streams are encoded once, and so is an input of at most 256 KiB that streams under other dictionaries share: encoded outputs are remembered for the whole file within `store_budget` (a remembered result is checked against its input before it is used), so memory stays bounded. With the strong encoder, in each group of eight segments of a stream, a segment like an earlier one (the same length, history length and bytes) is written from that one's plans, not planned again. Engine levels as in the `engine_effort` row above (the JPEG tables and the JBIG2 conversion run at every level). |

## Code from other projects

These parts follow another project's code closely enough to be altered versions of it. Each file says so and
keeps that project's licence notice, and every one of these licences allows it.

| File | The part | Follows | Licence |
| --- | --- | --- | --- |
| `src/flate/zlibexact.rs` | All of it | zlib's deflate.c and trees.c (Jean-loup Gailly, Mark Adler) | Zlib |
| `src/flate/inflate.rs` | The checks and the sort of a code's lengths; the bit-by-bit decode | zlib's inftrees.c and puff.c (Mark Adler) | Zlib |
| `src/flate/fast.rs` | The statistics that end a block, the hash-chain search, the lazy-match gain, the symbol costs, the cheapest-path parse, the statistics pass, the binary tree | libdeflate (Eric Biggers, Google) | MIT |
| `src/flate/matcher.rs`, `src/flate/fast.rs` | The binary tree's insertion and search | LzFind.c of the LZMA SDK (Igor Pavlov) | Public domain |

`src/fax/bilevel.rs` is not in the table, but its probability map has a history: it was adapted from the
earlier map in `src/cm/`, which format version 9 replaced because it followed PAQ (GPL). The call on it is
made on what its code says, not on where it came from. Its update moves the nearer of the two knots around the
input toward 0 or 65535, by a fixed shift. That comes from the earlier map, and no PAQ program does it: lpaq1
also moves the nearer knot, but at a rate set by a count, and paq8 moves both knots toward a target that
depends on the rate. (The commit that took the earlier map out counted its update as following PAQ; this is
the narrower call.) What bilevel.rs keeps of PAQ's map is design, not code: 33 knots per context that start as
squash, linear interpolation between them, the nearer knot learning, and a step by a shift. None of PAQ's
other constants or table values is in it, and its interpolation is its own. Its squash is worked out from the
logistic formula, and its stretch is the inverse of that squash, as PAQ's is, found by a loop of its own.
libzpaq (public domain) has a close relative, the nearer knot included. The new map in `src/cm/` keeps the
start as squash, the linear interpolation and a step by a shift, which its contract sets, and 33 knots 128
apart, which its implementer chose within the contract's 66 bytes per context after reading bilevel.rs's
set-up (it also tried 32 knots and a count). Its step at the model's rate, 1/64, is bilevel's too; measuring
1/32 to 1/256 picked it. It moves both knots.

The repository's history before format version 9 holds the code that version replaced: the tables and map in
`src/cm/`, which followed PAQ (lpaq1 and paq8, Matt Mahoney, GPL-2.0 or later), and the MQ decoder in
`src/fax/jbig2.rs`, translated from pdf.js's `arithmetic_decoder.js` (Apache-2.0, Mozilla). Those earlier
versions are altered versions of that code, and its licences apply to them.

## Encrypted PDFs

Both outputs handle files that open with the empty user password (anyone can read them) and use the standard
security handler: /V 1, 2, 4, 5 and /R 2 to 6 (RC4, AES-128, AES-256), one crypt filter (/CFM /V2, /AESV2 or
/AESV3, or /Identity) named by both /StmF and /StrF, and /EncryptMetadata false. The key comes from the empty
password only (algorithms 2, 2.A and 2.B), and is confirmed by /U (algorithms 6 and 11) and, for AES-256, by
/Perms (algorithm 13).

- Normal PDF: strings and streams are decrypted while reading, optimised as usual, and written with the same
  encryption: the same /Encrypt dictionary (and the objects it refers to), /ID, file key and permissions. Each
  string and stream is encrypted with its own object's key; object streams are encrypted as streams, the
  cross-reference stream is not; AES initialization vectors are derived from the file key and the object, so
  the output is the same on every run. Encrypted XMP metadata is compressed (no tool can read it from the file
  anyway); metadata left unencrypted stays as it is. The self-check reads the output back, checks that it opens
  with the same key and dictionary, and compares the decrypted objects. The file keeps exactly the same
  encryption, permission bits and content (but for the images the lossy step changes at efforts 60 and above),
  so the engine proceeds whatever /P says.
- Archive: each encrypted stream that encrypts back to exactly its stored bytes is stored decrypted (so it
  gets the zlib rebuild, Lepton and the model) and encrypted again on restore. The archive carries the file key
  once and, per stream, its method, object number and generation and its AES initialization vector. When the
  archive isn't clearly smaller than the encrypted streams alone, the archive without decryption is made too
  and the smaller kept. The decoder refuses crypt parts out of place and more than two parts in a row that
  restore nothing.
- Left untouched (normal PDF): files that need a password, other security handlers, signed files, streams
  with their own /Crypt filter, and anything readers may take differently: a /CF entry named /Identity (qpdf
  and pdf.js use it, PDFium and Poppler don't), /CFM /None or no /CFM (qpdf decrypts as AES, PDFium as RC4),
  /StmF and /StrF missing or naming different filters, a /V 4 or 5 key length given nowhere (Poppler takes 40
  bits), a /V 4 key that isn't 128 bits, /V 2 without /Length, /R 2 with more than 40 bits, /O or /U of unusual
  length, a /P outside 32 bits, and literal strings with a raw carriage return. Everything else the gate
  refuses (PDF/A, a signature, ...) still applies, and a file that keeps earlier saves is written as its newest save
  alone, as one that isn't encrypted. In the archive, files whose handler isn't opened as
  above are packed exactly as any other; a stream with its own /Crypt filter, or one that doesn't encrypt
  back to its stored bytes, is kept as stored; strings are never changed.

## Existing library (optional, `--features existing`)

The engine keeps whichever result is smaller after the checks, so an existing library can only help:

- [lepton_jpeg](https://crates.io/crates/lepton_jpeg) (Apache-2.0, Microsoft): recompresses JPEG streams in
  the archive and restores them bit for bit. Apache-2.0 requires shipping its licence and notices with the
  product.

Flate streams that zlib didn't write (Adobe, cairo, Quartz) are stored as they are. preflate-rs did that job
until its dependency `cabac` turned out to be LGPL, which the licence rule doesn't allow, so it is gone; our
own re-creation coder for those streams is a later roadmap item.

## The licence rule

Our own code plus libraries under MIT, Apache-2.0, BSD, ISC, Zlib, Unicode or MPL-2.0 only; never GPL, AGPL
or LGPL. `deny.toml` holds the rule and `cargo deny check` (installed with `cargo install cargo-deny
--locked`) checks it for every feature and target, in CI and in `bench/security.py supply`, together with the
RustSec advisories, version bans and the source of every crate; `cargo deny` skips crates marked `publish =
false`, so `bench/check_dep_sources.py` (CI and `supply` too) refuses any dependency that is not from
crates.io. A default build depends on `aes`, `md-5` and `sha2` (RustCrypto, MIT or Apache-2.0, for the
`encryption` feature) and the small crates they use, all MIT or Apache-2.0; `--features existing` adds
`lepton_jpeg` and what it needs (MIT, Apache-2.0, Zlib, BSD-2-Clause, Unicode-3.0).

## Layout

| Path | What |
| --- | --- |
| `src/api.rs`, `src/clock.rs` | The public API and the deadline behind `time_budget`. |
| `src/feature.rs`, `src/off/` | The Cargo features, and what stands in for a part a build leaves out. |
| `src/flate/` | Our Flate decoder and encoder, and the bit-exact zlib re-creation. |
| `src/pdf/` | PDF objects, strict parser, filters, images, encryption, the normal-PDF writer and its safety gate, the lossy image step and the page walk that measures how large each image is shown. |
| `src/jpeg/` | JPEG parsing and the optimal Huffman tables; the decoder and the baseline encoder of the lossy image step. |
| `src/fax/` | CCITT and JBIG2 codecs, the fax steps of both outputs, the bilevel model. |
| `src/cm/` | The context-mixing model and arithmetic coder. |
| `src/pack/` | The gpress archive (format versions 9 and 10), its rules (`rules.rs`), Type 1 fonts, encrypted streams, SHA-256. |
| `src/par.rs` | The thread pool. |
| `src/bin/gpress.rs` | The command-line tool. |
| `crates/gpress-c/` | C API, header, example. |
| `crates/gpress-wasm/` | WebAssembly module and the TypeScript package. |
| `fuzz/` | Fuzz targets for the archive decoder, the Flate decoder and the PDF path. |
| `tests/` | Integration tests; `tests/data/` holds the zlib vectors, the encrypted and JPEG fixtures and the format fixture. |
| `bench/` | The benchmark suite, the security suite, the independent checks, the stored results, the corpus lock. |
| `docs/` | This reference, the optimisation plan, the task briefs (`docs/tasks/`) for the work it recommends, and the wiki's pages (`docs/wiki/`, which `.github/workflows/wiki.yml` copies to the GitHub wiki on each push to main that changes them). |
| `deny.toml`, `SECURITY.md` | The licence rule; the threat model and how the safety rules are checked. |
| `.github/` | CI, CodeQL, the release and wiki workflows, Dependabot and the pull request template. |
| `LICENSE-MIT`, `LICENSE-APACHE` | gpress's own licence: MIT or Apache-2.0, at your option. |

## Benchmarks (`bench/`)

- `benchmark.py`: the standard measurement, in one command. `time` and `space` measure every file one
  process at a time (wall and CPU time, peak memory, bytes in and out, exactness): ours at each engine level and
  thread count, normal PDF and archive, against qpdf (normal PDF) and xz -9e, Brotli 11, Zstandard 19 with
  long matching and expand + xz (archive), each as its own process. A normal-PDF output must open (page count
  and every page's content decoded with pikepdf), an untouched file must be the input byte for byte, and a
  sample of outputs goes through `verify_pdf.py`; every archive is restored with a separate `unpack` and
  compared byte for byte. Rows go to `results/<run>/*.jsonl` as each file finishes, so a stopped run
  continues where it was. `report` makes the tables; `compare BASE NEW` fails when a setting got more than
  10% slower in total, a file more than 2x slower, any output larger, or the peak memory more than 10%
  higher; `selftest` plants each of those and checks they are caught. Every run starts with a calibration job
  so `compare` can tell a slower build from a slower machine. The competitors need measuring only once:
  `--with RUN` takes the settings a run lacks from a stored one (`results/all907-speed-ca8fa92` has every
  competitor on all 907 files). `measure.c` is the small parent process that runs each command (Linux gives
  a process the peak memory of the one that forked it). File lists are in `sets/` (`all`, and `sample120`:
  the 20 slowest and the 20 largest files, 34 with 6 in both lists, then 86 random ones, in two sections
  `--set sample120:random` and `:slowest-and-largest` select); the engine comes from `GPRESS` and the
  corpus from `CORPUS` (default `bench/corpus`, where `fetch_corpus.sh` puts it).
- `p5.py`: what the threads and the store do for a normal PDF, file by file. `run NAME --engine BIN`
  optimizes each file of a list (`--set`, `--files`; `--limit N` takes the N smallest) at each engine level and
  thread count, one process at a time with `PDFE_DEBUG` set, and keeps the child's wall and CPU time, peak
  memory, the SHA-256 of its output and the summary line in `results/NAME/p5-eE-tT.jsonl`. `report NAME`
  gives for each engine level the files whose output is the same at every thread count, the work saved (identical
  streams, store hits, segments planned once), CPU time against one thread, wall time with the busy and idle
  shares (on a quiet machine only), the store's peak and the peak memory; `--base OTHER` adds the CPU time
  saved on the files where repeats were found.
- `fetch_corpus.sh`, `sets/corpus.lock`: the 907 test files from their four public sources, pinned by commit
  with the sha256 of every file; the script fetches a copy in about two minutes and `--check` proves a copy
  has exactly those bytes.
- `security.py`: hostile input, pass or fail. Twenty archives damaged every way must be refused or restored
  exactly; hostile PDFs (nesting 100,000 deep, loops in the xref chain and the object streams, Flate, LZW and
  RunLength bombs, object streams past `max_stream`, never read through, too many, with every entry at one
  offset or each holding the next one, a 100,000 x 100,000 image, a million objects, bad UTF-16) must come
  back untouched, smaller or with a clean error inside a time and memory bound that grows with the input;
  mutation fuzzing of optimize, pack and unpack under a hard 512 MiB address space; `cargo audit`, the
  licence of every dependency and every `unsafe` block with its reason; the same inputs through the C library
  (`gpress_harness.c`). `selfcheck` plants a wrong file, a crash, a hang, a memory blow-up, a bad licence and
  a bare `unsafe` and checks that each is caught.
- `verify_pdf.py`: the independent check of one output: qpdf --check, pikepdf (object graph followed by
  value, streams decoded; a JPEG whose bytes changed must have the same coefficients, `jpegtran -copy all`
  writing the same bytes for both, and decode to the same pixels with qpdf's libjpeg, Pillow and pdf.js's own
  JPEG decoder, `verify_jpeg_pdfjs.mjs`; CCITT, JBIG2 and JPEG 2000 streams are compared by their decoded
  pixels with pikepdf's decoders, libtiff, jbig2dec and OpenJPEG, then Poppler's when pikepdf can't decode one
  or disagrees) and PDFium (pixels and text of every page, up to 30 pages). jbig2dec and jbig2enc are used
  only by the checks and benchmarks, never by the engine.
- `verify_render.py`, `verify_pdfjs.mjs`: two more renderers on every output, Poppler and pdf.js (pixels and
  text of every page; no new Poppler errors).
- `verify_encryption.py`: an output keeps the original's encryption exactly (qpdf's own reading of the
  revision, permissions, methods and file key, the /ID and the /Encrypt dictionary).
- `check_the_checkers.py`: the checks must catch one small change (a character, a metadata value, the page
  size, one bit of one pixel, one JPEG coefficient, a JPEG layout only pdf.js reads differently) and pass an
  unchanged copy and a JPEG with other Huffman tables.
- `extract_streams.py`, `flate_bench.py`, `jpeg_bench.py`, `fax_bench.py`, `examples/type1_fonts.rs`: the
  codecs one at a time against zlib-9, libdeflate, Zopfli, jpegtran, mozjpeg, xz and jbig2enc.

Results: [bench/RESULTS.md](../bench/RESULTS.md). The plan for making the engine faster and leaner without
changing a byte of its output: [docs/optimisation-plan.md](optimisation-plan.md), with a brief per
branch under [docs/tasks/](tasks/README.md).
