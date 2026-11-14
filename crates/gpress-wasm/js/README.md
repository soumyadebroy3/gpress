# gpress for the browser and Node

PDF compression, lossless by default, as a plain WebAssembly module with a typed wrapper: smaller PDFs that
standard readers open, and the gpress archive that restores a file byte for byte.

The package is not on npm yet: build it in this directory (`npm ci && npm run build`), or install a tarball
made by `npm pack`.

```ts
import { Gpress } from "gpress";
const g = await Gpress.load(fetch("/gpress.wasm"));                 // or gpress-web.wasm: no archive, smaller
// In Node, pass a file URL (or a path from the working directory) instead:
// await Gpress.load(new URL(import.meta.resolve("gpress/gpress.wasm")))
const smaller = g.optimize(bytes);                                  // { data, outcome, reason? }
const fits = g.optimizeToSize(bytes, 500 * 1024);                  // the same, with effort and reached
const archive = g.compress(bytes);
const original = g.expand(archive.data);                            // throws GpressError on a damaged archive
```

The options, the effort levels, the outcomes and the error codes are in the project's
[README](https://github.com/soumyadebroy3/gpress#use-it-from-code) and
[reference](https://github.com/soumyadebroy3/gpress/blob/main/docs/reference.md).

## Licence

Either of the Apache License, Version 2.0 (`LICENSE-APACHE`) or the MIT licence (`LICENSE-MIT`), at your option.
Both files are in this package.
