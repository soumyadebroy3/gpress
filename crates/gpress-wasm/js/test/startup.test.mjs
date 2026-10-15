// Start-up: from the module's bytes in memory to the first optimized PDF (compile, instantiate, the first
// optimize call of a one-page PDF of a few KB). Each sample is a fresh Node process, because V8 reuses
// compiled code within a process. Runs against dist/ after `npm run build`. bench/wasm_startup.mjs makes
// the same measurement for any module file, with more detail.
//
// The target is a median of at most 50 ms at effort 0, and the test prints the distance to it. No machine
// measured meets it yet (bench/RESULTS.md, Start-up): most of the first call runs while V8's optimizing
// compiler works in the background, so a machine with fewer cores, or a busy one, is much slower, and
// GitHub's two-core runner takes about twice the target. So the test fails only above 150 ms, which still
// catches a large step back; GPRESS_STARTUP_LIMIT_MS raises that limit (a smaller number is ignored). The
// default effort (100) does about one and a half times the work of effort 0 on this PDF and is held to
// twice the limit. Run the test files one at a time (`npm test` does), or the samples measure the other
// tests too.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { onePagePdf } from "./onepage.mjs";

const RUNS = 7;
const TARGET_MS = 50;
const LIMIT_MS = Math.max(150, Number(process.env.GPRESS_STARTUP_LIMIT_MS) || 0);
const wasm = new URL("../dist/gpress.wasm", import.meta.url);

/** One sample, in this (fresh) process: the times in ms and what came out. */
async function sample(effort) {
  const { Gpress } = await import("../dist/gpress.js");
  const bytes = readFileSync(wasm);
  const pdf = onePagePdf();
  const t0 = performance.now();
  const module = await Gpress.compile(bytes);
  const t1 = performance.now();
  const g = await Gpress.load(module);
  const t2 = performance.now();
  const r = g.optimize(pdf, effort === "default" ? {} : { effort: Number(effort) });
  const t3 = performance.now();
  let sum = 0;
  for (const b of r.data) sum = (sum * 31 + b) >>> 0;
  return { compile: t1 - t0, instantiate: t2 - t1, optimize: t3 - t2, total: t3 - t0, input: pdf.length, output: r.data.length, outcome: r.outcome, sum };
}

function median(v) {
  const s = [...v].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}

if (process.argv[2] === "--sample") {
  process.stdout.write(JSON.stringify(await sample(process.argv[3])) + "\n");
} else {
  const { test } = await import("node:test");
  const assert = (await import("node:assert/strict")).default;
  const { Gpress } = await import("../dist/gpress.js");
  const self = fileURLToPath(import.meta.url);

  test("one compiled module makes independent instances", async () => {
    const module = await Gpress.compile(wasm);
    assert.ok(module instanceof WebAssembly.Module);
    assert.equal(await Gpress.compile(module), module);
    const [a, b] = [await Gpress.load(module), await Gpress.load(module)];
    const fromBytes = await Gpress.load(readFileSync(wasm));
    const pdf = onePagePdf();
    const r = a.optimize(pdf, { effort: 0 });
    assert.equal(r.outcome, "smaller");
    assert.deepEqual(b.optimize(pdf, { effort: 0 }).data, r.data);
    assert.deepEqual(fromBytes.optimize(pdf, { effort: 0 }).data, r.data);
    // A dead instance (as after a trap) leaves the others working.
    a.dead = new Error("dead");
    assert.throws(() => a.optimize(pdf), /dead/);
    assert.deepEqual(b.optimize(pdf, { effort: 0 }).data, r.data);
  });

  // The target is for effort 0, the setting for a quick first result. The default effort is what a plain
  // optimize(pdf) runs; it is far from 50 ms (see the Start-up part of bench/RESULTS.md) but held to a
  // looser limit, so a large step back there fails too.
  for (const [effort, limit] of [["0", LIMIT_MS], ["default", 2 * LIMIT_MS]]) {
    test(`start-up at effort ${effort}: median of ${RUNS} fresh processes at most ${limit} ms`, (t) => {
      // One process first, not counted: on a fresh machine the first processes start slower (CI's did).
      execFileSync(process.execPath, [self, "--sample", effort], { encoding: "utf8" });
      const runs = [];
      for (let k = 0; k < RUNS; k++) {
        runs.push(JSON.parse(execFileSync(process.execPath, [self, "--sample", effort], { encoding: "utf8" })));
      }
      // The same output in every process, and smaller than the input.
      assert.equal(runs[0].outcome, "smaller");
      assert.ok(runs[0].output < runs[0].input);
      for (const r of runs) assert.deepEqual([r.output, r.sum], [runs[0].output, runs[0].sum]);
      const m = (k) => median(runs.map((r) => r[k]));
      t.diagnostic(
        `${runs[0].input} -> ${runs[0].output} bytes; median ms: compile ${m("compile").toFixed(1)}, instantiate ` +
          `${m("instantiate").toFixed(1)}, first optimize ${m("optimize").toFixed(1)}, total ${m("total").toFixed(1)} ` +
          `(runs: ${runs.map((r) => r.total.toFixed(0)).join(" ")})`,
      );
      if (effort === "0") {
        const over = m("total") - TARGET_MS;
        t.diagnostic(`target ${TARGET_MS} ms: ${over <= 0 ? "met" : `not met, ${over.toFixed(1)} ms over`}`);
      }
      assert.ok(m("total") <= limit, `start-up median ${m("total").toFixed(1)} ms is over ${limit} ms`);
    });
  }
}
