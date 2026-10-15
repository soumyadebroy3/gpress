// Start-up of the WebAssembly module: from its bytes in memory to the first optimized PDF, each sample a
// fresh Node process (V8 reuses compiled code within a process). A sample times compile, instantiate, the
// first optimize call of a one-page PDF of 3.8 KB (crates/gpress-wasm/js/test/onepage.mjs) and a second
// call in the same process (the work alone, once the code is compiled). The modules and efforts are
// measured in turn, round after round, so a busy machine slows them all alike; the table gives medians.
//
//   node bench/wasm_startup.mjs [--runs 9] [--efforts 0,100] [--flags "--no-liftoff"] LABEL=module.wasm ...
//   node bench/wasm_startup.mjs --sample module.wasm EFFORT     -> one JSON line
//
// The module is called through its plain exports (no wrapper), so any build of gpress-wasm can be measured.
// Node flags given with --flags go to every sample process (for example --cpu-prof to see where the time goes).
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
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

async function sample(path, effort) {
  const bytes = readFileSync(path);
  const pdf = onePagePdf();
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

const args = process.argv.slice(2);
if (args[0] === "--sample") {
  process.stdout.write(JSON.stringify(await sample(args[1], Number(args[2]))) + "\n");
} else {
  let runs = 9;
  let efforts = [0, 100];
  let flags = [];
  const modules = [];
  for (let k = 0; k < args.length; k++) {
    if (args[k] === "--runs") runs = Number(args[++k]);
    else if (args[k] === "--efforts") efforts = args[++k].split(",").map(Number);
    else if (args[k] === "--flags") flags = args[++k].split(/\s+/).filter(Boolean);
    else {
      const eq = args[k].indexOf("=");
      modules.push(eq > 0 ? [args[k].slice(0, eq), args[k].slice(eq + 1)] : [args[k], args[k]]);
    }
  }
  if (modules.length === 0) {
    console.error("usage: node bench/wasm_startup.mjs [--runs N] [--efforts 0,100] [--flags \"...\"] LABEL=module.wasm ...");
    process.exit(2);
  }
  const self = new URL(import.meta.url).pathname;
  const rows = new Map();
  for (let r = 0; r < runs; r++) {
    for (const [label, path] of modules) {
      for (const e of efforts) {
        const out = execFileSync(process.execPath, [...flags, self, "--sample", path, String(e)], { encoding: "utf8" });
        const key = `${label}, effort ${e}`;
        if (!rows.has(key)) rows.set(key, { path, samples: [] });
        rows.get(key).samples.push(JSON.parse(out.trim().split("\n").pop()));
      }
    }
  }
  const f = (v) => v.toFixed(1);
  console.log(`Fresh Node ${process.version} processes, ${runs} per row; medians in ms. Input ${onePagePdf().length} bytes.`);
  console.log("");
  console.log("| Module, effort | Module bytes | Compile | Instantiate | First call | Total | Total min..max | Second call | Output bytes |");
  console.log("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
  for (const [key, { path, samples }] of rows) {
    const m = (k) => median(samples.map((s) => s[k]));
    const totals = samples.map((s) => s.total);
    const same = samples.every((s) => s.output === samples[0].output && s.sum === samples[0].sum);
    console.log(
      `| ${key} | ${readFileSync(path).length.toLocaleString("en")} | ${f(m("compile"))} | ${f(m("instantiate"))} | ${f(m("first"))} | ` +
        `**${f(m("total"))}** | ${f(Math.min(...totals))}..${f(Math.max(...totals))} | ${f(m("second"))} | ` +
        `${samples[0].output.toLocaleString("en")}${same ? "" : " (differs between runs!)"} |`,
    );
  }
}
