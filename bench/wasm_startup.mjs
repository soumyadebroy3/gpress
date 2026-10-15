// Start-up of the WebAssembly module: from its bytes in memory to the first optimized PDF, each sample a
// fresh Node process (V8 reuses compiled code within a process). A sample times compile, instantiate, the
// first optimize call of a one-page PDF of 3.8 KB (crates/gpress-wasm/js/test/onepage.mjs, or the files
// given with --pdf) and a second call in the same process (the work alone, once the code is compiled). The
// modules, files and efforts are measured in turn, round after round, so a busy machine slows them all
// alike; the table gives medians.
//
//   node bench/wasm_startup.mjs [--runs 9] [--efforts 0,100] [--flags "--no-liftoff"] [--pdf file.pdf ...]
//                               [--quiet 0-2 [--max-busy 10]] LABEL=module.wasm ...
//   node bench/wasm_startup.mjs --sample module.wasm EFFORT [file.pdf]     -> one JSON line
//
// The module is called through its plain exports (no wrapper), so any build of gpress-wasm can be measured.
// Node flags given with --flags go to every sample process (for example --cpu-prof to see where the time goes).
// --quiet (Linux) keeps a round only when the listed cores were idle, other work under --max-busy percent
// of them, for a second before the round and a second after it; a dropped round is measured again. Use it
// on a shared machine, with the samples run on those cores (taskset -c 0-2 node bench/wasm_startup.mjs
// --quiet 0-2 ...).
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { fileURLToPath } from "node:url";
import { onePagePdf } from "../crates/gpress-wasm/js/test/onepage.mjs";

function optimize(ex, input, effort) {
  const ptr = ex.gpress_alloc(input.length);
  new Uint8Array(ex.memory.buffer, ptr, input.length).set(input);
  const code = ex.gpress_optimize(ptr, input.length, effort, 0, 0, 0, 0);
  if (code !== 0) throw new Error(`gpress_optimize returned ${code}`);
  const out = new Uint8Array(ex.memory.buffer, ex.gpress_out_ptr(), ex.gpress_out_len()).slice();
  ex.gpress_free_input(ptr, input.length);
  ex.gpress_clear_out();
  return out;
}

async function sample(path, effort, pdfPath) {
  const bytes = readFileSync(path);
  const pdf = pdfPath ? new Uint8Array(readFileSync(pdfPath)) : onePagePdf();
  const t0 = performance.now();
  const module = await WebAssembly.compile(bytes);
  const t1 = performance.now();
  const instance = await WebAssembly.instantiate(module, {});
  const t2 = performance.now();
  const first = optimize(instance.exports, pdf, effort);
  const t3 = performance.now();
  const second = optimize(instance.exports, pdf, effort);
  const t4 = performance.now();
  if (Buffer.compare(first, second) !== 0) throw new Error("two calls gave different bytes");
  let sum = 0;
  for (const b of first) sum = (sum * 31 + b) >>> 0;
  return { compile: t1 - t0, instantiate: t2 - t1, first: t3 - t2, second: t4 - t3, total: t3 - t0, input: pdf.length, output: first.length, sum, memory: instance.exports.memory.buffer.byteLength };
}

const median = (v) => {
  const s = [...v].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};

/** The listed cores' busy and total time so far, from /proc/stat ("0-2,5" style list). */
function cpuTimes(cores) {
  let busy = 0;
  let total = 0;
  for (const line of readFileSync("/proc/stat", "utf8").split("\n")) {
    const m = line.match(/^cpu(\d+) (.*)/);
    if (!m || !cores.has(Number(m[1]))) continue;
    // user nice system idle iowait irq softirq steal: steal is time the host gave to others, so busy too.
    const [user, nice, system, idle, iowait, irq, softirq, steal] = m[2].trim().split(/\s+/).map(Number);
    busy += user + nice + system + irq + softirq + steal;
    total += user + nice + system + idle + iowait + irq + softirq + steal;
  }
  return [busy, total];
}

/** The share of the listed cores (percent) other work took over the next `ms` (this process sleeps). */
async function busyShare(cores, ms) {
  const a = cpuTimes(cores);
  await new Promise((r) => setTimeout(r, ms));
  const b = cpuTimes(cores);
  return (100 * (b[0] - a[0])) / Math.max(1, b[1] - a[1]);
}

