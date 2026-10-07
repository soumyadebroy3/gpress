// Runs against dist/gpress.wasm and dist/gpress.js after `npm run build`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { Gpress, GpressError } from "../dist/gpress.js";

const g = await Gpress.load(new URL("../dist/gpress.wasm", import.meta.url).pathname);

test("version is set", () => assert.match(g.version, /^\d+\.\d+\.\d+$/));

test("compress and expand restore any bytes exactly", () => {
  for (const input of [new Uint8Array(0), new TextEncoder().encode("%PDF-1.7\n%%EOF\n"), Uint8Array.from({ length: 5000 }, (_, i) => (i * 7919) % 251)]) {
    const c = g.compress(input);
    assert.ok(Gpress.isArchive(c.data));
    assert.deepEqual(g.expand(c.data), input);
  }
});

test("a damaged archive is refused, never a wrong file", () => {
  assert.throws(() => g.expand(new TextEncoder().encode("not an archive")), (e) => e instanceof GpressError && e.code === GpressError.DAMAGED);
  const c = g.compress(new TextEncoder().encode("hello hello hello hello"));
  const bad = c.data.slice();
  bad[bad.length - 3] ^= 0x40;
  assert.throws(() => g.expand(bad), GpressError);
});

test("optimize returns untouched with a reason for non-PDF input", () => {
  const r = g.optimize(new TextEncoder().encode("hello"));
  assert.equal(r.outcome, "untouched");
  assert.ok(r.reason && r.reason.length > 0);
});
