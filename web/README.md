# The gpress demo page

A static page that makes PDFs smaller in the browser with the web module (`gpress-web.wasm`): choose or drop
PDFs, pick one of the six levels or a target size, download each result or all of them as a zip. There is no
framework, no build step of its own and nothing from another site: `index.html`, `style.css` and three small
scripts.

## Build and serve it

From the repository root, with Rust's `wasm32-unknown-unknown` target and Node installed:

```sh
(cd crates/gpress-wasm/js && npm ci && npm run build)   # the package: the wrapper and the two modules
node web/build.mjs                                      # copies the wrapper and the web module into web/dist/
python3 -m http.server --directory web 8000             # then open http://localhost:8000
```

`node web/build.mjs SITE` writes the whole site (the page's files and `dist/`) into the directory `SITE`
instead, for a web server or a host of static files. `.github/workflows/pages.yml` does that for GitHub Pages;
it runs only when started by hand.

## How it keeps files on the device

- The engine runs in a Web Worker (`worker.js`), one file at a time. It cannot be interrupted, so Cancel ends
  the worker and the next file starts a new one.
- The page fetches its scripts and the module once. It keeps the worker's scripts as `blob:` URLs and the
  module compiled, so a new worker needs no network: after loading, the page makes no request at all.
- The page's Content Security Policy allows scripts, styles and requests only from the page's own origin, so
  the browser itself refuses to send anything elsewhere.
- A result is offered only when it is smaller than the file that went in. A file the engine leaves untouched
  is listed with the engine's reason.

`logic.js` holds what needs no browser (the level table, the target-size loop, sizes as text, the zip) and
`node --test web/test/*.test.mjs` tests it; CI runs that in its WebAssembly job.

## Put the module into a page of your own

Copy `dist/gpress.js` and `dist/gpress-web.wasm` from the package build next to your page. Run the engine in a
worker, because a call blocks its thread until it is done:

```js
// worker.js
import { Gpress } from "./dist/gpress.js";
const ready = Gpress.load(fetch(new URL("./dist/gpress-web.wasm", import.meta.url)));
onmessage = async ({ data: file }) => {
  const g = await ready;
  const r = g.optimize(new Uint8Array(await file.arrayBuffer()), { effort: 40 });
  postMessage(r, [r.data.buffer]);   // { data, outcome: "smaller" | "untouched", reason? }
};
```

```js
// the page
const worker = new Worker("worker.js", { type: "module" });
worker.onmessage = ({ data: r }) => console.log(r.outcome, r.data.length, r.reason);
worker.postMessage(document.querySelector("input[type=file]").files[0]);
```

`worker.terminate()` stops a run that takes too long. A call that throws `GpressError` with code 4 has left
its instance unusable: load a new one. A page with a Content Security Policy needs `'wasm-unsafe-eval'` in its
`script-src`. The wrapper's comments in [`gpress.ts`](../crates/gpress-wasm/js/gpress.ts) describe every
option.
