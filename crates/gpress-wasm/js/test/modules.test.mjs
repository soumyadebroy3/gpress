// The package's two modules side by side, after `npm run build`: the web module (dist/gpress-web.wasm) leaves
// out the archive and nothing optimize uses, so it gives the same bytes as the whole module
// (dist/gpress.wasm), and its archive calls throw GpressFeatureError. Runs once, in the plain `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { deflateSync } from "node:zlib";
import { Gpress, GpressFeatureError } from "../dist/gpress.js";
import { onePagePdf } from "./onepage.mjs";

const dist = new URL("../dist/", import.meta.url);

test("the web module optimizes to the same bytes as the whole module", { skip: !!process.env.GPRESS_WASM }, async () => {
  const whole = await Gpress.load(new URL("gpress.wasm", dist));
  const web = await Gpress.load(new URL("gpress-web.wasm", dist));
  assert.ok(whole.features.includes("archive") && !web.features.includes("archive"));
  const enc = new TextEncoder();
  const text = Array.from({ length: 300 }, (_, i) => `BT /F1 12 Tf 72 ${700 - (i % 60) * 11} Td (Line ${i}) Tj ET\n`).join("");
  const content = deflateSync(enc.encode(text), { level: 1 });
  const head = enc.encode(`%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n2 0 obj\n<< /Filter /FlateDecode /Length ${content.length} >>\nstream\n`);
  const tail = enc.encode("\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
  const page = new Uint8Array([...head, ...content, ...tail]);
  for (const pdf of [onePagePdf(), page, enc.encode("not a PDF")]) {
    for (const effort of [0, 50, 100]) {
      const a = whole.optimize(pdf, { effort });
      const b = web.optimize(pdf, { effort });
      assert.equal(b.outcome, a.outcome);
      assert.deepEqual(b.data, a.data, `effort ${effort}`);
    }
  }
  const archive = whole.compress(page).data;
  assert.deepEqual(whole.expand(archive), page);
  assert.throws(() => web.compress(page), (e) => e instanceof GpressFeatureError && e.feature === "archive");
  assert.throws(() => web.expand(archive), (e) => e instanceof GpressFeatureError && e.feature === "archive");
});
