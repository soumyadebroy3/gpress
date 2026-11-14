// The page's own logic, kept apart from the page so that `node --test web/test/*.test.mjs` runs it without a
// browser: the level table, one file's run with the target-size loop, sizes as text, download names and the
// zip behind "Download all". Nothing here touches the document or the network.

/** The six effort levels. 0, 20 and 40 are lossless; 60, 80 and 100 change images, within fixed floors. */
export const LEVELS = [
  { effort: 0, lossless: true, name: "Fastest", about: "The quickest pass. Good for many files or very large ones." },
  { effort: 20, lossless: true, name: "Balanced", about: "Searches harder for a smaller file." },
  { effort: 40, lossless: true, name: "Smallest", about: "Searches hardest: the smallest lossless result, and the slowest of the three." },
  { effort: 60, lossless: false, name: "Light", about: "Images above 300 pixels per inch are scaled down to 300, and photos stored without JPEG become JPEG at quality 85." },
  { effort: 80, lossless: false, name: "Medium", about: "As Light, with photos scaled down to 150 pixels per inch." },
  { effort: 100, lossless: false, name: "Strong", about: "As Medium at JPEG quality 75, and JPEG photos are written again at that quality when it saves a tenth or more." },
];

export const DEFAULT_EFFORT = 40;

/** The largest input the page gives the engine, in MiB: the engine's own default limit. */
export const MAX_INPUT_MIB = 1024;

/** The engine reads a file only when it starts with a PDF header, and so does the page. */
export function isPdf(bytes) {
  return bytes.length >= 5 && String.fromCharCode(...bytes.subarray(0, 5)) === "%PDF-";
}

/**
 * One file through the engine. `g` is a loaded Gpress; `effort` is used when `target` (bytes) is null. With a
 * target the levels are tried from 0 upward and the first result at or under it is kept, else the smallest:
 * the module's own optimizeToSize when it has one, this loop otherwise. `onLevel(effort)` is called before
 * each run of the loop. Returns { outcome, effort, reached?, data?, reason? }. The outcome is "smaller" (with
 * `data`, always smaller than the input, so nothing larger or equal is ever offered), "untouched" (with the
 * engine's reason) or "refused" (not a PDF: the engine is not run).
 */
export function compress(g, input, { effort = DEFAULT_EFFORT, target = null } = {}, onLevel = () => {}) {
  if (!isPdf(input)) return { outcome: "refused", effort, reason: "This is not a PDF file." };
  const options = { maxInputMiB: MAX_INPUT_MIB };
  let r;
  if (target === null) {
    onLevel(effort);
    r = { ...g.optimize(input, { ...options, effort }), effort };
  } else if (typeof g.optimizeToSize === "function") {
    r = g.optimizeToSize(input, target, options);
  } else {
    for (const level of LEVELS) {
      onLevel(level.effort);
      const tried = { ...g.optimize(input, { ...options, effort: level.effort }), effort: level.effort };
      if (!r || tried.data.length < r.data.length) r = tried;
      if (r.data.length <= target) break;
    }
    r.reached = r.data.length <= target;
  }
  const out = { outcome: "untouched", effort: r.effort };
  if (target !== null) out.reached = r.reached;
  if (r.outcome === "smaller" && r.data.length < input.length) return { ...out, outcome: "smaller", data: r.data };
  return { ...out, reason: r.reason || "no saving possible" };
}

/** The target in bytes for a file of `size` bytes: `amount` percent of it, or `amount` KB or MB. Null when unusable. */
export function targetBytes(size, amount, unit) {
  if (!Number.isFinite(amount) || amount <= 0) return null;
  const bytes = unit === "%" ? (size * amount) / 100 : amount * (unit === "MB" ? 1 << 20 : 1 << 10);
  return Math.floor(bytes);
}

