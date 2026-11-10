// Runs against dist/gpress.js and the module test/module.mjs picks (dist/gpress.wasm, or
// dist/gpress-web.wasm with GPRESS_WASM) after `npm run build`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { deflateSync as zlibSync } from "node:zlib";
import { Gpress, GpressError, GpressFeatureError } from "../dist/gpress.js";
import { modulePath } from "./module.mjs";

const g = await Gpress.load(modulePath);
// The web module has no archive: there every archive test checks that compress and expand say so instead.
const archive = g.features.includes("archive");

function archiveTest(name, body) {
  test(archive ? name : `${name}: without the archive, compress and expand say they need it`, () => {
    if (archive) return body();
    for (const call of [() => g.compress(new Uint8Array(10)), () => g.expand(new Uint8Array(10))]) {
      assert.throws(call, (e) => e instanceof GpressFeatureError && e instanceof GpressError &&
        e.code === GpressError.NEEDS_FEATURE && e.feature === "archive" && e.message.includes("`archive`"));
    }
  });
}

test("version is set", () => {
  assert.match(g.version, /^\d+\.\d+\.\d+$/);
  assert.equal(g.archiveVersion, archive ? 9 : 0);
});

test("the module lists its parts: every one, or the web set without the archive", () => {
  const web = modulePath.endsWith("gpress-web.wasm");
  assert.deepEqual(g.features, web ? ["jpeg", "fax", "encryption"] : ["archive", "jpeg", "fax", "encryption"]);
});

archiveTest("compress and expand restore any bytes exactly", () => {
  for (const input of [new Uint8Array(0), new TextEncoder().encode("%PDF-1.7\n%%EOF\n"), Uint8Array.from({ length: 5000 }, (_, i) => (i * 7919) % 251)]) {
    const c = g.compress(input);
    assert.ok(Gpress.isArchive(c.data));
    assert.deepEqual(g.expand(c.data), input);
  }
});

archiveTest("a damaged archive is refused, never a wrong file", () => {
  assert.throws(() => g.expand(new TextEncoder().encode("not an archive")), (e) => e instanceof GpressError && e.code === GpressError.DAMAGED);
  // Every single flipped bit is either refused or changes nothing that is restored (some bits of the coded
  // body are padding): never a wrong file. A flip in the magic is always refused.
  const input = new TextEncoder().encode("hello hello hello hello");
  const c = g.compress(input);
  let refused = 0;
  for (let i = 0; i < c.data.length; i++) {
    for (const bit of [0x01, 0x40]) {
      const bad = c.data.slice();
      bad[i] ^= bit;
      try {
        assert.deepEqual(g.expand(bad), input, `flip ${bit} at ${i} restored a wrong file`);
      } catch (e) {
        if (!(e instanceof GpressError)) throw e;
        refused++;
      }
    }
  }
  assert.ok(refused > 0);
  const magic = c.data.slice();
  magic[0] ^= 0x01;
  assert.throws(() => g.expand(magic), (e) => e instanceof GpressError && e.code === GpressError.DAMAGED);
  const other = c.data.slice();
  other[4] = 7;
  assert.throws(() => g.expand(other), (e) => e instanceof GpressError && e.code === GpressError.UNSUPPORTED_VERSION);
});

