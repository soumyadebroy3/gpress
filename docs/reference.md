# gpress reference

Everything the [README](../README.md) leaves out: every option, the parts a build can leave out, what each
part of the engine does, how encrypted PDFs are handled, the dependency licence rule, the repository layout and
the benchmark and check scripts.

## Options

Everything a caller can tune is a field of `Options` (the C struct has every field but `store_budget`,
`flate_rules` and `rewrite_rules`, and the TypeScript options most of them; those stay at their defaults in both),
with `Options::check` saying whether the engine accepts a set; nothing is read from the environment except
`PDFE_DEBUG`, which only prints timings and figures to standard error. For a normal PDF that includes one
summary line per file, which `bench/p5.py` reads:

    plan: S streams, J jobs (I identical to an earlier one, H sharing their input); store: budget B, peak P bytes, K kept, X hits, Y computed, about Z s saved; segments planned once: N

the streams, the jobs that encode them (a stream identical to an earlier one is no job of its own; H jobs
have the encoder input of another under another dictionary), what the store held at most and answered (with
the encoding time its answers saved, roughly), and the segments written from an earlier segment's plans.

| Option | Default | What it does |
| --- | --- | --- |
| `effort` | 100 | 0 (fastest) to 100 (smallest), both outputs. Normal PDF: 0 is the fast path, our fast Flate encoder (`src/flate/fast.rs`) with its near-optimal level for page content, fonts and the rest (one pass of it for a stream that decodes to more than 4 MiB) and a lazy level for images, image predictors chosen from an estimate instead of trial encodes, and only the cheap steps (object streams, the cross-reference stream, JPEG Huffman tables, JBIG2 for fax images); on the 120-file sample it took 16.1 s of CPU when first measured, against 25.3 s for qpdf's strongest lossless setting and 195.5 s for the strong encoder that effort 0 used before (one thread; files of 1 to 10 MB: median 0.20 s, p90 1.14 s, slowest 3.9 s). 1 to 19 bridge to the strong encoder: images get closer parses, from 10 the streams that take the most room get the strong encoder's quick settings within a budget that doubles every point (and so do the object streams and the cross-reference stream when they fit), except a Flate stream that effort 0's settings can't shrink, which is kept as it is. 20 to 100 are the strong encoder: the Flate settings go in steps of 20, the streams that take the most room get the strongest step that fits a work budget proportional to the file's size, and image predictors are ranked on sampled rows (all as before the fast path). Below 100 the decoded bytes have a budget too (16 per byte at effort 0, twice as much every 10 points, at least 4 MiB), so the time follows the file's size even when its streams decode to a hundred times their size. On the sample, as first measured for the fast path in October 2026 (CPU s, output as a share of the input on the 90 files it rewrites): 0: 16.1, 87.02%; 5: 23.0, 86.70%; 10: 40.7, 86.11%; 15: 111, 85.74%; 19: 183, 85.59%; 20: 244, 85.43%; 50: 334, 85.17%; 100: 983, 85.01% (qpdf's strongest lossless setting: 87.23% counting only the outputs bench/verify_pdf.py finds the same apart from the trailer's /ID). Later changes made 20, 50 and 100 faster without changing their output: measured again on a machine about 1.2 times slower, the current code takes 16.8 s at 0, 176 s at 50 and 512 s at 100 (bench/RESULTS.md, "Effort levels"). In the archive, data that looks compressed already is coded plainly below 50. Decided from sizes only, never from time. |
| `threads` | 0 | 0: every core the machine offers, at most 8 (each thread holds a stream of its own, so memory grows with them, until a budget for what they hold at once exists); at most 64. The output is identical for any count (the pieces of work are the same and are put together in order); a thread the system refuses is no loss. No more stream jobs or encoders run at once than there are threads (`SECURITY.md`, "Memory with threads"). |
| `store_budget` | 2 MiB | Normal PDF: bytes of encoded outputs one file keeps, so that an input of at most 256 KiB that another stream also encodes (the same data under another dictionary, say) is encoded once; a larger repeat is encoded again. The oldest go first, and each kept output counts 512 bytes besides its own; 0 keeps none, at most 1 GiB. A kept output is used only after it decodes to the same input. Never changes the output; nothing is kept from one call to the next (`SECURITY.md` says why). The `PDFE_DEBUG` summary gives its peak and hits. |
| `max_input` | 1 GiB | Larger inputs are refused at once. |
| `max_stream` | 64 MiB | Normal PDF: a stream that decodes to more is kept as it is. Object streams are decoded once each (in an encrypted file whose metadata is not encrypted, the catalog's once more) and let go once their objects are read; each may decode to this much or 256 MiB, whichever is less, at each filter stage; the ones held at once to four times that, each header entry kept counted as 16 bytes; and all of them, their stored bytes and every stage counted, to four times that or 64 bytes per byte of the file, whichever is more. A file past any of these is left as it is (a `max_stream` of 0 leaves every file that has an object read from an object stream; the C and TypeScript options read 0 as the default). These count decoded bytes, not the objects parsed from them, which take more: an array of small integers about 35 times its bytes (48 MiB of them, in a file of 50 KB within every default limit, took 1.7 GB). |
| `max_stream_archive` | 256 MiB | Archive: a stream that decodes to more is stored as it is. The object streams read to open an encrypted file are held to it as `max_stream` holds them; past it, the file's encrypted streams are stored as they are. |
| `max_decoded` | 256 MiB | Archive, both ways. Compress: the most decoded bytes the parts of one archive may claim in all; in file order, a stream whose decoded form (plus any Lepton data for it) does not fit in what is left is stored as it is, decided from sizes alone. The archive's header declares the total. Expand: an archive declaring more than this is refused before anything is decoded (`Error::DecodedTooLarge`), and while decoding every part is held to what the header declared, so a damaged body can claim no more. The test set's largest file needs 232 MiB (`enormous.pdf`, 310 KB). |
| `max_len` | 1 GiB | Expand: an archive declaring more is refused before anything is decoded or reserved (`Error::DeclaredTooLarge`). The format's own limit is 16 GiB. |
| `objects_per_stream` | 200 | Normal PDF: most objects per compressed object stream. |
| `respect_conformance` | on | Normal PDF: leave PDF/A, PDF/X, signed, certified and usage-rights files untouched. Off, every object's value is still kept, but those guarantees are given up. |
| `flate_over_jpeg` | on | Normal PDF: also try our Flate in front of a JPEG (never in front of JPEG 2000 or JBIG2, whose filter chain keeps its shape for readers such as pdf.js). |
| `jpeg_tables` | on | Normal PDF: JPEGs get optimal Huffman tables (the same coefficients). |
| `jbig2` | on | Normal PDF: CCITT fax images become JBIG2 when that is at least 5% smaller (the same pixels), and so do 1-bit images stored without an image codec (a DeviceGray image or a stencil mask under Flate, LZW, RunLength, the ASCII filters or none) when JBIG2 is at least 5% smaller than the best other encoding of the stream (both 5%: the rule `rewrite_jbig2_min_saving`). |
| `palette` | on | Normal PDF: an 8-bit DeviceGray or DeviceRGB image that uses few colours becomes an Indexed image over its own colour space (the same colour for every pixel) when the whole stream, its colour table counted, comes out smaller. |
| `rewrite_encrypted` | on | Normal PDF: files the empty user password opens are rewritten with their encryption kept exactly. Off, every encrypted file is left untouched. |
| `drop_history` | off | Normal PDF: a file that keeps earlier saves (incremental updates) is written with every save kept and compared ("Earlier revisions" under "What is ours"). On, only the newest save is written, as a file of one save, and the earlier ones are gone for good: `Report::history_dropped` lists them, and the command-line tool prints one line for each on standard error. A file where any save is signed, certified, PDF/A or PDF/X or has usage rights stays untouched either way. |
| `decrypt` | on | Archive: the streams of such files are stored decrypted (so they get the zlib rebuild, Lepton and the model) and encrypted again on expand. |
| `use_existing_libraries` | on | Use Lepton for JPEG streams in the archive when the build has it (`--features existing`). An archive with parts Lepton made expands only with such a build (the error says so). |
| `time_budget` | none | Give up after this much wall time: `Error::TimeBudget` and no output, the input unchanged. Checked between streams, Flate segments and archive parts (every 64 KiB of model work) in all three operations, so it can be overshot by a little (on expand by one part's zlib re-creation). A call that finishes in time gives the same bytes as one without a budget. |
| `rewrite_rules` | each rule's default | Normal PDF: the values the rewrite decides by, each a rule with a name, a default and a range ([Rules of the rewrite](#rules-of-the-rewrite)). |

Memory while expanding follows these three: at most about `max_len` for the output, `max_len + max_decoded`
for the model's history (one byte per byte it codes, and while an image part is coded a ring of its residuals:
about a row and a pixel, never more than the part), and the model's tables (up to 164 MiB for files over
4 MiB). A decoded stream is not kept beside the history: its zlib stream is made again from there, with one
copy of the part only for PNG or TIFF predictor rows or a JPEG, and up to three while a Type 1 font is closed
again (each at most `max_decoded` and 2 GiB). Compress holds the input and its archive. Its encoder holds the
same history and tables and no copy of the file: a stream is inflated again as it is coded, a Deflate block at
a time (a Type 1 font whole; for PNG predictor rows also their row types, a byte a row, and about 64 KiB of
rows, or one row if longer). The planner keeps no decoded Flate data, only each stream's zlib settings and
Lepton data, a fax stream's image and an encrypted stream's decrypted bytes. Every pack ends with a restore
check, which is an expand as above: after the encoder with one thread, beside it with two (two histories and
two sets of tables at once). The security suite runs every pack and unpack with `max_decoded` at 64 MiB and
holds them to 512 MiB.

