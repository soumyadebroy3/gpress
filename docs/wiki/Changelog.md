Every pull request merged into `main` (or into a branch that reached it), newest first, with its title as it stands on GitHub. Dates are UTC. What the recent ones give is on [Status and roadmap](Status-and-roadmap).

## 2026-11-15

| Pull request | Change |
| --- | --- |
| [#47](https://github.com/soumyadebroy3/gpress/pull/47) | Raise the web module's size target to 1 MB gzip |
| [#43](https://github.com/soumyadebroy3/gpress/pull/43) | Effort levels: 0, 20 and 40 lossless, 60, 80 and 100 lossy, and a target size |
| [#46](https://github.com/soumyadebroy3/gpress/pull/46) | Archive rules: every tuning value of the archive as a setting |

## 2026-11-14

| Pull request | Change |
| --- | --- |
| [#42](https://github.com/soumyadebroy3/gpress/pull/42) | Keep earlier revisions when compressing, drop them only on request |
| [#45](https://github.com/soumyadebroy3/gpress/pull/45) | Flate encoders: 181 tuning values as named settings (group flate_) |
| [#44](https://github.com/soumyadebroy3/gpress/pull/44) | Safety gate: find more spellings of a missing object's header; refuse a deeply nested trailer |
| [#41](https://github.com/soumyadebroy3/gpress/pull/41) | Images: 1-bit images as JBIG2, images of few colours as Indexed |
| [#40](https://github.com/soumyadebroy3/gpress/pull/40) | Safety gate: require a catalog, and keep the output within qpdf 12's bound |
| [#38](https://github.com/soumyadebroy3/gpress/pull/38) | Release workflow: downloads for five targets, the npm tarball and the C library, as a draft release |
| [#37](https://github.com/soumyadebroy3/gpress/pull/37) | Update aes to 0.9.4 |
| [#39](https://github.com/soumyadebroy3/gpress/pull/39) | Tests: fix the loop that clippy of Rust 1.100 rejects |

## 2026-11-10

| Pull request | Change |
| --- | --- |
| [#34](https://github.com/soumyadebroy3/gpress/pull/34) | Revision check: test the trailer keys left out of the comparison, and say the rule more plainly |
| [#32](https://github.com/soumyadebroy3/gpress/pull/32) | Cargo: sha2 0.11.1 in both lock files |
| [#33](https://github.com/soumyadebroy3/gpress/pull/33) | CI: build and test the WebAssembly package with Node 24 |
| [#17](https://github.com/soumyadebroy3/gpress/pull/17) | Revision check: leave files with earlier saves untouched even when no object is redefined |
| [#31](https://github.com/soumyadebroy3/gpress/pull/31) | WebAssembly package: TypeScript 7.0.2 |
| [#29](https://github.com/soumyadebroy3/gpress/pull/29) | CI: actions/checkout 7.0.1 in every job that checks out |
| [#30](https://github.com/soumyadebroy3/gpress/pull/30) | CI: actions/setup-node 7.1.0 for the WebAssembly package |
| [#28](https://github.com/soumyadebroy3/gpress/pull/28) | CI: actions/upload-artifact 7.0.2 for the WebAssembly package |
| [#27](https://github.com/soumyadebroy3/gpress/pull/27) | CI: actions/setup-go 7.0.0 for the secret scan |

## 2026-10-25

| Pull request | Change |
| --- | --- |
| [#26](https://github.com/soumyadebroy3/gpress/pull/26) | README: a classier front page, every figure checked against the record |
| [#18](https://github.com/soumyadebroy3/gpress/pull/18) | pr-review: outside pull requests never run in a session and are the owner's to merge |
| [#16](https://github.com/soumyadebroy3/gpress/pull/16) | Ready to go public: licence, README, PR checks and security scans |
| [#19](https://github.com/soumyadebroy3/gpress/pull/19) | Clean-room model tables, probability maps and MQ decoder (archive format 9) |

#19 was merged into the branch of #16 and reached `main` with #16.

## 2026-10-23

| Pull request | Change |
| --- | --- |
| [#15](https://github.com/soumyadebroy3/gpress/pull/15) | Lightweight: build only what a product needs, memory that follows the input, the report (L1, L3, L5) |
| [#14](https://github.com/soumyadebroy3/gpress/pull/14) | Pure gains and the parallel engine (A7, A8, B2, D5, B1, P1 to P5) |

## 2026-10-22

| Pull request | Change |
| --- | --- |
| [#13](https://github.com/soumyadebroy3/gpress/pull/13) | Object streams: read once, within max_stream (a memory fix) |
| [#2](https://github.com/soumyadebroy3/gpress/pull/2) | Flate: the fixed costs per block (A1, A3) |
| [#8](https://github.com/soumyadebroy3/gpress/pull/8) | Lightweight: a smaller WebAssembly module, a size budget in CI, and its start-up measured (L2, L4) |

## 2026-10-16

| Pull request | Change |
| --- | --- |
| [#12](https://github.com/soumyadebroy3/gpress/pull/12) | ASCII decoders honour the decode limit; default threads back to at most 8 |
| [#11](https://github.com/soumyadebroy3/gpress/pull/11) | CLAUDE.md: commits are Soumya's, with no Claude trailers |
| [#3](https://github.com/soumyadebroy3/gpress/pull/3) | Archive: the model 1.4x faster and one copy of the decoded bytes (D1, D2, D4) |
| [#10](https://github.com/soumyadebroy3/gpress/pull/10) | CI: run the checks as parallel jobs |
| [#1](https://github.com/soumyadebroy3/gpress/pull/1) | Add fast Deflate encoder for effort levels 0-19 |
| [#9](https://github.com/soumyadebroy3/gpress/pull/9) | Flate parse: one length at a time in WebAssembly (the PR #6 slowdown there) |

## 2026-10-15

| Pull request | Change |
| --- | --- |
| [#6](https://github.com/soumyadebroy3/gpress/pull/6) | Flate: faster parse and search, lists in less memory (A6, A5, B3) |
| [#7](https://github.com/soumyadebroy3/gpress/pull/7) | CI: clippy on Rust 1.99 and a WebAssembly job that runs its tests |
| [#5](https://github.com/soumyadebroy3/gpress/pull/5) | Add pr-review mod: fixed review of every PR, no merge without a pass |
| [#4](https://github.com/soumyadebroy3/gpress/pull/4) | Add cheap-route mod: send easy subagent tasks to a cheaper model |

## Before pull requests

The engine came first, committed to `main` on 2026-10-07 and 2026-10-08 (partly through a merge of an integration branch): gpress's own Flate decoder and optimal-parse encoder, the PDF reader and writer with its safety gate, the archive format with its context-mixing model, and the engine as a library with C and WebAssembly bindings. Planning documents followed on 2026-10-09, and the first pull request was merged on 2026-10-15.
