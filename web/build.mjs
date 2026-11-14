// Puts the package's two files where the page loads them from: dist/gpress.js and dist/gpress-web.wasm of
// crates/gpress-wasm/js (built there with `npm run build`) are copied into web/dist/.
//
//   node web/build.mjs           web/dist/ only, to serve web/ as it is
//   node web/build.mjs SITE      also the page's own files, so that SITE is all a web server needs
import { copyFileSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const web = dirname(fileURLToPath(import.meta.url));
const dist = join(web, "..", "crates", "gpress-wasm", "js", "dist");
const site = process.argv[2] ? resolve(process.argv[2]) : web;

mkdirSync(join(site, "dist"), { recursive: true });
try {
  for (const file of ["gpress.js", "gpress-web.wasm"]) copyFileSync(join(dist, file), join(site, "dist", file));
} catch (e) {
  console.error(`build: ${e.message}\nbuild: build the package first: npm ci && npm run build in crates/gpress-wasm/js`);
  process.exit(1);
}
// The page's own files: every .html, .css and .js beside this script.
if (site !== web) for (const file of readdirSync(web).filter((f) => /\.(html|css|js)$/.test(f))) copyFileSync(join(web, file), join(site, file));
console.error(`build: wrote ${site}`);
