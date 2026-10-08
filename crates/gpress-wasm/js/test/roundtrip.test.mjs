// Runs against dist/gpress.wasm and dist/gpress.js after `npm run build`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { deflateSync as zlibSync } from "node:zlib";
import { Gpress, GpressError } from "../dist/gpress.js";

const g = await Gpress.load(new URL("../dist/gpress.wasm", import.meta.url).pathname);

test("version is set", () => {
  assert.match(g.version, /^\d+\.\d+\.\d+$/);
  assert.equal(g.archiveVersion, 8);
});

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
  const other = c.data.slice();
  other[4] = 7;
  assert.throws(() => g.expand(other), (e) => e instanceof GpressError && e.code === GpressError.UNSUPPORTED_VERSION);
});

test("the decoded-bytes budget is the caller's on both sides", () => {
  // 3 MiB of zeros in one zlib stream: within a 4 MiB budget the stream is modelled and the archive is tiny;
  // with a 1 MiB budget it is stored as it is (still restored exactly), and an archive made with the larger
  // budget is refused under the smaller one before anything is decoded.
  const zeros = new Uint8Array(3 << 20);
  const z = zlibSync(zeros);
  const head = new TextEncoder().encode(`%PDF-1.4\n1 0 obj\n<< /Filter /FlateDecode /Length ${z.length} >>\nstream\n`);
  const tail = new TextEncoder().encode("\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
  const file = new Uint8Array(head.length + z.length + tail.length);
  file.set(head, 0); file.set(z, head.length); file.set(tail, head.length + z.length);
  const wide = g.compress(file, { maxDecodedMiB: 4 });
  const narrow = g.compress(file, { maxDecodedMiB: 1 });
  assert.ok(wide.data.length < narrow.data.length);
  assert.deepEqual(g.expand(wide.data, { maxDecodedMiB: 4 }), file);
  assert.deepEqual(g.expand(narrow.data, { maxDecodedMiB: 1 }), file);
  assert.throws(() => g.expand(wide.data, { maxDecodedMiB: 1 }), (e) => e instanceof GpressError && e.code === GpressError.DECODED_TOO_LARGE);
});

test("a /Length that wraps to an earlier endstream on 32 bits is not trusted", () => {
  // In the 32-bit module start + length would wrap to the offset of the early "endstream" unless the sum
  // is checked; the file must pack and restore exactly, like on 64 bits.
  const enc = new TextEncoder();
  const head = "%PDF-1.4\n% endstream before the data\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
  const early = head.indexOf("endstream");
  const payload = "BT /F1 12 Tf (not a zlib stream) Tj ET";
  const guess = `2 0 obj\n<< /Length 4294967296 >>\nstream\n`;
  const start = head.length + guess.length;
  const length = 2 ** 32 - start + early;
  const obj = `2 0 obj\n<< /Length ${length} >>\nstream\n`;
  assert.equal(obj.length, guess.length);
  const pdf = enc.encode(head + obj + payload + "\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
  const c = g.compress(pdf);
  assert.deepEqual(g.expand(c.data), pdf);
});

test("optimize returns untouched with a reason for non-PDF input", () => {
  const r = g.optimize(new TextEncoder().encode("hello"), { jbig2: false, jpegTables: false, objectsPerStream: 50 });
  assert.equal(r.outcome, "untouched");
  assert.ok(r.reason && r.reason.length > 0);
});

test("the limits are the caller's", () => {
  const big = new Uint8Array(3 << 20);
  assert.throws(() => g.compress(big, { maxInputMiB: 2 }), (e) => e instanceof GpressError && e.code === GpressError.TOO_LARGE);
  const c = g.compress(big);
  assert.throws(() => g.expand(c.data, { maxLenMiB: 1 }), (e) => e instanceof GpressError && e.code === GpressError.DECLARED_TOO_LARGE);
  assert.deepEqual(g.expand(c.data, { maxLenMiB: 3 }), big);
});