Three kinds of constants stay in the code: the ones that define the file formats (a decoder must agree with
its encoder on those: the model's settings, the part tags, the fax work budget); the ones readers set (JBIG2
images of at most 2^30 pixels, the EOL rules readers disagree on); and measured thresholds (at most 2048
pixels of fax work per byte, at most 4096 JPEG segments, plain coding below effort 50, fax streams of at least
64 bytes, Lepton only when 10% smaller than the Flate bytes, 16 cross-reference rows per byte of the file plus
65536). The values the rewrite decides by (JBIG2 only when 5% smaller, Flate in front of a JPEG only when it
saves 1%, the second Flate pass only for JPEGs with poor tables, the quick budget's 16 bytes per byte and 4 MiB,
and more) are rules with those as their defaults: [Rules of the rewrite](#rules-of-the-rewrite). The engine's
own types (`pdf::rewrite::Options`, `pack::PackOptions`, `pack::UnpackOptions`, `flate::Options` with its
iterations, exact rounds, chain length, segment size, splitting, alternatives, multi-start limit, nice length
and polishing) are public too, for measuring and for callers that want a knob the library's `Options` doesn't
carry.

## Rules of the rewrite

The values the lossless rewrite decides by, each a rule: a whole number with a name, a default (the value that
was fixed in the code before), a lowest and a highest value. A rule that is not set has its default, and with
no rule set every output is byte for byte what it was before. In Rust they are the field `rewrite_rules` of
`Options`: `options.rewrite_rules.set("jbig2_min_saving", 10)` (the name without `rewrite_`) returns false, and
changes nothing, for a name that is no rule or a value outside the rule's range; nothing is moved to the
nearest allowed value. Any value in a rule's range is safe: the output is still read back and compared with
the input, returned only when smaller, and the same for any thread count. The rules of a table (`image_level_`,
`big_level_`, `predictor_sample_`, `work_cost_`) are one rule per effort or band of 20.

| Rule | Default | Lowest | Highest | What it decides |
| --- | ---: | ---: | ---: | --- |
| `rewrite_min_file_saving` | 1 | 1 | 1,099,511,627,776 | The rewritten file is returned only when it is at least this many bytes smaller than the input; else the input comes back untouched. |
| `rewrite_min_stream_saving` | 1 | 1 | 1,099,511,627,776 | A stream gets a new encoding only when its new data is at least this many bytes smaller than its stored data (with the stream's dictionary counted where the fast path chooses an image's predictor). |
| `rewrite_flate_over_image_min_saving` | 1 | 0 | 100 | Our Flate goes in front of an image codec that had no general filter (a JPEG, say) only when it saves more than this percent (over the JPEG with its new tables, when there is one). |
| `rewrite_jpeg_min_saving` | 1 | 1 | 1,099,511,627,776 | A JPEG with new Huffman tables is taken only when it is at least this many bytes smaller. |
| `rewrite_jpeg_code_orders` | 7 | 1 | 7 | Which assignments of code words are tried for each JPEG scan (bits: 1 by symbol value, 2 most frequent first, 4 fewest one bits); the one with the fewest 0xFF bytes is written, the first of equals. |
| `rewrite_jpeg_second_pass_flate_gain` | 10 | 0 | 100 | From `rewrite_full_from`, a JPEG's original bytes get a Flate pass of their own only when Flate took this percent off the JPEG with its new tables, |
| `rewrite_jpeg_second_pass_table_gain` | 10 | 0 | 100 | and the new tables took this percent off the original. |
| `rewrite_jbig2_min_saving` | 5 | 0 | 100 | A fax (CCITT) image becomes JBIG2 only when that is at least this percent smaller, and at least a byte. |
| `rewrite_max_predictor_image` | 50,331,648 | 0 | 1,073,741,824 | An image that decodes to more gets no predictor search. A value over the default counts only as far as `max_stream` (the search holds copies of the image). |
| `rewrite_min_predictor_image` | 1,024 | 0 | 1,099,511,627,776 | An image with fewer bytes of samples gets no predictor search. |
| `rewrite_predictor_candidates` | 55 | 0 | 63 | The predictors ranked by encoding them (bits: 1 none, 2 Sub, 4 Up, 8 Average, 16 Paeth, 32 a type for each row), ranked in the order none, each row, Up, Sub, Paeth, Average; the first of equals wins. 55 is all but Average. |
| `rewrite_fast_predictor_candidates` | 63 | 0 | 63 | The predictors the fast path's estimate compares (the same bits, in the order of the bits). |
| `rewrite_fast_predictor_min_bits` | 8 | 1 | 16 | On the fast path an image with fewer bits per component gets no predictor. |
| `rewrite_row_predictor_rule` | 0 | 0 | 1 | How the type of each row is chosen: 0 the smallest sum of absolute residuals (libpng's way), 1 the fewest bits by an order-0 estimate of the row's residuals. |
| `rewrite_predictor_sample_bands` | 8 | 1 | 1,024 | The rows the predictors are ranked on are taken as this many bands spread over the image. |
| `rewrite_fast_predictor_sample` | 65,536 | 0 | 1,099,511,627,776 | Bytes of an image the fast path's estimate looks at. |
| `rewrite_predictor_sample_0` | 65,536 | 0 | 1,099,511,627,776 | Bytes of an image the predictors are ranked on, when they are ranked by encoding them, at efforts 0 to 19. The whole image when it is smaller, and from `rewrite_full_from` on. |
| `rewrite_predictor_sample_20` | 131,072 | 0 | 1,099,511,627,776 | The same at efforts 20 to 39. |
| `rewrite_predictor_sample_40` | 262,144 | 0 | 1,099,511,627,776 | The same at efforts 40 to 59. |
| `rewrite_predictor_sample_60` | 524,288 | 0 | 1,099,511,627,776 | The same at efforts 60 to 79. |
| `rewrite_predictor_sample_80` | 1,048,576 | 0 | 1,099,511,627,776 | The same at efforts 80 to 99. |
| `rewrite_predictor_rank_level` | 7 | 0 | 12 | The fast encoder's level the predictors are ranked with on the fast path (0: the strong encoder's quick settings, as from effort 20). |
| `rewrite_rank_together` | 8,388,608 | 0 | 1,099,511,627,776 | Predictor candidates whose input is up to this size are ranked side by side, bigger ones one after another. Speed and memory only. |
| `rewrite_object_streams` | 1 | 0 | 1 | Small objects are packed into object streams (1) or written one by one (0). |
| `rewrite_object_stream_order` | 0 | 0 | 1 | The order of the objects in the object streams: 0 by kind (dictionaries by type and subtype, then arrays, then the rest), 1 by object number. |
| `rewrite_xref_predictor` | 0 | 0 | 2 | The cross-reference stream: 0 the smaller of plain and Up-predicted, 1 always plain, 2 always Up. |
| `rewrite_string_form` | 0 | 0 | 1 | A string is written in the shorter of its two forms (0; a literal on a tie) or in hexadecimal (1). |
| `rewrite_full_from` | 100 | 0 | 101 | From this effort on there are no budgets: every stream gets the effort's full settings and predictors are ranked on the whole image (101: never). |
| `rewrite_trial_predictors_from` | 10 | 0 | 20 | On the fast path, from this effort on predictors are chosen by encoding the candidates, below it from an estimate. |
| `rewrite_bridge_from` | 10 | 0 | 20 | From this effort on, below 20, the streams that take the most room get the strong encoder's quick settings within a budget (20: never). |
| `rewrite_bridge_start` | 16 | 0 | 1,048,576 | That budget at `rewrite_bridge_from`, in 256ths of the file's size, |
| `rewrite_bridge_growth` | 200 | 100 | 1,000 | and what it is multiplied by for each point of effort above, in percent. |
| `rewrite_bridge_test` | 1 | 0 | 1 | On that bridge, a Flate stream (not an image) that effort 0's settings cannot shrink is kept as it is (1), or gets the strong encoder all the same (0). |
| `rewrite_big_stream` | 4,194,304 | 0 | 1,099,511,627,776 | Below effort 20, a stream other than an image that decodes to more than this gets the level of `rewrite_big_level_` at most. |
| `rewrite_image_level_0` | 7 | 1 | 12 | The fast encoder's level for images at effort 0. |
| `rewrite_image_level_1` | 7 | 1 | 12 | The fast encoder's level for images at effort 1. |
| `rewrite_image_level_2` | 7 | 1 | 12 | The fast encoder's level for images at effort 2. |
| `rewrite_image_level_3` | 9 | 1 | 12 | The fast encoder's level for images at effort 3. |
| `rewrite_image_level_4` | 9 | 1 | 12 | The fast encoder's level for images at effort 4. |
| `rewrite_image_level_5` | 9 | 1 | 12 | The fast encoder's level for images at effort 5. |
| `rewrite_image_level_6` | 10 | 1 | 12 | The fast encoder's level for images at effort 6. |
| `rewrite_image_level_7` | 10 | 1 | 12 | The fast encoder's level for images at effort 7. |
| `rewrite_image_level_8` | 11 | 1 | 12 | The fast encoder's level for images at effort 8. |
| `rewrite_image_level_9` | 11 | 1 | 12 | The fast encoder's level for images at effort 9. |
| `rewrite_image_level_10` | 12 | 1 | 12 | The fast encoder's level for images at effort 10. |
| `rewrite_image_level_11` | 12 | 1 | 12 | The fast encoder's level for images at effort 11. |
| `rewrite_image_level_12` | 12 | 1 | 12 | The fast encoder's level for images at effort 12. |
| `rewrite_image_level_13` | 12 | 1 | 12 | The fast encoder's level for images at effort 13. |
| `rewrite_image_level_14` | 12 | 1 | 12 | The fast encoder's level for images at effort 14. |
| `rewrite_image_level_15` | 12 | 1 | 12 | The fast encoder's level for images at effort 15. |
| `rewrite_image_level_16` | 12 | 1 | 12 | The fast encoder's level for images at effort 16. |
| `rewrite_image_level_17` | 12 | 1 | 12 | The fast encoder's level for images at effort 17. |
| `rewrite_image_level_18` | 12 | 1 | 12 | The fast encoder's level for images at effort 18. |
| `rewrite_image_level_19` | 12 | 1 | 12 | The fast encoder's level for images at effort 19. |
| `rewrite_big_level_0` | 9 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 0. |
| `rewrite_big_level_1` | 9 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 1. |
| `rewrite_big_level_2` | 9 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 2. |
| `rewrite_big_level_3` | 9 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 3. |
| `rewrite_big_level_4` | 9 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 4. |
| `rewrite_big_level_5` | 9 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 5. |
| `rewrite_big_level_6` | 10 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 6. |
| `rewrite_big_level_7` | 10 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 7. |
| `rewrite_big_level_8` | 10 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 8. |
| `rewrite_big_level_9` | 10 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 9. |
| `rewrite_big_level_10` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 10. |
| `rewrite_big_level_11` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 11. |
| `rewrite_big_level_12` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 12. |
| `rewrite_big_level_13` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 13. |
| `rewrite_big_level_14` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 14. |
| `rewrite_big_level_15` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 15. |
| `rewrite_big_level_16` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 16. |
| `rewrite_big_level_17` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 17. |
| `rewrite_big_level_18` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 18. |
| `rewrite_big_level_19` | 12 | 1 | 12 | The fast encoder's highest level for a stream over `rewrite_big_stream` at effort 19. |
| `rewrite_quick_budget_factor` | 16 | 0 | 1,048,576 | Below `rewrite_full_from`, decoded bytes that may be re-encoded in all: this many per byte of the file at effort 0, |
| `rewrite_quick_budget_doubling` | 10 | 1 | 100 | twice as many every so many points of effort, |
| `rewrite_quick_budget_floor` | 4,194,304 | 0 | 1,099,511,627,776 | and never fewer than this. A stream that does not fit in what is left is kept as it is. |
| `rewrite_work_budget_start` | 100 | 0 | 1,048,576 | Extra encoder work per byte of the file from effort 20, in hundredths of quick-encoding one decoded byte: this much at effort 1, |
| `rewrite_work_budget_doubling` | 10 | 1 | 100 | twice as much every so many points of effort. |
| `rewrite_work_cost_20` | 170 | 100 | 100,000 | The cost of a decoded byte at the Flate settings of efforts 20 to 39, in percent of its cost at the quick settings. |
| `rewrite_work_cost_40` | 200 | 100 | 100,000 | The same for efforts 40 to 59. |
| `rewrite_work_cost_60` | 310 | 100 | 100,000 | The same for efforts 60 to 79. |
| `rewrite_work_cost_80` | 390 | 100 | 100,000 | The same for efforts 80 to 99. |
| `rewrite_step_down` | 1 | 0 | 1 | A stream too big for the effort's settings is offered the lower steps of 20 before the quick settings (1), or the quick settings at once (0). |
| `rewrite_budget_order` | 0 | 0 | 1 | The budgets go to the streams that take the most room first (0), or in the order of their object numbers (1). |
| `rewrite_job_check_ratio` | 2 | 1 | 1,099,511,627,776 | A stream job checks its own new data only when the stream decodes to at least this many times its stored size (the self-check then finds it by its hash). Speed only. |
| `rewrite_most_saves` | 64 | 1 | 1,000 | A file that keeps earlier saves (incremental updates) is left as it is when it has more saves than this. |
| `rewrite_saves_length_factor` | 16 | 1 | 256 | The same when the lengths of its saves add up to more than this many times the file's length. |

Why the ranges end where they do:

- The least savings in bytes (`rewrite_min_file_saving`, `rewrite_min_stream_saving`, `rewrite_jpeg_min_saving`)
  are at least 1, so an output is never as large as what it replaces.
- A highest value of 1,099,511,627,776 (1 TiB) is more than any input at the default `max_input` (1 GiB):
  it means "never" or "always". `rewrite_max_predictor_image` above its default still counts only as
  far as `max_stream`, since the search holds copies of the image, and it goes to 1 GiB at most: the fast
  path's estimate counts an image's bytes in 32 bits.
- `rewrite_jpeg_code_orders` keeps at least one order, and `rewrite_predictor_sample_bands` at least one band.
  `rewrite_quick_budget_doubling`, `rewrite_work_budget_doubling` and `rewrite_job_check_ratio` divide, so they
  are at least 1.
- `rewrite_fast_predictor_min_bits` runs over the bit depths an image can have (1 to 16), and the levels over
  the fast encoder's (1 to 12; `rewrite_predictor_rank_level` 0 is the strong encoder's quick settings).
