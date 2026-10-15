// A one-page PDF of about 3.8 KB for the start-up test and bench/wasm_startup.mjs, the same bytes every
// time (for a given zlib): a Flate content stream of text in two fonts and a few lines, and the two fonts
// (TrueType, not embedded) with their widths and descriptors, as an office program writes them.
import { deflateSync } from "node:zlib";

export function onePagePdf() {
  const words = ("the quick brown fox jumps over lazy dog page text stream font layout measure start module small " +
    "fast light paper line column figure table width height result method section number value report total " +
    "first second data model input output memory compile instance budget limit target median sample process").split(" ");
  let s = 12345;
  const next = () => (s = (s * 1103515245 + 12345) % 2147483648) >> 8;
  const word = () => words[next() % words.length];
  let content = "q 0.2 0.2 0.6 rg 72 742 468 1.5 re f Q\nBT /F2 16 Tf 72 752 Td (A one-page test document) Tj ET\n";
  let y = 724;
  for (let para = 0; para < 6; para++) {
    content += `BT /F2 11 Tf 72 ${y} Td (${para + 1}. ${word()} ${word()}) Tj ET\n`;
    y -= 15;
    for (let line = 0; line < 8; line++) {
      let t = "";
      for (let w = 0; w < 12; w++) t += (w ? " " : "") + word() + (next() % 9 === 0 ? `, ${next() % 1000}` : "");
      content += `BT /F1 9.5 Tf 72 ${y} Td 0.${next() % 9} Tw (${t}) Tj ET\n`;
      y -= 12;
    }
    y -= 6;
  }
  content += "0.5 w 72 60 m 540 60 l S\nBT /F1 8 Tf 302 44 Td (1) Tj ET\n";
  const z = deflateSync(Buffer.from(content, "latin1"), { level: 6 });
  const font = (name, step, desc) => {
    const widths = [];
    for (let c = 32; c <= 126; c++) widths.push(250 + ((c * step) % 450));
    return `<< /Type /Font /Subtype /TrueType /BaseFont /${name} /FirstChar 32 /LastChar 126 /Widths [${widths.join(" ")}] /Encoding /WinAnsiEncoding /FontDescriptor ${desc} 0 R >>`;
  };
  const descriptor = (name, flags, stem) =>
    `<< /Type /FontDescriptor /FontName /${name} /Flags ${flags} /FontBBox [-665 -325 2000 1006] /ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV ${stem} >>`;
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R /F2 6 0 R >> /ProcSet [/PDF /Text] >> /Contents 8 0 R >>",
    font("Arial", 37, 5),
    descriptor("Arial", 32, 80),
    font("Arial,Bold", 41, 7),
    descriptor("Arial,Bold", 262176, 153),
    null, // the content stream
    "<< /Producer (gpress start-up test) /CreationDate (D:20260101000000Z) >>",
  ];
  const parts = [Buffer.from("%PDF-1.4\n%\xe2\xe3\xcf\xd3\n", "latin1")];
  let pos = parts[0].length;
  const offsets = [];
  objects.forEach((o, k) => {
    offsets.push(pos);
    const body =
      o === null
        ? Buffer.concat([Buffer.from(`${k + 1} 0 obj\n<< /Length ${z.length} /Filter /FlateDecode >>\nstream\n`), z, Buffer.from("\nendstream\nendobj\n")])
        : Buffer.from(`${k + 1} 0 obj\n${o}\nendobj\n`, "latin1");
    parts.push(body);
    pos += body.length;
  });
  let xref = `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const off of offsets) xref += `${String(off).padStart(10, "0")} 00000 n \n`;
  xref += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R /Info 9 0 R >>\nstartxref\n${pos}\n%%EOF\n`;
  parts.push(Buffer.from(xref, "latin1"));
  return new Uint8Array(Buffer.concat(parts));
}
