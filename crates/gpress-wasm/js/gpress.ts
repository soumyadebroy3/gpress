/**
 * gpress for JavaScript and TypeScript: lossless PDF compression in the browser and Node.
 *
 *   import { Gpress } from "gpress";
 *   const g = await Gpress.load(fetch("gpress.wasm"));          // or a Uint8Array / ArrayBuffer / path in Node
 *   const smaller = g.optimize(pdfBytes, { effort: 70 });       // { data, outcome: "smaller" | "untouched", reason? }
 *   const archive = g.compress(anyBytes);                       // the gpress archive (.gpz)
 *   const original = g.expand(archive.data);                    // Uint8Array, or throws GpressError
 *
 * The module is deterministic (same bytes as the native library) and never traps: problems come back
 * as GpressError with a stable numeric code. Work happens on the calling thread; run it in a Web Worker
 * to keep a page responsive.
 */

export type Outcome = "smaller" | "untouched";

export interface GpressOptions {
  /** 0..100: 0 quick, 100 everything the engine knows (default 100). */
  effort?: number;
  /** Largest accepted input in MiB (default 1024). */
  maxInputMiB?: number;
  /** Largest decoded stream kept in memory, MiB (default 64). */
  maxStreamMiB?: number;
  /** Leave PDF/A, PDF/X, signed and certified files untouched (default true). */
  respectConformance?: boolean;
  /** Also try Flate in front of JPEG images (default true). */
  flateOverJpeg?: boolean;
  /** Use the optional existing libraries when the build has them (default true). */
  useExistingLibraries?: boolean;
}

export interface GpressResult {
  data: Uint8Array;
  outcome: Outcome;
  /** For "untouched": why the file was left as it was. */
  reason?: string;
}

export class GpressError extends Error {
  constructor(public readonly code: number, message: string) {
    super(message);
    this.name = "GpressError";
  }
  static readonly TOO_LARGE = 1;
  static readonly DAMAGED = 2;
  static readonly UNSUPPORTED_VERSION = 3;
  static readonly INTERNAL = 4;
}

interface Exports {
  memory: WebAssembly.Memory;
  gpress_alloc(len: number): number;
  gpress_free_input(ptr: number, len: number): void;
  gpress_optimize(ptr: number, len: number, effort: number, maxIn: number, maxStream: number, flags: number): number;
  gpress_compress(ptr: number, len: number, effort: number, maxIn: number, maxStream: number, flags: number): number;
  gpress_expand(ptr: number, len: number, maxIn: number): number;
  gpress_out_ptr(): number;
  gpress_out_len(): number;
  gpress_outcome(): number;
  gpress_message_ptr(): number;
  gpress_message_len(): number;
  gpress_clear_out(): void;
  gpress_version_number(): number;
}

type Source = Response | Promise<Response> | ArrayBuffer | Uint8Array | WebAssembly.Module | string;

export class Gpress {
  private constructor(private readonly ex: Exports) {}

  /** Loads the module from a fetch() Response, bytes, a compiled Module, or (in Node) a file path. */
  static async load(source: Source): Promise<Gpress> {
    let module: WebAssembly.Module;
    if (source instanceof WebAssembly.Module) {
      module = source;
    } else if (typeof source === "string") {
      // A file path (Node only). The import is dynamic so browser bundlers don't try to resolve it.
      const fs = (await import(/* @vite-ignore */ "node:fs/promises" as string)) as { readFile(p: string): Promise<Uint8Array> };
      const bytes = await fs.readFile(source);
      module = await WebAssembly.compile(bytes.slice().buffer as ArrayBuffer);
    } else if (source instanceof ArrayBuffer) {
      module = await WebAssembly.compile(source);
    } else if (source instanceof Uint8Array) {
      module = await WebAssembly.compile(source.slice().buffer as ArrayBuffer);
    } else {
      const response = await source;
      module =
        typeof WebAssembly.compileStreaming === "function" && response.headers.get("content-type")?.includes("application/wasm")
          ? await WebAssembly.compileStreaming(response)
          : await WebAssembly.compile(await response.arrayBuffer());
    }
    const instance = await WebAssembly.instantiate(module, {});
    return new Gpress(instance.exports as unknown as Exports);
  }

  /** "major.minor.patch" of the loaded module. */
  get version(): string {
    const n = this.ex.gpress_version_number();
    return `${Math.floor(n / 1_000_000)}.${Math.floor(n / 1000) % 1000}.${n % 1000}`;
  }

  /** A smaller PDF that any reader opens, every object's value preserved; or the input untouched. */
  optimize(input: Uint8Array, options: GpressOptions = {}): GpressResult {
    return this.run(input, (ptr, len, o) => this.ex.gpress_optimize(ptr, len, o.effort, o.maxIn, o.maxStream, o.flags), options);
  }

  /** The gpress archive of any input; expand() restores it byte for byte. */
  compress(input: Uint8Array, options: GpressOptions = {}): GpressResult {
    return this.run(input, (ptr, len, o) => this.ex.gpress_compress(ptr, len, o.effort, o.maxIn, o.maxStream, o.flags), options);
  }

  /** The original bytes from an archive. A damaged archive throws GpressError, never returns a wrong file. */
  expand(archive: Uint8Array, options: Pick<GpressOptions, "maxInputMiB"> = {}): Uint8Array {
    return this.run(archive, (ptr, len, o) => this.ex.gpress_expand(ptr, len, o.maxIn), options).data;
  }

  /** True when the bytes start like a gpress archive. */
  static isArchive(data: Uint8Array): boolean {
    return data.length >= 4 && data[0] === 0x89 && data[1] === 0x47 && data[2] === 0x50 && data[3] === 0x5a;
  }

  private run(
    input: Uint8Array,
    call: (ptr: number, len: number, o: { effort: number; maxIn: number; maxStream: number; flags: number }) => number,
    options: GpressOptions,
  ): GpressResult {
    const o = {
      effort: clamp(options.effort ?? 100, 0, 100),
      maxIn: clamp(options.maxInputMiB ?? 0, 0, 1 << 20),
      maxStream: clamp(options.maxStreamMiB ?? 0, 0, 1 << 20),
      flags: (options.respectConformance === false ? 1 : 0) | (options.flateOverJpeg === false ? 2 : 0) | (options.useExistingLibraries === false ? 4 : 0),
    };
    const ptr = this.ex.gpress_alloc(input.length);
    if (ptr === 0) throw new GpressError(GpressError.TOO_LARGE, "out of memory allocating the input buffer");
    try {
      new Uint8Array(this.ex.memory.buffer, ptr, input.length).set(input);
      const code = call(ptr, input.length, o);
      const message = this.message();
      if (code !== 0) throw new GpressError(code, message || `gpress error ${code}`);
      // Copy out: the module reuses its result buffer on the next call.
      const data = new Uint8Array(this.ex.memory.buffer, this.ex.gpress_out_ptr(), this.ex.gpress_out_len()).slice();
      const outcome: Outcome = this.ex.gpress_outcome() === 2 ? "untouched" : "smaller";
      return message ? { data, outcome, reason: message } : { data, outcome };
    } finally {
      this.ex.gpress_free_input(ptr, input.length);
      this.ex.gpress_clear_out();
    }
  }

  private message(): string {
    const len = this.ex.gpress_message_len();
    if (len === 0) return "";
    return new TextDecoder().decode(new Uint8Array(this.ex.memory.buffer, this.ex.gpress_message_ptr(), len));
  }
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, Math.floor(Number.isFinite(v) ? v : lo)));
}