- `rewrite_bridge_growth` is at least 100%, so the bridge's budget does not shrink as the effort grows, and
  `rewrite_work_cost_` at least 100%, so no step of 20 costs less than the quick settings. Larger budgets cost
  time, not safety: every stream is still held to `max_stream`, and `time_budget` ends a run that takes too
  long.
- `rewrite_most_saves` (1 to 1,000, as a file is read with at most 1,001 cross-reference sections) and
  `rewrite_saves_length_factor` (1 to 256) bound the work on a file with
  earlier saves: each save is read as a file of its own, so the time grows with them.
- `rewrite_full_from`, `rewrite_trial_predictors_from` and `rewrite_bridge_from` are efforts; 101 and 20 mean
  "never". Effort 20 itself (the fast encoder below it) and the bands of 20 stay fixed: the tables here and the
  Flate encoder's are laid out on them.

## Building only what a product needs

The parts of the engine are Cargo features, all on by default. A product that needs fewer builds with
`--no-default-features --features ...`, and the code of a part it leaves out is not compiled. The C library
and the WebAssembly crate have the same features and pass them on to the engine.

| Feature | The part | Without it |
| --- | --- | --- |
| `archive` | The gpress archive: `compress` and `expand`, the context-mixing model, the exact re-creation of zlib streams. | `compress` and `expand` return `Error::NeedsFeature(Feature::Archive)`. `optimize` is not touched. |
| `jpeg` | Normal PDF: optimal Huffman tables for JPEG images. | The step is skipped: the output is the one `jpeg_tables` off gives. |
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

