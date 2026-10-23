## What and why

<!-- What this changes and why. Link the issue, or the brief in docs/tasks/ it carries out. -->

## How it was checked

<!-- The commands you ran and what they showed. CI runs all of these again, and main only takes a pull request
whose checks pass ("All checks passed" and CodeQL). -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`, and again with `--features existing`
- [ ] `cargo test --release --workspace`, and again with `--features existing`
- [ ] A changed dependency: `cargo deny check` and `python3 -I bench/check_dep_sources.py`
- [ ] A change to the engine or `crates/gpress-wasm/`: the WebAssembly package (`npm ci && npm run build && npm test` in `crates/gpress-wasm/js`) and `python3 -I bench/sizes.py check`

## Output

- [ ] Outputs are byte-identical to `main`'s, or the change in output is the point of this pull request and is described above
- [ ] Every speed, size or memory figure above was measured (how, on which files, on what machine), not estimated

## Safety

- [ ] Untrusted input stays bounded: nothing a file claims sizes memory or work on its own (SECURITY.md)
- [ ] Every new `unsafe` block says why it is sound
- [ ] README.md, docs/reference.md and SECURITY.md say what the code now does
