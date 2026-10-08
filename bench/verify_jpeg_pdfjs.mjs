// pdf.js's own JPEG decoder (JpegImage: Firefox's PDF viewer uses it when the browser's decoder can't take the
// image) on pairs of JPEG files: for each pair, whether both decode to the same samples, under each colour
// transform a PDF can ask for. pdf.js parses JPEG markers its own way, so it sees layout problems libjpeg
// doesn't. Used by verify_pdf.py for every JPEG whose bytes changed.
//
//   PDFJS_MODULES=/path/to/node_modules node verify_jpeg_pdfjs.mjs PAIRS.json   -> one JSON line
//   PAIRS.json: [[original.jpg, output.jpg], ...]   output: [{"same": true|false, "detail": "..."}, ...]
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const mods = process.env.PDFJS_MODULES;
const dec = await import(pathToFileURL(path.join(mods, 'pdfjs-dist/image_decoders/pdf.image_decoders.mjs')).href);
dec.setVerbosityLevel(0);

function decode(file, colorTransform) {
  try {
    const j = new dec.JpegImage({ colorTransform });
    j.parse(new Uint8Array(readFileSync(file)));
    const px = j.getData({ width: j.width, height: j.height, forceRGB: false, isSourcePDF: true });
    return createHash('sha256').update(`${j.width}x${j.height}x${j.numComponents}:`).update(px).digest('hex');
  } catch (e) {
    return 'error: ' + String((e && e.message) || e).slice(0, 100);
  }
}

const out = [];
for (const [a, b] of JSON.parse(readFileSync(process.argv[2], 'utf8'))) {
  let res = { same: true, detail: '' };
  // -1: as the file says (Adobe marker), 0: no transform, 1: YCbCr/YCCK (a PDF's /ColorTransform).
  for (const ct of [-1, 0, 1]) {
    const x = decode(a, ct), y = decode(b, ct);
    if (x !== y) {
      res = { same: false, detail: `colour transform ${ct}: ${x.slice(0, 70)} vs ${y.slice(0, 70)}` };
      break;
    }
    if (x.startsWith('error')) {
      res.detail = 'pdf.js fails the same way on both: ' + x.slice(0, 80);
    }
  }
  out.push(res);
}
console.log(JSON.stringify(out));
