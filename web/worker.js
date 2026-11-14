// Runs the engine off the page's thread, one file per message. The engine cannot be interrupted, so the page
// cancels by ending this worker and starting another. Everything the worker loads comes in the message (the
// compiled module and blob: URLs of the two scripts, see app.js), so it asks the network for nothing.
// A file the browser cannot read, or cannot hold, fails as one kind of error.
const read = (blob) => blob.arrayBuffer().catch((e) => Promise.reject({ code: "read", message: e.message }));

onmessage = async ({ data: job }) => {
  try {
    const { Gpress } = await import(job.wrapper);
    const { compress, crc32, isPdf } = await import(job.logic);
    // The first bytes say whether it is a PDF; only a PDF is read whole.
    const head = new Uint8Array(await read(job.file.slice(0, 5)));
    const input = isPdf(head) ? new Uint8Array(await read(job.file)) : head;
    // A fresh instance for each file: a module's memory never shrinks, and one that trapped is dead.
    const g = await Gpress.load(job.wasm);
    const r = compress(g, input, job, (effort) => postMessage({ effort }));
    if (r.data) r.crc = crc32(r.data);
    postMessage({ result: r }, r.data ? [r.data.buffer] : []);
  } catch (e) {
    postMessage({ error: { code: e?.code, message: String(e?.message ?? e) } });
  }
};
