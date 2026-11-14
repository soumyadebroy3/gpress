// Runs the engine off the page's thread, one file per message. The engine cannot be interrupted, so the page
// cancels by ending this worker and starting another. Everything the worker loads comes in the message: the
// compiled module and blob: URLs of the two scripts. A new worker therefore asks the network for nothing.
let scripts;

onmessage = async ({ data: job }) => {
  try {
    scripts ??= Promise.all([import(job.wrapper), import(job.logic)]);
    const [{ Gpress }, { compress, crc32 }] = await scripts;
    const input = new Uint8Array(await job.file.arrayBuffer());
    // A fresh instance for each file: a module's memory never shrinks, and one that trapped is dead.
    const g = await Gpress.load(job.wasm);
    const r = compress(g, input, job, (effort) => postMessage({ effort }));
    if (r.data) r.crc = crc32(r.data);
    postMessage({ result: r }, r.data ? [r.data.buffer] : []);
  } catch (e) {
    postMessage({ error: { code: e?.code, name: e?.name, message: String(e?.message ?? e) } });
  }
};
