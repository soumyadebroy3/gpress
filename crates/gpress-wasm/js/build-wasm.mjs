// Builds the module for the package (`npm run build:wasm`): cargo's "wasm" profile (Cargo.toml: built for
// size, LTO, one codegen unit, panic = "abort", symbols stripped), then binaryen's wasm-opt when it is on
// PATH. Without wasm-opt the module is used as cargo built it, and a message says so.
//
//   node build-wasm.mjs [--out FILE] [cargo arguments, such as --features web]
//
// The output is dist/gpress.wasm unless --out names another file. bench/sizes.py builds the module with this
// script too, so the size it records is the size of the package's module.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
let out = join(here, "dist", "gpress.wasm");
const at = args.indexOf("--out");
if (at >= 0) {
  out = resolve(args[at + 1]);
  args.splice(at, 2);
}

// wasm-opt's setting. -O1, not -Oz: on this module -Oz (and -Os) takes 12% off the raw bytes but adds 4% to
// the gzip size, which is what a browser downloads, and makes the first call in a fresh process slower;
// -O1 takes 8% off the raw bytes and 3% off the gzip size at the same speed (bench/RESULTS.md, "Lightweight").
const level = "-O1";

// The WebAssembly features rustc's wasm32-unknown-unknown target uses by default. The module is stripped,
// so wasm-opt cannot read them from it and has to be told; it fails with a clear message if rustc starts
// using one more.
const features = [
  "--enable-bulk-memory",
  "--enable-multivalue",
  "--enable-mutable-globals",
  "--enable-nontrapping-float-to-int",
  "--enable-reference-types",
  "--enable-sign-ext",
];

const cargo = spawnSync(
  "cargo",
  ["build", "--profile", "wasm", "-p", "gpress-wasm", "--target", "wasm32-unknown-unknown", "--message-format=json-render-diagnostics", ...args],
  { cwd: here, stdio: ["inherit", "pipe", "inherit"], encoding: "utf8", maxBuffer: 1 << 28 },
);
if (cargo.error) {
  console.error(`build-wasm: cannot run cargo: ${cargo.error.message}`);
  process.exit(1);
}
if (cargo.status !== 0) process.exit(cargo.status ?? 1);
let built = null;
for (const line of cargo.stdout.split("\n")) {
  if (!line.startsWith("{")) continue;
  const m = JSON.parse(line);
  if (m.reason === "compiler-artifact" && m.target?.name === "gpress_wasm") {
    built = m.filenames.find((f) => f.endsWith(".wasm")) ?? built;
  }
}
if (!built) {
  console.error("build-wasm: cargo did not report the module it built");
  process.exit(1);
}

mkdirSync(dirname(out), { recursive: true });
const probe = spawnSync("wasm-opt", ["--version"], { encoding: "utf8" });
if (probe.error || probe.status !== 0) {
  console.error("build-wasm: wasm-opt is not on PATH, so the module is used as cargo built it (install binaryen for a smaller one)");
  copyFileSync(built, out);
} else {
  const opt = spawnSync("wasm-opt", [level, ...features, built, "-o", out], { stdio: "inherit" });
  if (opt.status !== 0) {
    console.error("build-wasm: wasm-opt failed");
    process.exit(opt.status ?? 1);
  }
  console.error(`build-wasm: ${probe.stdout.trim()}, ${level}`);
}
console.error(`build-wasm: wrote ${out}`);
