// The page: choosing files, the settings, the list of results and the downloads. The engine runs in
// worker.js, and everything that needs no document is in logic.js.
import { LEVELS, DEFAULT_EFFORT, MAX_INPUT_MIB, explain, formatBytes, outputName, savedPercent, targetBytes, zip } from "./logic.js";

const $ = (id) => document.getElementById(id);

/** A new element with its properties and children. Text goes in as text, never as markup. */
function el(tag, props = {}, ...children) {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...children);
  return node;
}

const say = (text) => ($("status").textContent = text);

for (const level of LEVELS) {
  const radio = el("input", { type: "radio", name: "effort", value: level.effort, checked: level.effort === DEFAULT_EFFORT });
  const label = el("label", { className: "choice" }, radio, el("b", {}, `${level.name} (${level.effort})`), el("small", {}, level.about));
  $(level.lossless ? "lossless" : "lossy").append(label);
}

// The compiled module and blob: URLs of the worker's scripts, once loaded. They are fetched once and kept in
// memory, so starting a worker again after Cancel needs no network.
let engine = null;

async function loadEngine() {
  const keep = async (path) => {
    const response = await fetch(path);
    if (!response.ok) throw new Error(`${path}: HTTP ${response.status}`);
    return URL.createObjectURL(await response.blob());
  };
  // The module, the largest download, starts with the scripts and compiles as it arrives.
  const [module, wrapper, logic, worker] = await Promise.all([fetch("dist/gpress-web.wasm"), ...["dist/gpress.js", "logic.js", "worker.js"].map(keep)]);
  const { Gpress } = await import(wrapper);
  return { wrapper, logic, worker, wasm: await Gpress.compile(module) };
}

loadEngine().then(
  (loaded) => {
    engine = loaded;
    $("pick").disabled = false;
    say("Ready.");
  },
  (e) => {
    $("status").className = "error";
    say(`The compressor could not be loaded (${e.message}). Check your connection and reload the page; it needs a browser with WebAssembly.`);
  },
);

// One entry per chosen file: { file, row, state, effort, target, reached, after, blob, crc, url, note, ms }.
// The states are queued, working, smaller, untouched, failed and cancelled.
const items = [];
let worker = null;
let current = null;
let started = 0;
let ticker = 0;

function add(files) {
  for (const file of files) {
    const item = { file, state: "queued", row: el("li") };
    items.push(item);
    $("list").append(item.row);
    render(item);
  }
  $("results").hidden = items.length === 0;
  next();
}

/** Starts the next waiting file, or sums up when none is left. */
function next() {
  if (current) return;
  current = items.find((item) => item.state === "queued") ?? null;
  refresh();
  if (!current) {
    // With nothing left to do the worker goes, and with it the memory the last file took.
    stopWorker();
    if (items.length) say(`Done. ${summary()}`);
    return;
  }
  const item = current;
  started = performance.now();
  if (item.file.size > MAX_INPUT_MIB * 2 ** 20) return finish({ state: "failed", note: explain({ code: 1 }) });
  const target = $("use-target").checked ? targetBytes(item.file.size, $("amount").valueAsNumber, $("unit").value) : null;
  // With a target the level is not known before the worker reports the one it is trying.
  const effort = target === null ? Number(document.querySelector("[name=effort]:checked").value) : undefined;
  Object.assign(item, { state: "working", target, effort });
  render(item);
  say(`Compressing ${item.file.name} (${items.indexOf(item) + 1} of ${items.length})`);
  worker ??= startWorker();
  worker.postMessage({ ...engine, file: item.file, effort, target });
}

/** The file being worked on has ended in the state `changes` names. */
function finish(changes) {
  const item = Object.assign(current, changes, { ms: performance.now() - started });
  current = null;
  render(item);
  next();
}

function startWorker() {
  const w = new Worker(engine.worker);
  w.onmessage = ({ data }) => {
    if (w !== worker || !current) return;
    if (data.error) return finish({ state: "failed", note: explain(data.error) });
    if (!data.result) {
      current.effort = data.effort;
      return render(current);
    }
    const { outcome: state, effort, reached, reason: note, data: bytes, crc } = data.result;
    if (state !== "smaller") return finish({ state, effort, reached, note });
    const blob = new Blob([bytes], { type: "application/pdf" });
    finish({ state, effort, reached, crc, blob, after: blob.size, url: URL.createObjectURL(blob) });
  };
  w.onerror = (e) => {
    if (w !== worker) return;
    stopWorker();
    if (current) finish({ state: "failed", note: explain({ code: 4, message: e.message || "the worker stopped" }) });
  };
  return w;
}

