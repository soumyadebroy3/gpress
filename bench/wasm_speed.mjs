// Times the WebAssembly module's optimize calls in one Node process, for comparing builds of the module
// (bench/RESULTS.md, "Lightweight"). It talks to the module's exports directly, so it needs no npm build.
//
//   node bench/wasm_speed.mjs MODULE.wasm LIST [--effort N] [--corpus DIR] [--out DIR]
//
// LIST names corpus files, one per line, relative to the corpus ("#" lines are skipped), like
// bench/sets/sample120.txt; the corpus is --corpus, else $CORPUS, else bench/corpus. --out writes each
// output there under its corpus path, for a byte-for-byte check against the native tool. Prints one JSON
// object: the time to load the module, and per file and in total the CPU and wall seconds of the optimize
// call and the sha256 of its output. CPU time is the whole process's (V8's compiler threads included).
// Run it pinned to one core (taskset -c N) and compare builds back to back.

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(name);
  if (i < 0) return dflt;
  const v = args[i + 1];
  args.splice(i, 2);
  return v;
};
const effort = Number(opt("--effort", "0"));
const corpus = resolve(opt("--corpus", process.env.CORPUS || join(dirname(fileURLToPath(import.meta.url)), "corpus")));
const outDir = opt("--out", null);
if (args.length !== 2) {
  console.error("usage: node bench/wasm_speed.mjs MODULE.wasm LIST [--effort N] [--corpus DIR] [--out DIR]");
  process.exit(2);
}
const [modulePath, listPath] = args;
const files = readFileSync(listPath, "utf8")
  .split("\n")
  .map((l) => l.trim())
  .filter((l) => l && !l.startsWith("#"));

const cpuNow = () => {
  const u = process.cpuUsage();
  return (u.user + u.system) / 1e6;
};

const bytes = readFileSync(modulePath);
let t0 = performance.now();
let c0 = cpuNow();
const mod = await WebAssembly.compile(bytes);
const load = async () => (await WebAssembly.instantiate(mod, {})).exports;
let ex = await load();
const loadWall = (performance.now() - t0) / 1000;
const loadCpu = cpuNow() - c0;

const rows = [];
let cpu = 0;
let wall = 0;
for (const rel of files) {
  const input = readFileSync(join(corpus, rel));
  const row = { file: rel, bytes_in: input.length };
  const p = ex.gpress_alloc(input.length);
  new Uint8Array(ex.memory.buffer, p, input.length).set(input);
  t0 = performance.now();
  c0 = cpuNow();
  let rc;
  try {
    rc = ex.gpress_optimize(p, input.length, effort, 0, 0, 0, 0);
  } catch (e) {
    // A trap leaves the instance dead: note it and start a fresh one.
    rc = -1;
    row.trap = String(e);
  }
  row.cpu = cpuNow() - c0;
  row.wall = (performance.now() - t0) / 1000;
  cpu += row.cpu;
  wall += row.wall;
  if (rc === 0) {
    const out = new Uint8Array(ex.memory.buffer, ex.gpress_out_ptr(), ex.gpress_out_len());
    row.status = ex.gpress_outcome() === 1 ? "SMALLER" : "UNTOUCHED";
    row.bytes_out = out.length;
    row.sha256 = createHash("sha256").update(out).digest("hex");
    if (outDir) {
      const dest = join(outDir, rel);
      mkdirSync(dirname(dest), { recursive: true });
      writeFileSync(dest, out);
    }
  } else if (rc > 0) {
    row.status = "ERROR";
    row.code = rc;
  } else {
    row.status = "TRAP";
  }
  if (rc >= 0) {
    ex.gpress_clear_out();
    ex.gpress_free_input(p, input.length);
  } else {
    ex = await load();
  }
  rows.push(row);
}
const r3 = (x) => Math.round(x * 1000) / 1000;
console.log(
  JSON.stringify({
    module: modulePath,
    module_bytes: bytes.length,
    effort,
    files: rows.length,
    load_wall: r3(loadWall),
    load_cpu: r3(loadCpu),
    cpu: r3(cpu),
    wall: r3(wall),
    rows: rows.map((r) => ({ ...r, cpu: r3(r.cpu), wall: r3(r.wall) })),
  }),
);
