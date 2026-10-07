// pdf.js (the engine of Firefox's PDF viewer) looking at one PDF: page count, and for every page (up to 30)
// a hash of its pixels and of its text. Used by verify_render.py, which compares original and output.
//
//   PDFJS_MODULES=/path/to/node_modules node verify_pdfjs.mjs FILE   -> one JSON line
//   (npm install pdfjs-dist @napi-rs/canvas  in a separate folder; the engine itself has no dependencies)
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const mods = process.env.PDFJS_MODULES;
const pdfjs = await import(pathToFileURL(path.join(mods, 'pdfjs-dist/legacy/build/pdf.mjs')).href);
const dist = path.join(mods, 'pdfjs-dist') + '/';
const PAGES = 30;
const sha = (x) => createHash('sha256').update(x).digest('hex');

async function look(file) {
  const res = { count: null, pages: [], text: [], error: null };
  let doc, task;
  try {
    task = pdfjs.getDocument({
      data: new Uint8Array(readFileSync(file)),
      isEvalSupported: false,
      disableFontFace: true,
      verbosity: 0,
      standardFontDataUrl: dist + 'standard_fonts/',
      cMapUrl: dist + 'cmaps/',
      cMapPacked: true,
      wasmUrl: dist + 'wasm/',
      iccUrl: dist + 'iccs/',
    });
    doc = await task.promise;
  } catch (e) {
    res.error = 'open: ' + (e && e.name);
    return res;
  }
  res.count = doc.numPages;
  for (let i = 1; i <= Math.min(doc.numPages, PAGES); i++) {
    try {
      const page = await doc.getPage(i);
      const v1 = page.getViewport({ scale: 1 });
      // Same size rule for both files: the longer side at most 800 pixels.
      const scale = Math.min(1, 800 / Math.max(v1.width, v1.height, 1));
      const vp = page.getViewport({ scale });
      const cc = doc.canvasFactory.create(Math.max(1, Math.ceil(vp.width)), Math.max(1, Math.ceil(vp.height)));
      await page.render({ canvas: cc.canvas, canvasContext: cc.context, viewport: vp }).promise;
      const img = cc.context.getImageData(0, 0, cc.canvas.width, cc.canvas.height);
      res.pages.push(sha(Buffer.from(img.data.buffer)));
      doc.canvasFactory.destroy(cc);
      const tc = await page.getTextContent();
      res.text.push(sha(JSON.stringify(tc.items.map((t) => [t.str, t.transform]))));
      page.cleanup();
    } catch (e) {
      res.pages.push('failed: ' + (e && e.name));
      res.text.push('failed');
    }
  }
  await task.destroy();
  return res;
}

console.log(JSON.stringify(await look(process.argv[2])));