/** A size as people read it, in units of 1024: "812 B", "4.20 KB", "12.3 MB", "512 MB", "1.50 GB". */
export function formatBytes(n) {
  const units = ["B", "KB", "MB", "GB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${i === 0 || n >= 100 ? Math.round(n) : n.toFixed(n >= 10 ? 1 : 2)} ${units[i]}`;
}

/** The share of `before` that `after` saves: "37.2%", and "under 0.1%" for a saving too small to show. */
export function savedPercent(before, after) {
  const p = before > 0 ? (1 - after / before) * 100 : 0;
  return p > 0 && p < 0.1 ? "under 0.1%" : `${p.toFixed(1)}%`;
}

/** The name a result is saved under: "report.pdf" gives "report-compressed.pdf". No path can come through. */
export function outputName(name) {
  const stem = name.replace(/\.pdf$/i, "").replace(/[\\/:\0]/g, "_") || "document";
  return `${stem}-compressed.pdf`;
}

/** What an error from the worker means to the person who chose the file. */
export function explain(error) {
  if (error.code === 1 || error.name === "RangeError") return "This file is too large for the browser's memory. Your file is unchanged.";
  if (error.code === 4) return `The compressor ran out of memory or stopped on this file. Your file is unchanged. (${error.message})`;
  if (error.name === "NotReadableError" || error.name === "NotFoundError") return "The browser could not read this file. It may have been moved or deleted.";
  return `Could not compress this file: ${error.message}`;
}

const CRC = new Uint32Array(256).map((_, n) => {
  for (let k = 0; k < 8; k++) n = n & 1 ? (n >>> 1) ^ 0xedb88320 : n >>> 1;
  return n;
});

/** The CRC-32 a zip stores for a file. */
export function crc32(bytes) {
  let c = ~0;
  for (let i = 0; i < bytes.length; i++) c = CRC[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return ~c >>> 0;
}

/**
 * A zip of `files` ({ name, crc, size, data }), stored without compression (they are compressed PDFs), as the
 * parts of a Blob: the headers are made here and each `data` (bytes or a Blob) goes in as it is, so no file is
 * copied. Names are made unique. Null when the files do not fit a plain zip.
 * shortcut: no zip64, so at most 65,535 files and under 4 GiB in all; upgrade if a page ever holds more.
 */
export function zip(files, date = new Date()) {
  const text = new TextEncoder();
  const time = (date.getHours() << 11) | (date.getMinutes() << 5) | (date.getSeconds() >> 1);
  const day = (Math.max(0, date.getFullYear() - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate();
  const parts = [];
  const directory = [];
  const used = new Set();
  let offset = 0;
  for (const f of files) {
    let name = f.name;
    for (let k = 2; used.has(name.toLowerCase()); k++) name = f.name.replace(/(\.[^.]*)?$/, ` (${k})$1`);
    used.add(name.toLowerCase());
    const nameBytes = text.encode(name);
    // The fields a local header and a directory entry share: version 2.0, UTF-8 names, stored.
    const shared = new DataView(new ArrayBuffer(26));
    [20, 0x0800, 0, time, day].forEach((v, i) => shared.setUint16(i * 2, v, true));
    [f.crc, f.size, f.size].forEach((v, i) => shared.setUint32(10 + i * 4, v, true));
    shared.setUint16(22, nameBytes.length, true);
    const local = new DataView(new ArrayBuffer(4));
    local.setUint32(0, 0x04034b50, true);
    const entry = new DataView(new ArrayBuffer(46));
    entry.setUint32(0, 0x02014b50, true);
    entry.setUint16(4, 20, true);
    new Uint8Array(entry.buffer).set(new Uint8Array(shared.buffer), 6);
    entry.setUint32(42, offset, true);
    parts.push(local, shared, nameBytes, f.data);
    directory.push(entry, nameBytes);
    offset += 30 + nameBytes.length + f.size;
  }
  const directorySize = directory.reduce((n, p) => n + p.byteLength, 0);
  if (files.length > 0xffff || offset + directorySize > 0xffffffff) return null;
  const end = new DataView(new ArrayBuffer(22));
  end.setUint32(0, 0x06054b50, true);
  end.setUint16(8, files.length, true);
  end.setUint16(10, files.length, true);
  end.setUint32(12, directorySize, true);
  end.setUint32(16, offset, true);
  return [...parts, ...directory, end];
}