function coreList(s) {
  const cores = new Set();
  for (const part of s.split(",")) {
    const [lo, hi = lo] = part.split("-").map(Number);
    for (let c = lo; c <= hi; c++) cores.add(c);
  }
  return cores;
}

const args = process.argv.slice(2);
if (args[0] === "--sample") {
  process.stdout.write(JSON.stringify(await sample(args[1], Number(args[2]), args[3])) + "\n");
} else {
  let runs = 9;
  let efforts = [0, 100];
  let flags = [];
  let quiet = null;
  let maxBusy = 10;
  const pdfs = [];
  const modules = [];
  for (let k = 0; k < args.length; k++) {
    if (args[k] === "--runs") runs = Number(args[++k]);
    else if (args[k] === "--efforts") efforts = args[++k].split(",").map(Number);
    else if (args[k] === "--flags") flags = args[++k].split(/\s+/).filter(Boolean);
    else if (args[k] === "--pdf") pdfs.push(args[++k]);
    else if (args[k] === "--quiet") quiet = coreList(args[++k]);
    else if (args[k] === "--max-busy") maxBusy = Number(args[++k]);
    else {
      const eq = args[k].indexOf("=");
      modules.push(eq > 0 ? [args[k].slice(0, eq), args[k].slice(eq + 1)] : [args[k], args[k]]);
    }
  }
  if (modules.length === 0) {
    console.error(
      'usage: node bench/wasm_startup.mjs [--runs N] [--efforts 0,100] [--flags "..."] [--pdf file.pdf ...] [--quiet 0-2 [--max-busy 10]] LABEL=module.wasm ...',
    );
    process.exit(2);
  }
  const self = fileURLToPath(import.meta.url);
  const inputs = pdfs.length ? pdfs.map((p) => [basename(p), p]) : [[null, null]];
  const rows = new Map();
  let dropped = 0;
  for (let r = 0; r < runs; ) {
    if (quiet) {
      while ((await busyShare(quiet, 1000)) > maxBusy) {}
    }
    const round = [];
    for (const [label, path] of modules) {
      for (const [name, pdf] of inputs) {
        for (const e of efforts) {
          const out = execFileSync(process.execPath, [...flags, self, "--sample", path, String(e), ...(pdf ? [pdf] : [])], { encoding: "utf8" });
          round.push([`${label}${name ? `, ${name}` : ""}, effort ${e}`, path, JSON.parse(out.trim().split("\n").pop())]);
        }
      }
    }
    if (quiet && (await busyShare(quiet, 1000)) > maxBusy) {
      dropped++;
      console.error(`round ${r + 1}: the cores were busy after it, measuring it again`);
      continue;
    }
    for (const [key, path, s] of round) {
      if (!rows.has(key)) rows.set(key, { path, samples: [] });
      rows.get(key).samples.push(s);
    }
    r++;
  }
  const f = (v) => v.toFixed(1);
  const what = pdfs.length ? "Inputs as named" : `Input ${onePagePdf().length} bytes`;
  const how = quiet ? `; only rounds with cores ${[...quiet].join(",")} idle (other work under ${maxBusy}%), ${dropped} dropped` : "";
  console.log(`Fresh Node ${process.version} processes, ${runs} per row${how}; medians in ms. ${what}.`);
  console.log("");
  console.log("| Module, effort | Module bytes | Compile | Instantiate | First call | Total | Total min..max | Second call | Input bytes | Output bytes |");
  console.log("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
  for (const [key, { path, samples }] of rows) {
    const m = (k) => median(samples.map((s) => s[k]));
    const totals = samples.map((s) => s.total);
    const same = samples.every((s) => s.output === samples[0].output && s.sum === samples[0].sum);
    console.log(
      `| ${key} | ${readFileSync(path).length.toLocaleString("en")} | ${f(m("compile"))} | ${f(m("instantiate"))} | ${f(m("first"))} | ` +
        `**${f(m("total"))}** | ${f(Math.min(...totals))}..${f(Math.max(...totals))} | ${f(m("second"))} | ` +
        `${samples[0].input.toLocaleString("en")} | ${samples[0].output.toLocaleString("en")}${same ? "" : " (differs between runs!)"} |`,
    );
  }
}
