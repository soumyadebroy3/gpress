// Builds the module for the package (`npm run build:wasm`): cargo's "wasm" profile (Cargo.toml: opt-level 3,
// LTO, one codegen unit, panic = "abort", symbols stripped), then binaryen's wasm-opt when it is on PATH.
// Without wasm-opt the module is used as cargo built it, and a message says so.
//
//   node build-wasm.mjs [--out FILE | --out=FILE] [cargo arguments, such as --features web]
//
// The output is dist/gpress.wasm unless --out names another file; every other argument goes to cargo. The
// module is written to a temporary file next to the output and renamed over it when complete, so a failed
// step leaves no partial module there. bench/sizes.py builds the module with this script too, so the size it
// records is the size of the package's module.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, renameSync, rmSync, statSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const usage = "usage: node build-wasm.mjs [--out FILE | --out=FILE] [cargo arguments, such as --features web]";
let out = join(here, "dist", "gpress.wasm");
const args = [];
const argv = process.argv.slice(2);
for (let k = 0; k < argv.length; k++) {
  let file;
  if (argv[k] === "--out") {
    // The next argument, unless there is none or it is another option (`--out --features web`).
    const next = argv[k + 1];
    if (next !== undefined && !next.startsWith("-")) file = argv[++k];
  } else if (argv[k].startsWith("--out=")) {
    file = argv[k].slice("--out=".length);
  } else {
    args.push(argv[k]);
    continue;
  }
  if (!file) {
    console.error(`build-wasm: --out needs a file name\n${usage}`);
    process.exit(2);
  }
  out = resolve(file);
}

// wasm-opt's setting. -O1, not -Oz: on this module -Oz, -Os and -O2 take more off the raw bytes (-Oz 10%) but
// less off the gzip size, which is what a browser downloads; -O1 takes 7% off the raw bytes and the most off
// the gzip size (3%), at the same speed (bench/RESULTS.md, "Lightweight").
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
const tmp = join(dirname(out), `.${basename(out)}.${process.pid}.tmp`);
/** Removes the temporary file and exits with the message. */
function fail(message, status) {
  rmSync(tmp, { force: true });
  console.error(`build-wasm: ${message}`);
  process.exit(status);
}
const probe = spawnSync("wasm-opt", ["--version"], { encoding: "utf8" });
const optimized = !probe.error && probe.status === 0;
if (!optimized) {
  console.error("build-wasm: wasm-opt is not on PATH, so the module is used as cargo built it (install binaryen for a smaller one)");
  try {
    copyFileSync(built, tmp);
  } catch (e) {
    fail(`cannot copy the module: ${e.message}`, 1);
  }
} else {
  const opt = spawnSync("wasm-opt", [level, ...features, built, "-o", tmp], { stdio: "inherit" });
  if (opt.error || opt.status !== 0) {
    const why = opt.error ? `: ${opt.error.message}` : opt.signal ? ` (${opt.signal})` : ` (exit status ${opt.status})`;
    fail(`wasm-opt failed${why}`, opt.status || 1);
  }
}
try {
  renameSync(tmp, out);
} catch (e) {
  fail(`cannot write ${out}: ${e.message}`, 1);
}
if (optimized) {
  console.error(`build-wasm: ${probe.stdout.trim()}, ${level}: ${statSync(built).size} bytes from cargo, ${statSync(out).size} after`);
}
console.error(`build-wasm: wrote ${out}`);