The npm package has two modules: `dist/gpress.wasm` with every part (383,303 bytes of gzip), and
`dist/gpress-web.wasm`, the `web` set (267,937 bytes of gzip, 30% less). Both have the same wrapper; in the web
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
| Fast Flate encoder | `src/flate/fast.rs` | Effort 0 to 19. Twelve levels: hash chains (4-byte hashes, from level 3 also 3-byte ones) with greedy, one-step or two-step lazy matching for levels 1 to 7; binary trees and a cheapest-path parse under costs from a first pass (two passes from level 10) for levels 8 to 12. Blocks end where the symbol statistics change, counted as the symbols are made (no search); each block gets the shared Huffman code builder, a one-pass header and the cheapest of dynamic, fixed and stored. Inputs are cut at fixed 1 MiB offsets, each segment primed with the 32 KiB before it, worked on in parallel and joined bit for bit, so the output is the same for any thread count. On the sample's 3373 Flate streams (152.5 MB, one core), against zlib -9: level 1 +6.5% at 140 MB/s, level 7 -1.2% at 58 MB/s, level 12 -4.2% at 9 MB/s (0.9% above the strong encoder's quick settings, which run at 1.4 MB/s). |
| zlib rebuild | `src/flate/zlibexact.rs` | zlib's deflate re-created bit for bit, to rebuild streams zlib wrote; it follows zlib's own code. Streams other encoders wrote are stored as they are. |
| PDF reader | `src/pdf/parse.rs`, `xref.rs`, `filters.rs` | Strict parser: objects, xref tables and streams, object streams, LZW, ASCIIHex, ASCII85, RunLength, predictors. |
| PDF writer | `src/pdf/rewrite.rs`, `images.rs` | Keeps every reachable object's value; re-encodes streams with our Flate; picks a better PNG predictor (or none) for images, counting the bytes its parameters take; gives JPEGs optimal Huffman tables; puts Flate in front of a JPEG only when that saves at least 1% (never in front of JPEG 2000 or JBIG2); packs objects into compressed object streams grouped by type; writes a compressed xref stream; reads its own output back and compares before returning it (a changed JPEG must read back with the same coded symbols, an image written as JBIG2 with the same pixels, an image written as Indexed with the same colour for every pixel and no other change to its dictionary). An image with fewer samples than its size needs is kept exactly as it is (readers differ on such images). |
| Safety gate | `src/pdf/rewrite.rs` (`gate`) | Encrypted files the empty password doesn't open, signed, certified, PDF/A, PDF/X, files with earlier revisions (incremental updates, as found below) that can't be kept (next row), files we can't read strictly (including a literal string with a raw carriage return, which qpdf reads as a line feed and pdf.js, PDFium and Poppler keep), objects beyond /Size, objects present but not indexed (the trailer or a reached object refers to a number that the cross-reference data doesn't have in use, and a header of that number is in the file's bytes, whatever its generation, with any white space and comments between its parts: Poppler rebuilds a damaged file's cross-reference data from headers, and takes one with two spaces, with its generation spelt `00`, or with a NUL right after the number and a comment before the line end), no catalog (the trailer's /Root is neither a dictionary nor a reference to one that the cross-reference data has), a trailer value nested 16 arrays or dictionaries deep, the innermost counted even when empty (from 64 dictionaries deep, PDFium opens the file and not what would be written from it), object numbers far apart (beyond the cross-reference rows a file of that size may have, 16 per byte plus 65536, or at or above a third of the length of the file that would be written: qpdf 12 reads an object only when its number is below that), object streams past the bounds `max_stream` sets, or with an object read whose header entry starts where an entry listed after it starts too (among the entries up to the first one past the last the cross-reference data points at), or an object that runs into the next one's bytes (each object of an object stream is read only up to where the next entry the cross-reference data reaches starts, which some readers do and others don't): returned untouched. A file has earlier revisions when it has more cross-reference sections than one save writes, a `startxref` before its first section or more of them than sections (a save appended without a /Prev link; those bytes count wherever they are in the file, so inside a stream, a string or a comment too), or an object that two sections, or one section twice, define differently. One save writes one section, or two: those of a linearised file (/Linearized within its first 1024 bytes, the newer section before the older, and the older trailer saying nothing about the document that the newer doesn't), or those of a hybrid file whose newest section is an empty table that names an /XRefStm (what it names lying before the older section, and the two trailers saying the same about the document). Nothing a file claims (an object stream's /N, the trailer's /Size, an image's /Width, predictor parameters) sizes memory or work on its own; every count is checked against the data it describes, or the file's size, first. |
| Earlier revisions | `src/pdf/rewrite/history.rs`, `src/pdf/xref.rs` (`saves`) | A file that keeps earlier saves (incremental updates) is written with every save kept. Where each save ends is found first: every `startxref` has its `%%EOF` before the next one, and the newest save's /Prev chain links each save's cross-reference section, in file order, one section a save (two for a linearised first save). Each save is then read on its own, the file cut at its end, and must open, load and pass the gate as a file of one save. The first revision is written like a file of its own; each later one is appended as one cross-reference stream with /Prev, the objects that revision adds or changes (re-encoded and packed the same way) and a free row for each object it takes away. No object is renumbered: new objects (object streams, cross-reference streams) are numbered above every number any revision uses. Before the output is returned each of its revisions is read back, the output cut at its end, and compared value for value with the input cut at that revision's end. Kept revisions are written losslessly at every effort. Left untouched, with the reason: a file where any save is signed, certified, PDF/A or PDF/X or has usage rights, whatever `respect_conformance` and `drop_history` say (the bytes are searched for /ByteRange, /DocMDP, /FieldMDP, /UR3, GTS_PDFX, GTS_PDFA and `pdfaid:` too, so an object no save reaches counts); a save the newest doesn't link (with `drop_history` it is dropped like the other earlier saves); an /XRefStm in any save (hybrid files); saves that differ in their encryption (with `drop_history` the newest is written with its own); a reference to another generation of an object (a stream's /Length counts, and so does the object stream a row names), or to a number the save doesn't have in use (readers differ on what it names, and Poppler may then look through the bytes and take the last object of every number); a row that doesn't point at the header of its own object (pypdf then moves rows to other numbers or looks through the bytes); a /Root that names a dictionary other than a catalog (PDFium opens no such file, and pypdf shows an earlier save's catalog); a /Prev of 0, where qpdf and PDFium end the chain; a `startxref` or /Prev that names white space or a comment before its section (pypdf may give the chain up there and look through the bytes); a save whose trailer lacks a key that the save before it has and that names an object there (some readers fill it in from the older trailer; with `drop_history`, any key it lacks), or whose trailer nests arrays and dictionaries 16 levels deep; a cross-reference row with a lower generation than an older row of its number (PDFium keeps the higher), with a generation beyond 16 bits or of an unknown type; a row in use for object 0 (qpdf 11 reads the object it points at); a free row of generation 65535 for object 1 (Poppler takes a table that starts with one to start at object 0); a linearisation dictionary whose /L is the whole file's length or a real number (with the whole length, Poppler and PDFium's progressive loader then start from the first-page section and never see a later save); more than 64 saves (the rule `rewrite_most_saves`) or 1001 sections; saves whose lengths add up to more than 16 times the file (`rewrite_saves_length_factor`), or that take more cross-reference rows or object-stream bytes in all than one read of the file may; and a revision whose highest object number, with the new objects', would be at or above a third of the output's length up to its end (qpdf 12 reads no such object; with `drop_history` no earlier revision is written, so only the newest counts). With `drop_history` every save is read and checked the same way, but for the three cases marked above, and only the newest is written; each dropped save is listed, oldest first, with its bytes in the input and how many objects it reaches with a value the output doesn't hold. |
| JPEG tables | `src/jpeg/` | Baseline and extended sequential JPEG (8-bit) written again with optimal Huffman tables, the same coefficients (like `jpegtran -optimize -copy all`): every scan is read to its coded symbols, which are written back with the best code of at most 16 bits that leaves the all-ones code word free (package-merge); of three orders of the symbols within a code length, the one with the fewest stuffed 0xFF bytes is kept. Every other byte stays (ICC and Adobe markers, comments, restart markers, bytes after the end); one table segment goes before each scan. The new file is read back and compared symbol by symbol before it is used. Progressive, arithmetic-coded, lossless, hierarchical, 12-bit, fractionally sampled and anything unusual or damaged is left as it is, and so are layouts readers could take differently once the tables move (fill bytes before a marker; every segment's contents must fill it exactly), data bound to the file's bytes (multi-picture offsets, APP11 boxes such as content credentials), and files of more than 4096 segments. |
| CCITT fax codec | `src/fax/ccitt.rs`, `tables.rs` | Decoder and encoder for T.4 (1-D, 2-D) and T.6 as CCITTFaxDecode uses them. The decoder records every choice of the original encoder (fill bits, EOLs, tag bits, end codes, padding, bytes after the end, and Adobe's way of choosing between pass and horizontal mode) and keeps a row only if writing it again gives the same bits. Checked against libtiff both ways and against libtiff and Poppler on all 183 fax streams of the test files. |
| JBIG2 | `src/fax/jbig2.rs` | MQ coder; generic-region encoder and decoder (T.88, template 0, nominal adaptive pixels, typical prediction); the embedded stream PDF reads. jbig2dec decodes our output exactly. |
| Fax images, normal PDF | `src/fax/pdf.rs` | A CCITT image becomes JBIG2 when that is at least 5% smaller and nothing is uncertain: one CCITT filter with direct parameters, a DeviceGray image or a mask, the width and height the data holds, at most 2^30 pixels (width rounded up to 32; PDFium's JBIG2 decoder, which pdf.js also uses, shows nothing above 2^31), no byte alignment, EOLs where every reader expects them, every row written back bit for bit, end codes or 0 bits at the end, coding work of at most 2048 pixels per byte of data, and our JBIG2 decoder giving the same pixels. JBIG2Decode's output is the same bits as CCITTFaxDecode's (BlackIs1 handled), so Decode, ImageMask and masks keep their meaning. A 1-bit image stored without an image codec (general filters only, or none) is held to the same rules where they apply: a DeviceGray image or a stencil mask, direct width and height that the data fills exactly (rows padded to a byte; the padding bits are not pixels and are not compared), the same size and work bounds, at most 65,536 changing elements and 8 more for each stored byte (general filters can store millions in a few bytes; fax data spends a bit or more on each), an encoding every reader reads our way (see Indexed images), JBIG2 at least 5% smaller than the best other encoding of the stream and smaller with its dictionary counted, and our decoder giving the same pixels. |
| Indexed images | `src/pdf/indexed.rs` | An 8-bit image becomes an Indexed image over its own colour space when nothing is uncertain: DeviceGray or DeviceRGB, named in the image itself, in a file that names no default colour space (DefaultGray, DefaultRGB) and is not encrypted (the table would be sealed after the sizes are compared); general filters only, with an encoding every reader reads our way: no reference in Filter or DecodeParms, no DP (Poppler 24.02 reads it as DecodeParms, PDFium 7999 does not), and a predictor only as a number with the image's own Colors, BitsPerComponent and Columns, a TIFF one at 8 bits only (PDFium 7999 undoes a TIFF predictor with the image's components, whatever its parameters say); direct width and height that the samples fill exactly; no Decode, colour-key Mask, ImageMask, Interpolate or Alternates; not the SMask or Mask of another object, named directly or through an object that is only a reference; no soft mask with a Matte colour; and at most 256 colours of DeviceRGB or 16 of DeviceGray (an index must take fewer bits than the samples it stands for). CalGray, CalRGB and ICCBased images are left alone: readers take an image's samples and a table's colours through such a colour space by routes that can round differently (Poppler 24.02 over ICCBased, PDFium 7999 over CalRGB). Indexes take 8, 4 or 2 bits for at most 256, 16 or 4 colours, rows padded to a byte (never 1 bit: PDFium 7999 draws a 1-bit image that it scales another way than the 8-bit image it was made of); the table holds the colours in ascending order of their bytes. The Indexed image gets the Flate and predictor search of any other image and is kept only when the stream, the table in its dictionary counted, is smaller than the best other encoding. A grey image stored as RGB stays RGB. |
| Encrypted PDFs | `src/pdf/crypt.rs`, `src/pack/sealed.rs` | The standard security handler with the empty user password (see below). RC4 and the key derivations are ours; AES, MD5 and SHA-2 come from RustCrypto. |
| gpress archive | `src/pack/`, `src/cm/` | Any PDF, packed with our context-mixing model (bit-history states, mixers, SSE) with PDF-aware contexts: syntax, predicted object numbers and xref offsets, drawing instructions, image samples coded as residuals. Streams are recognised by their bytes (zlib, JPEG), not only by their dictionaries. Format version 9: the model's logistic tables are worked out from their formulas, and its secondary estimation moves both knots around the input at each bit; a long repeat is coded as one 16-bit-precision decision instead of eight model decisions (huge flat images, duplicates); palette and packed-pixel images are modelled by their exact neighbours; an image's row length is confirmed from its data length; a JPEG behind Flate goes through Lepton when that beats the Flate bytes. Restored byte for byte; every archive is restored and checked before it is returned (with a second thread, the restore decodes the bytes while the encoder writes them). A damaged archive is refused with an error, never restored wrong: the header declares the restored length and the decoded bytes the parts claim in all (`max_len` and `max_decoded` refuse a file declaring more than the caller can afford, before anything is decoded); what a part restores is bounded by what is left of the declared length and what it claims by what is left of the declared decoded total, so the decoder's work and memory follow the two numbers the caller allowed, not what a damaged body says; decoding stops when the data runs out; and the file's checksum is checked at the end. The model's settings, the part kinds and how each is coded are constants of the format version; nothing earlier shipped, so versions before 9 are refused. The fallback form (version 0: the whole file coded by the model alone, written only when the archive fails its own restore check) uses the current model too, so a fallback file from a build before version 9 is refused as damaged. |
| Type 1 fonts | `src/pack/type1.rs`, `src/cm/model.rs` | Type 1 font programs (FontFile with /Length1 /Length2 /Length3, typical of TeX) lose both layers of encryption before coding: the eexec layer (binary, PFB, and hex when its lines are regular) and the charstring layer inside it (lenIV honoured). Both ciphers are reversible byte for byte and are put back on restore; a font that would not come back exactly is coded as it is. On restore, a layout that doesn't fit inside the font's length is refused before anything is decoded for it. The charstrings get their own model kind, which follows their tokens. |
| Fax images, archive | `src/fax/bilevel.rs`, `part.rs` | A CCITT stream is stored as its image, coded by a bilevel context-mixing model (templates up to four rows high, vertical runs, two mixers, two probability maps, 16-bit precision, typical prediction, a fast path for white areas), plus the encoder's choices; on restore our CCITT encoder writes the original bytes back. Streams under 64 bytes stay plain. The model's work is bounded by the file's size (2^24 plus 512 pixels per byte up to any point of the file); streams past it are coded plainly, and a restore that would go past it is refused. |
| SHA-256 | `src/pack/sha256.rs` | For the restore check. |
| Threads and effort | `src/par.rs`, `flate::Options::effort`, `pdf::rewrite::Options`, `pack::PackOptions` | Standard-library threads only: the segments of a stream, the streams of a PDF and the streams the archive looks at are worked on in parallel and put together in their original order, so the output is the same for any thread count (one thread uses no thread machinery at all, as in a browser). Identical streams are encoded once, and so is an input of at most 256 KiB that streams under other dictionaries share: encoded outputs are remembered for the whole file within `store_budget` (a remembered result is checked against its input before it is used), so memory stays bounded. With the strong encoder, in each group of eight segments of a stream, a segment like an earlier one (the same length, history length and bytes) is written from that one's plans, not planned again. Effort as in the table above (the JPEG tables and the JBIG2 conversion run at every effort). |

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
  cross-reference stream is not; AES initialization vectors are derived from the file key and the object (and,
  in a kept revision after the first, from that revision's place), so
  the output is the same on every run. Encrypted XMP metadata is compressed (no tool can read it from the file
  anyway); metadata left unencrypted stays as it is. The self-check reads the output back, checks that it opens
  with the same key and dictionary, and compares the decrypted objects. The file keeps exactly the same
  encryption, permission bits and content, so the engine proceeds whatever /P says.
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
  refuses (PDF/A, a signature, ...) still applies, and earlier revisions are kept as in a file that isn't
  encrypted. In the archive, files whose handler isn't opened as
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
| `src/pdf/` | PDF objects, strict parser, filters, images, encryption, the normal-PDF writer and its safety gate. |
| `src/jpeg/` | JPEG parsing and the optimal Huffman tables. |
| `src/fax/` | CCITT and JBIG2 codecs, the fax steps of both outputs, the bilevel model. |
| `src/cm/` | The context-mixing model and arithmetic coder. |
| `src/pack/` | The gpress archive (format version 9), Type 1 fonts, encrypted streams, SHA-256. |
| `src/par.rs` | The thread pool. |
| `src/bin/gpress.rs` | The command-line tool. |
| `crates/gpress-c/` | C API, header, example. |
| `crates/gpress-wasm/` | WebAssembly module and the TypeScript package. |
| `fuzz/` | Fuzz targets for the archive decoder, the Flate decoder and the PDF path. |
| `tests/` | Integration tests; `tests/data/` holds the zlib vectors, the encrypted and JPEG fixtures and the format fixture. |
| `bench/` | The benchmark suite, the security suite, the independent checks, the stored results, the corpus lock. |
| `docs/` | This reference, the optimisation plan and the task briefs (`docs/tasks/`) for the work it recommends. |
| `deny.toml`, `SECURITY.md` | The licence rule; the threat model and how the safety rules are checked. |
| `.github/` | CI, CodeQL, Dependabot and the pull request template. |
| `LICENSE-MIT`, `LICENSE-APACHE` | gpress's own licence: MIT or Apache-2.0, at your option. |

## Benchmarks (`bench/`)

- `benchmark.py`: the standard measurement, in one command. `time` and `space` measure every file one
  process at a time (wall and CPU time, peak memory, bytes in and out, exactness): ours at each effort and
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
  optimizes each file of a list (`--set`, `--files`; `--limit N` takes the N smallest) at each effort and
  thread count, one process at a time with `PDFE_DEBUG` set, and keeps the child's wall and CPU time, peak
  memory, the SHA-256 of its output and the summary line in `results/NAME/p5-eE-tT.jsonl`. `report NAME`
  gives for each effort the files whose output is the same at every thread count, the work saved (identical
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
