// The page's logic without a browser: `node --test web/test/*.test.mjs` from the repository root. The last
// test runs the real web module and needs the package built (crates/gpress-wasm/js: npm ci && npm run build);
// without the build it is skipped, except in CI, where it must run.
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { crc32 as zlibCrc32 } from "node:zlib";
import { LEVELS, DEFAULT_EFFORT, compress, crc32, explain, formatBytes, isPdf, outputName, savedPercent, targetBytes, zip } from "../logic.js";

const pdf = (length) => {
  const bytes = new Uint8Array(length);
  bytes.set(new TextEncoder().encode("%PDF-1.7\n"));
  return bytes;
};

/** An engine whose output size at each effort is given, recording the efforts it was asked for. */
function fake(sizes, reason) {
  const calls = [];
  return {
    calls,
    optimize(input, { effort, maxInputMiB }) {
      assert.equal(maxInputMiB, 1024);
      calls.push(effort);
      const size = sizes[effort];
      return size < input.length ? { data: pdf(size), outcome: "smaller" } : { data: input.slice(), outcome: "untouched", reason };
    },
  };
}

test("the level table: six levels, 0 to 40 lossless, 60 to 100 not, 40 the default", () => {
  assert.deepEqual(LEVELS.map((l) => l.effort), [0, 20, 40, 60, 80, 100]);
  assert.deepEqual(LEVELS.map((l) => l.lossless), [true, true, true, false, false, false]);
  assert.equal(DEFAULT_EFFORT, 40);
  for (const l of LEVELS) assert.ok(l.name && l.about && !l.about.includes("\n"));
});

test("one level: the effort goes to the engine as a number and the result comes back", () => {
  const g = fake({ 20: 600 });
  const r = compress(g, pdf(1000), { effort: 20 });
  assert.deepEqual(g.calls, [20]);
  assert.deepEqual({ outcome: r.outcome, effort: r.effort, size: r.data.length }, { outcome: "smaller", effort: 20, size: 600 });
  assert.equal("reached" in r, false);
});

test("an untouched file keeps the engine's reason and offers no data", () => {
  const r = compress(fake({ 40: 1000 }, "the file is signed"), pdf(1000), { effort: 40 });
  assert.deepEqual(r, { outcome: "untouched", effort: 40, reason: "the file is signed" });
});

test("a result that is not smaller than the input is never offered", () => {
  const same = { optimize: (input) => ({ data: input.slice(), outcome: "smaller" }) };
  assert.deepEqual(compress(same, pdf(1000), { effort: 0 }), { outcome: "untouched", effort: 0, reason: "no saving possible" });
});

test("a file that is not a PDF is refused without running the engine", () => {
  const g = fake({});
  assert.equal(compress(g, new TextEncoder().encode("PK\x03\x04 not a pdf"), { effort: 40 }).outcome, "refused");
  assert.equal(compress(g, new Uint8Array(0), { target: 10 }).outcome, "refused");
  assert.deepEqual(g.calls, []);
  assert.equal(isPdf(pdf(9)), true);
});

test("target size: the levels are tried from 0 upward and the first that fits is kept", () => {
  const sizes = { 0: 900, 20: 800, 40: 700, 60: 500, 80: 300, 100: 200 };
  const seen = [];
  let g = fake(sizes);
  let r = compress(g, pdf(1000), { target: 750 }, (effort) => seen.push(effort));
  assert.deepEqual([r.outcome, r.effort, r.reached, r.data.length], ["smaller", 40, true, 700]);
  assert.deepEqual(g.calls, [0, 20, 40]);
  assert.deepEqual(seen, [0, 20, 40]);

  g = fake(sizes);
  r = compress(g, pdf(1000), { target: 950, effort: 100 });
  assert.deepEqual([r.effort, r.reached, g.calls], [0, true, [0]]);
});