archiveTest("the decoded-bytes budget is the caller's on both sides", () => {
  // 3 MiB of zeros in one zlib stream: within a 4 MiB budget the stream is decoded and modelled; with a 1 MiB
  // budget it is stored as it is (still restored exactly), so the archives differ; and an archive made with the
  // larger budget is refused under the smaller one before anything is decoded. (Which archive is smaller depends
  // on the model: here the 3 KB of zlib bytes modelled as they are come out smaller than the zeros.)
  const zeros = new Uint8Array(3 << 20);
  const z = zlibSync(zeros);
  const head = new TextEncoder().encode(`%PDF-1.4\n1 0 obj\n<< /Filter /FlateDecode /Length ${z.length} >>\nstream\n`);
  const tail = new TextEncoder().encode("\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
  const file = new Uint8Array(head.length + z.length + tail.length);
  file.set(head, 0); file.set(z, head.length); file.set(tail, head.length + z.length);
  const wide = g.compress(file, { maxDecodedMiB: 4 });
  const narrow = g.compress(file, { maxDecodedMiB: 1 });
  assert.notDeepEqual(wide.data, narrow.data);
  assert.deepEqual(g.expand(wide.data, { maxDecodedMiB: 4 }), file);
  assert.deepEqual(g.expand(narrow.data, { maxDecodedMiB: 1 }), file);
  assert.throws(() => g.expand(wide.data, { maxDecodedMiB: 1 }), (e) => e instanceof GpressError && e.code === GpressError.DECODED_TOO_LARGE);
});

archiveTest("a /Length that wraps to an earlier endstream on 32 bits is not trusted", () => {
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

archiveTest("an image row past 2^32 bits packs as on 64 bits", () => {
  // 32 DeviceN components of 16 bits, 8,388,609 pixels wide: width x components x bits is 2^32 + 512, which the
  // 32-bit module's row length once wrapped to 512, so it took 100 rows of 64 bytes and wrote an archive the
  // native library refuses. The stream is zlib's level 9 of 6,400 bytes (i * 7919 % 251); `native` is the
  // native library's archive of the file.
  const enc = new TextEncoder();
  const z = Buffer.from(
    "eNpj6JJcbLTH+2rKu1qeaarr7U6EPyz41SmxyHC315XktzXcU1XW2R4Pe5D/s0N8ocEuz8tJb6q5piivtTkWej/vR7vYAv2dHpcSX1dx" +
    "TlZaY3005F7u9zbR+Xo73C8mvKrkmKS42upI8N2cb60i83S3u12If1nBPlFhleXhoDvZX1uE5+pscz0f96KcbYL8SotDgbezvjQLzdHe" +
    "6nIu9nkZa7/cCvODAbcyPzcJztba4nw25lkpS5/scrMD/jczPjUKzNLc7HQm+mkJc6/MMtP9fjfSPzbwz9TY5Hg66kkxU4/0UpN9vtfT" +
    "PtTzzVDf6HAq8nERY7fUEuO9PtdS39fxTlfbYH8y4lEhw6jXR70+6vVRr496fdTro14f9fqo10e9Pur1Ua+Pen3U66NeH/X6qNdHvT7q" +
    "9VGvj3p91OujXh+eXgcA3Qs2Zg==",
    "base64");
  const native = new Uint8Array(Buffer.from(
    "iUdQWgnMBYAyBENBpi9DdDhPGJOUIHBd+//eTVOxq/EJd/9f/wS+POojRQACS1LH6qwBe9BGlfeZUU2Bs8EKFzA/CrpDYbUhhAOFpf2q" +
    "xMpxbc7zQTcNvDM2jKqK5aNN467QH9gAOuy86jOpKvZy9q8BC1MKumN13L9FtpiuPLnotI16hkf6fHRLwqWQjJoWe95/MQFKXfb2GtD2" +
    "XO37+A3ZLMClVSN9jn3cIQQotqH6woOaNCUEWb0z41QEP2PnFxO58gJYi0rqMz1t58u9IiN9CgVbrYsqWo/8EZFw9B4UiwQCdGgFky3G" +
    "D1g1/Igz8UOOtaGN08wDBzEF4jDtj+PxLfUAj9586jnQEH01jEHZ3q0A",
    "base64"));
  const names = Array.from({ length: 32 }, (_, i) => `/C${i}`).join(" ");
  const head = enc.encode("%PDF-1.4\n1 0 obj\n<< /Type /XObject /Subtype /Image /Width 8388609 /Height 100 " +
    `/ColorSpace [/DeviceN [${names}] /DeviceGray 2 0 R] /BitsPerComponent 16 /Filter /FlateDecode ` +
    `/Length ${z.length} >>\nstream\n`);
  const tail = enc.encode("\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
  const pdf = new Uint8Array(head.length + z.length + tail.length);
  pdf.set(head, 0); pdf.set(z, head.length); pdf.set(tail, head.length + z.length);
  assert.deepEqual(g.compress(pdf).data, native);
  assert.deepEqual(g.expand(native), pdf);
});

test("optimize makes a page smaller at every effort, the fast path too", () => {
  // The fast encoder (efforts below 20) once trapped in this 32-bit module: an empty hash table entry passed
  // for a match. A page with a content stream stored by zlib's fastest level leaves room to gain.
  const enc = new TextEncoder();
  let text = "";
  for (let i = 0; i < 400; i++) text += `BT /F1 12 Tf 72 ${700 - (i % 60) * 11} Td (Line ${i} of the page) Tj ET\n`;
  const content = zlibSync(enc.encode(text), { level: 1 });
  const objs = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
  ];
  const parts = [enc.encode("%PDF-1.4\n")];
  const offsets = [];
  let at = parts[0].length;
  const add = (bytes) => { parts.push(bytes); at += bytes.length; };
  objs.forEach((o, i) => { offsets.push(at); add(enc.encode(`${i + 1} 0 obj\n${o}\nendobj\n`)); });
  offsets.push(at);
  add(enc.encode(`4 0 obj\n<< /Filter /FlateDecode /Length ${content.length} >>\nstream\n`));
  add(content);
  add(enc.encode("\nendstream\nendobj\n"));
  const xref = at;
  add(enc.encode(`xref\n0 5\n0000000000 65535 f \n${offsets.map((o) => `${String(o).padStart(10, "0")} 00000 n \n`).join("")}`));
  add(enc.encode(`trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`));
  const pdf = new Uint8Array(at);
  let k = 0;
  for (const p of parts) { pdf.set(p, k); k += p.length; }
  for (const effort of [0, 10, 100]) {
    const r = g.optimize(pdf, { effort });
    assert.equal(r.outcome, "smaller", `effort ${effort}: ${r.reason}`);
    assert.ok(r.data.length < pdf.length);
  }
});

test("optimize returns untouched with a reason for non-PDF input", () => {
  const r = g.optimize(new TextEncoder().encode("hello"), { jbig2: false, jpegTables: false, objectsPerStream: 50 });
  assert.equal(r.outcome, "untouched");
  assert.ok(r.reason && r.reason.length > 0);
});

archiveTest("the limits are the caller's", () => {
  const big = new Uint8Array(3 << 20);
  assert.throws(() => g.compress(big, { maxInputMiB: 2 }), (e) => e instanceof GpressError && e.code === GpressError.TOO_LARGE);
  const c = g.compress(big);
  assert.throws(() => g.expand(c.data, { maxLenMiB: 1 }), (e) => e instanceof GpressError && e.code === GpressError.DECLARED_TOO_LARGE);
  assert.deepEqual(g.expand(c.data, { maxLenMiB: 3 }), big);
});
