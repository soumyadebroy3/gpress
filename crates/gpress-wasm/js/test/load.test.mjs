// Gpress.compile and Gpress.load take the module from every kind of source the Source type names. Runs
// against dist/ after `npm run build`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { Gpress } from "../dist/gpress.js";

const url = new URL("../dist/gpress.wasm", import.meta.url);
const wasm = readFileSync(url);
const pdf = new TextEncoder().encode("%PDF-1.4\n1 0 obj\n<< >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");

async function works(source) {
  const g = await Gpress.load(source);
  assert.match(g.version, /^\d+\.\d+\.\d+$/);
  g.optimize(pdf, { effort: 0 });
}

test("the module loads from bytes of every kind", async () => {
  const at = new Uint8Array(wasm.length + 40);
  at.set(wasm, 37);
  const shared = new SharedArrayBuffer(wasm.length);
  new Uint8Array(shared).set(wasm);
  const own = at.buffer.slice(37, 37 + wasm.length);
  await works(own);
  await works(at.subarray(37, 37 + wasm.length));
  await works(new DataView(at.buffer, 37, wasm.length));
  await works(new Uint8Array(shared));
  await works(shared);
  // An ArrayBuffer and a Module made in another realm (as in an iframe).
  const other = vm.runInNewContext("(b) => { const a = new ArrayBuffer(b.length); new Uint8Array(a).set(b); return a; }")(wasm);
  assert.equal(other instanceof ArrayBuffer, false);
  await works(other);
  const otherModule = await vm.runInNewContext("(b) => WebAssembly.compile(b)")(new Uint8Array(wasm));
  assert.equal(otherModule instanceof WebAssembly.Module, false);
  assert.equal(await Gpress.compile(otherModule), otherModule);
  await works(otherModule);
});

test("the module loads from a file and from a Response", async () => {
  await works(fileURLToPath(url));
  await works(url);
  for (const type of ["application/wasm", "application/wasm; charset=utf-8", "Application/Wasm", "application/octet-stream"]) {
    await works(new Response(wasm, { headers: { "content-type": type } }));
  }
  await works(Promise.resolve(new Response(wasm)));
  await assert.rejects(Gpress.load(new Response("nope", { status: 404 })), /HTTP 404/);
});