test("target size: when no level fits, the smallest result is kept and says it fell short", () => {
  let g = fake({ 0: 900, 20: 800, 40: 700, 60: 500, 80: 300, 100: 200 });
  let r = compress(g, pdf(1000), { target: 100 });
  assert.deepEqual([r.outcome, r.effort, r.reached, r.data.length], ["smaller", 100, false, 200]);
  assert.deepEqual(g.calls, [0, 20, 40, 60, 80, 100]);

  // Not every level is smaller than the one before: the smallest wins, the earlier one on a tie.
  g = fake({ 0: 900, 20: 600, 40: 650, 60: 600, 80: 990, 100: 700 });
  r = compress(g, pdf(1000), { target: 100 });
  assert.deepEqual([r.effort, r.reached, r.data.length], [20, false, 600]);

  // A file the engine leaves alone at every level.
  r = compress(fake({ 0: 1000, 20: 1000, 40: 1000, 60: 1000, 80: 1000, 100: 1000 }, "PDF/A"), pdf(1000), { target: 500 });
  assert.deepEqual(r, { outcome: "untouched", effort: 0, reached: false, reason: "PDF/A" });
});

test("target size: the module's own optimizeToSize is used when it has one", () => {
  const g = fake({});
  g.optimizeToSize = (input, target, options) => {
    assert.deepEqual([input.length, target, options], [1000, 400, { maxInputMiB: 1024 }]);
    return { data: pdf(380), outcome: "smaller", effort: 60, reached: true };
  };
  const r = compress(g, pdf(1000), { target: 400, effort: 0 });
  assert.deepEqual([r.outcome, r.effort, r.reached, r.data.length, g.calls], ["smaller", 60, true, 380, []]);
});

test("a target as a percentage or a size", () => {
  assert.equal(targetBytes(1000, 50, "%"), 500);
  assert.equal(targetBytes(999, 33.3, "%"), 332);
  assert.equal(targetBytes(1000, 2, "KB"), 2048);
  assert.equal(targetBytes(1000, 1.5, "MB"), 1572864);
  for (const bad of [0, -1, NaN, Infinity]) assert.equal(targetBytes(1000, bad, "%"), null);
});

test("sizes, savings and names as text", () => {
  assert.deepEqual([0, 812, 1024, 4301, 12900, 536870912, 1610612736, 5e12].map(formatBytes), ["0 B", "812 B", "1.00 KB", "4.20 KB", "12.6 KB", "512 MB", "1.50 GB", "4657 GB"]);
  assert.deepEqual([savedPercent(1000, 628), savedPercent(1000, 1000), savedPercent(100000, 99999), savedPercent(0, 0)], ["37.2%", "0.0%", "under 0.1%", "0.0%"]);
  assert.deepEqual(["report.pdf", "Scan.PDF", "notes", "a/b\\c:d.pdf", ".pdf"].map(outputName), ["report-compressed.pdf", "Scan-compressed.pdf", "notes-compressed.pdf", "a_b_c_d-compressed.pdf", "document-compressed.pdf"]);
});

test("errors are told in words a person can act on", () => {
  assert.match(explain({ code: 1, message: "input is 9 bytes, over the limit of 8" }), /too large/);
  assert.match(explain({ name: "RangeError", message: "Array buffer allocation failed" }), /too large/);
  assert.match(explain({ code: 4, message: "the module trapped (unreachable); load it again" }), /unchanged.*trapped/);
  assert.match(explain({ name: "NotReadableError", message: "x" }), /could not read/);
  assert.match(explain({ message: "something else" }), /something else/);
});