function stopWorker() {
  worker?.terminate();
  worker = null;
}

/** Ends the worker, and with it the file being compressed; the files still waiting are not started. */
function cancel() {
  stopWorker();
  current = null;
  for (const item of items) {
    if (item.state === "queued" || item.state === "working") {
      item.state = "cancelled";
      render(item);
    }
  }
  refresh();
  say("Cancelled.");
}

function forget(item) {
  if (item.url) URL.revokeObjectURL(item.url);
  Object.assign(item, { state: "queued", url: null, blob: null });
}

const made = () => items.filter((item) => item.state === "smaller");

/** The controls as the list now stands. */
function refresh() {
  const busy = current !== null;
  clearInterval(ticker);
  if (busy) ticker = setInterval(() => current && render(current), 1000);
  $("cancel").hidden = !busy;
  $("again").disabled = busy;
  $("target").disabled = busy;
  $("levels").disabled = busy || $("use-target").checked;
  $("all").hidden = made().length === 0;
}

function summary() {
  const before = made().reduce((n, item) => n + item.file.size, 0);
  const after = made().reduce((n, item) => n + item.after, 0);
  return made().length
    ? `${made().length} of ${items.length} made smaller: ${formatBytes(before)} to ${formatBytes(after)}, ${savedPercent(before, after)} saved.`
    : "No file was made smaller.";
}

function render(item) {
  const { file, state, effort } = item;
  const level = LEVELS.find((l) => l.effort === effort);
  const how = level ? `Level ${effort}, ${level.lossless ? "lossless" : "images may be reduced"}` : "";
  const took = item.ms === undefined ? "" : `${(item.ms / 1000).toFixed(1)} s`;
  const missed = item.reached === false ? ` No level reaches the target of ${formatBytes(item.target)}` : "";
  const note = el("p", { className: "note" });
  let sizes = formatBytes(file.size);
  let download = "";
  if (state === "queued") note.append("Waiting");
  if (state === "cancelled") note.append("Cancelled");
  if (state === "working") note.append(`Compressing${level ? `, level ${effort}` : ""}, ${Math.round((performance.now() - started) / 1000)} s`);
  if (state === "failed") note.append(item.note);
  if (state === "untouched") note.append(`Left as it was: ${item.note}. ${how}, ${took}.${missed && `${missed}.`}`);
  if (state === "smaller") {
    sizes += ` to ${formatBytes(item.after)}`;
    const hit = item.reached ? ` Target of ${formatBytes(item.target)} reached.` : missed && `${missed}: this is the smallest, ${formatBytes(item.after - item.target)} over.`;
    note.append(el("b", {}, `${savedPercent(file.size, item.after)} smaller.`), ` ${how}, ${took}.${hit}`);
    const name = outputName(file.name);
    download = el("a", { className: "button", href: item.url, download: name }, "Download", el("span", { className: "hidden" }, ` ${name}`));
  }
  item.row.className = state;
  item.row.replaceChildren(el("span", { className: "name", title: file.name }, file.name), el("span", { className: "sizes" }, sizes), note, download);
}

function downloadAll() {
  const parts = zip(made().map((item) => ({ name: outputName(item.file.name), crc: item.crc, size: item.after, data: item.blob })));
  if (!parts) return say("These files are too many or too large for one zip: download them one by one.");
  const url = URL.createObjectURL(new Blob(parts, { type: "application/zip" }));
  el("a", { href: url, download: "gpress-pdfs.zip" }).click();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

$("pick").addEventListener("change", (e) => {
  add(e.target.files);
  e.target.value = "";
});
$("cancel").addEventListener("click", cancel);
$("all").addEventListener("click", downloadAll);
$("again").addEventListener("click", () => {
  for (const item of items) {
    forget(item);
    render(item);
  }
  next();
});
$("clear").addEventListener("click", () => {
  cancel();
  items.splice(0).forEach(forget);
  $("list").replaceChildren();
  $("results").hidden = true;
  say("Ready.");
});
$("use-target").addEventListener("change", refresh);
$("amount").addEventListener("change", (e) => {
  if (!(e.target.valueAsNumber > 0)) e.target.value = 50;
});

// A file dropped anywhere on the page is taken, so the browser never opens it in place of the page.
for (const type of ["dragover", "dragleave", "drop"]) {
  document.addEventListener(type, (e) => {
    e.preventDefault();
    // A dragleave with somewhere to go is a move between two parts of the page, not out of it.
    $("drop").classList.toggle("over", type === "dragover" || (type === "dragleave" && e.relatedTarget !== null));
    if (type === "drop" && engine) add(e.dataTransfer.files);
  });
}