test("the zip for Download all: every file stored whole, with its CRC, under a name of its own", async () => {
  const data = [pdf(300), pdf(70000).fill(7, 9), pdf(9)];
  for (const d of data) assert.equal(crc32(d), zlibCrc32(d));
  const names = ["a-compressed.pdf", "a-compressed.pdf", "résumé-compressed.pdf"];
  const parts = zip(data.map((d, i) => ({ name: names[i], crc: crc32(d), size: d.length, data: i === 1 ? new Blob([d]) : d })), new Date(2026, 10, 14, 12, 30, 8));
  const bytes = new Uint8Array(await new Blob(parts).arrayBuffer());
  const view = new DataView(bytes.buffer);
  const end = bytes.length - 22;
  assert.equal(view.getUint32(end, true), 0x06054b50);
  assert.deepEqual([view.getUint16(end + 8, true), view.getUint16(end + 10, true)], [3, 3]);
  let at = view.getUint32(end + 16, true);
  assert.equal(at + view.getUint32(end + 12, true), end);
  const found = [];
  for (let i = 0; i < 3; i++) {
    assert.equal(view.getUint32(at, true), 0x02014b50);
    const [flags, method, time, day] = [8, 10, 12, 14].map((o) => view.getUint16(at + o, true));
    assert.deepEqual([flags, method, time, day], [0x0800, 0, (12 << 11) | (30 << 5) | 4, (46 << 9) | (11 << 5) | 14]);
    const [crc, stored, size] = [16, 20, 24].map((o) => view.getUint32(at + o, true));
    const nameLength = view.getUint16(at + 28, true);
    const name = new TextDecoder().decode(bytes.subarray(at + 46, at + 46 + nameLength));
    // The local header says the same, and the file's bytes follow it.
    const local = view.getUint32(at + 42, true);
    assert.equal(view.getUint32(local, true), 0x04034b50);
    assert.deepEqual(bytes.subarray(local + 4, local + 30), bytes.subarray(at + 6, at + 32));
    assert.equal(new TextDecoder().decode(bytes.subarray(local + 30, local + 30 + nameLength)), name);
    const body = bytes.subarray(local + 30 + nameLength, local + 30 + nameLength + size);
    assert.deepEqual([stored, size, crc], [data[i].length, data[i].length, zlibCrc32(body)]);
    assert.deepEqual(body, data[i]);
    found.push(name);
    at += 46 + nameLength;
  }
  assert.equal(at, end);
  assert.deepEqual(found, ["a-compressed.pdf", "a-compressed (2).pdf", "résumé-compressed.pdf"]);
});

test("a zip that would need zip64 is refused", () => {
  assert.equal(zip([{ name: "a.pdf", crc: 0, size: 0xffffffff, data: new Uint8Array(0) }]), null);
  assert.equal(zip(Array.from({ length: 0x10000 }, (_, i) => ({ name: `${i}.pdf`, crc: 0, size: 0, data: new Uint8Array(0) }))), null);
});

test("the page is light and asks no other site for anything", () => {
  const read = (name) => readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
  const files = ["index.html", "style.css", "app.js", "worker.js", "logic.js"].map(read);
  const total = files.reduce((n, text) => n + Buffer.byteLength(text), 0);
  assert.ok(total < 30000, `the page's own files are ${total} bytes, over 30 KB`);
  // The one address of another site is the link to the source, which a person follows by choice.
  const elsewhere = files.join("\n").match(/\b(?:https?:)?\/\/[\w.-]+\.[a-z]{2,}[^\s"'<>)]*/g) ?? [];
  assert.deepEqual(elsewhere.filter((url) => !url.includes("www.w3.org/2000/svg")), ["https://github.com/soumyadebroy3/gpress"]);
  const policy = files[0].match(/http-equiv="Content-Security-Policy" content="([^"]*)"/)[1];
  assert.match(policy, /default-src 'none'/);
  assert.match(policy, /connect-src 'self'(;|$)/);
  assert.doesNotMatch(policy, /unsafe-inline|unsafe-eval'|https?:|\*/);
});

const dist = new URL("../../crates/gpress-wasm/js/dist/", import.meta.url);
const built = existsSync(new URL("gpress-web.wasm", dist));
test("the real web module through the page's logic", { skip: !built && !process.env.CI && "the package is not built" }, async () => {
  const { Gpress } = await import(new URL("gpress.js", dist));
  const g = await Gpress.load(new URL("gpress-web.wasm", dist));
  const input = new Uint8Array(readFileSync(new URL("../../tests/data/format-v9.pdf", import.meta.url)));
  for (const { effort } of LEVELS) {
    const r = compress(g, input, { effort });
    assert.equal(r.outcome, "smaller");
    assert.ok(isPdf(r.data) && r.data.length < input.length);
    assert.deepEqual(r.data, g.optimize(input, { effort }).data);
  }
  const smallest = Math.min(...LEVELS.map(({ effort }) => g.optimize(input, { effort }).data.length));
  const hit = compress(g, input, { target: input.length - 1 });
  assert.deepEqual([hit.outcome, hit.effort, hit.reached], ["smaller", 0, true]);
  const miss = compress(g, input, { target: 10 });
  assert.deepEqual([miss.outcome, miss.reached, miss.data.length], ["smaller", false, smallest]);
  assert.equal(compress(g, new TextEncoder().encode("%PDF-1.4\nnot really"), { effort: 0 }).outcome, "untouched");
});
