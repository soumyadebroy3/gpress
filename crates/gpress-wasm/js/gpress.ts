/**
 * gpress for JavaScript and TypeScript: PDF compression in the browser and Node, lossless unless a lossy
 * effort is asked for.
 *
 *   import { Gpress } from "gpress";
 *   const g = await Gpress.load(fetch("gpress.wasm"));          // or a Uint8Array / ArrayBuffer / path in Node
 *   const smaller = g.optimize(pdfBytes, { effort: 20 });       // { data, outcome: "smaller" | "untouched", reason? }
 *   const fits = g.optimizeToSize(pdfBytes, 500_000);           // the same, with the effort picked and `reached`
 *   const archive = g.compress(anyBytes);                       // the gpress archive (.gpz)
 *   const original = g.expand(archive.data);                    // Uint8Array, or throws GpressError
 *
 *   const module = await Gpress.compile(fetch("gpress.wasm"));  // compile once ...
 *   const a = await Gpress.load(module), b = await Gpress.load(module);   // ... and make instances cheaply
 *
 * The module is deterministic (the same bytes as the native library, on any machine) and problems come
 * back as GpressError with a stable numeric code. The module is built with panic = "abort", so a bug in
 * the engine (or a failed memory growth) traps it instead of returning: the wrapper turns the trap into a
 * GpressError with code INTERNAL (4), and the instance is then dead (every later call throws the same
 * error): load a fresh one. Work happens on the calling thread and there is no clock inside the module
 * (so no time budget); run it in a Web Worker to keep a page responsive and to end it when it has taken
 * too long. Inputs of 2 GiB and more are refused here, before they reach the module's 32-bit memory.
 *
 * The package has two modules: dist/gpress.wasm with every part of the engine, and dist/gpress-web.wasm, the
 * web compressor's (everything optimize uses, without the archive; smaller to download). A call that needs a
 * part the loaded module leaves out throws GpressFeatureError, whose `feature` names it ("archive" for
 * compress and expand in the web module); `features` lists the parts a module has.
 */

export type Outcome = "smaller" | "untouched";

export interface GpressOptions {
  /**
   * 0..100 in steps of 20; a value in between counts as the step below (default 40). 0, 20 and 40 are
   * lossless: 0 fastest, 40 the smallest lossless output. 60, 80 and 100 are lossy: the work of 40 after a
   * step that changes images only, stronger each step, within floors (photos never below 150 pixels per inch
   * at their displayed size, line art and scans never below 300, 1-bit and small images never touched). The
   * archive is lossless at every effort: 40 and above all mean its strongest setting.
   */
  effort?: number;
  /**
   * For measuring: the engine's own lossless level, 0..100, in place of what `effort` means; nothing is
   * lossy then, unless a lossy override below is set.
   */
  engineEffort?: number;
  /**
   * Lossy overrides for optimize(), each 0 or absent = what the effort says; any of them set makes the output
   * lossy at any effort. Photos shown at more than this many pixels per inch are resampled down to it (never
   * below 150, whatever is asked).
   */
  photoPpi?: number;
  /** Line art and scans shown at more than this many pixels per inch are resampled down to it (never below 300). */
  artPpi?: number;
  /** JPEG quality, kept within 50 to 95, for a photo that is resampled or stored without JPEG. */
  jpegQuality?: number;
  /** Largest accepted input in MiB (default 1024). */
  maxInputMiB?: number;
  /** Largest decoded stream re-encoded (normal PDF) or stored decoded (archive), MiB (default 64 / 256). */
  maxStreamMiB?: number;
  /**
   * Archive: the most decoded bytes the parts of one archive may claim in all, MiB (default 256); streams
   * past it are stored as they are. The archive's header declares the total, and expand() refuses an archive
   * declaring more than its own maxDecodedMiB before anything is decoded.
   */
  maxDecodedMiB?: number;
  /** Normal PDF: most objects per object stream (default 200). */
  objectsPerStream?: number;
  /** Leave PDF/A, PDF/X, signed and certified files untouched (default true). */
  respectConformance?: boolean;
  /** Also try Flate in front of JPEG images (default true). */
  flateOverJpeg?: boolean;
  /** Give JPEG images optimal Huffman tables, the same coefficients (default true). */
  jpegTables?: boolean;
  /** Write CCITT fax images as JBIG2 when that is clearly smaller, the same pixels (default true). */
  jbig2?: boolean;
  /** Rewrite files the empty password opens, keeping their encryption (default true). */
  rewriteEncrypted?: boolean;
  /** Archive: store the streams of such files decrypted, encrypted again on expand (default true). */
  decrypt?: boolean;
  /** Use the optional existing library (Lepton) when the build has it (default true). */
  useExistingLibraries?: boolean;
}

export interface ExpandOptions {
  /** Largest accepted archive in MiB (default 1024). */
  maxInputMiB?: number;
  /** Largest file restored, MiB: an archive declaring more is refused before anything is decoded (default 1024). */
  maxLenMiB?: number;
  /**
   * Most decoded bytes the archive's parts may claim in all, MiB: an archive declaring more is refused before
   * anything is decoded (default 256). The module's memory while expanding is about maxLenMiB + maxDecodedMiB
   * for the model's history plus twice the largest part (at most maxDecodedMiB) and the model's tables (up to
   * 164 MiB), so a page sets both to what it can afford.
   */
  maxDecodedMiB?: number;
}

export interface GpressResult {
  data: Uint8Array;
  outcome: Outcome;
  /** For "untouched": why the file was left as it was. */
  reason?: string;
}

/** What optimizeToSize() picked. */
export interface SizedResult extends GpressResult {
  /** The effort that made `data`: 0, 20, 40, 60, 80 or 100. */
  effort: number;
  /** Whether `data` is at most the target. False: no effort reaches it, and `data` is the smallest of the six. */
  reached: boolean;
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
  static readonly INVALID_OPTIONS = 5;
  static readonly TIME_BUDGET = 6;
  static readonly DECLARED_TOO_LARGE = 7;
  static readonly NEEDS_EXISTING_LIBRARY = 8;
  static readonly DECODED_TOO_LARGE = 9;
  static readonly NEEDS_FEATURE = 10;
}

/** A part of the engine a module may leave out, named as its Cargo feature. */
export type Feature = "archive" | "jpeg" | "fax" | "encryption" | "existing";

/** The parts in the order of their bits in the module (archive 1, jpeg 2, fax 4, encryption 8, existing 16). */
const FEATURES: readonly Feature[] = ["archive", "jpeg", "fax", "encryption", "existing"];

/** The call needs a part of the engine the loaded module leaves out (code NEEDS_FEATURE): `feature` names it. */
export class GpressFeatureError extends GpressError {
  constructor(public readonly feature: Feature, message: string) {
    super(GpressError.NEEDS_FEATURE, message);
    this.name = "GpressFeatureError";
  }
}

/** Largest input the 32-bit module takes: under 2 GiB. */
export const MAX_INPUT_BYTES = 0x7fffffff;

/** The switches in the module's flags word: a set bit turns the step off. */
const FLAG_IGNORE_CONFORMANCE = 1;
const FLAG_NO_FLATE_OVER_JPEG = 2;
const FLAG_NO_EXISTING_LIBRARIES = 4;
const FLAG_NO_JPEG_TABLES = 8;
const FLAG_NO_JBIG2 = 16;
const FLAG_KEEP_ENCRYPTED = 32;
const FLAG_NO_DECRYPT = 64;
/** The effort argument is the engine's own level. */
const FLAG_ENGINE_EFFORT = 128;

interface Exports {
  memory: WebAssembly.Memory;
  gpress_alloc(len: number): number;
  gpress_free_input(ptr: number, len: number): void;
  gpress_optimize(ptr: number, len: number, effort: number, maxIn: number, maxStream: number, objectsPerStream: number, flags: number, photoPpi: number, artPpi: number, jpegQuality: number): number;
  gpress_optimize_to_size(ptr: number, len: number, target: number, maxIn: number, maxStream: number, objectsPerStream: number, flags: number): number;
  gpress_compress(ptr: number, len: number, effort: number, maxIn: number, maxStream: number, maxDecoded: number, flags: number): number;
  gpress_expand(ptr: number, len: number, maxIn: number, maxLen: number, maxDecoded: number): number;
  gpress_out_ptr(): number;
  gpress_out_len(): number;
  gpress_outcome(): number;
  gpress_message_ptr(): number;
  gpress_message_len(): number;
  gpress_clear_out(): void;
  gpress_version_number(): number;
  gpress_archive_version(): number;
  gpress_features(): number;
  gpress_needed_feature(): number;
}

/** Where the module comes from: a fetch() Response, bytes, a compiled Module, a URL, or (in Node) a file path. */
export type Source = Response | Promise<Response> | ArrayBuffer | ArrayBufferView | WebAssembly.Module | URL | string;

interface Packed {
  effort: number;
  maxIn: number;
  maxStream: number;
  maxDecoded: number;
  objectsPerStream: number;
  flags: number;
  photoPpi: number;
  artPpi: number;
  jpegQuality: number;
}

export class Gpress {
  /** Set once the module has trapped: its memory and state can no longer be trusted. */
  private dead: GpressError | null = null;

  private constructor(private readonly ex: Exports) {}

  /**
   * Compiles the module without making an instance. Compiling is most of the cost of a first load, so
   * compile once and pass the Module to load() for every instance after that (one per worker, or a fresh
   * one after a trap). A Module can be posted to a Web Worker. A Response (or a URL that is not a file)
   * compiles while it downloads when the server sends it as application/wasm.
   */
  static async compile(source: Source): Promise<WebAssembly.Module> {
    // The kinds are told apart by their tags, not instanceof, so that a value made in another realm (an
    // iframe, a vm context) is taken too.
    const kind = tagOf(source);
    if (kind === "WebAssembly.Module") return source as WebAssembly.Module;
    if (kind === "ArrayBuffer" || kind === "SharedArrayBuffer" || ArrayBuffer.isView(source)) {
      return WebAssembly.compile(bytesOf(source as ArrayBufferLike | ArrayBufferView));
    }
    const url = kind === "URL" ? (source as URL) : null;
    if (typeof source === "string" || url?.protocol === "file:") {
      // A file (Node only). The import is dynamic so browser bundlers don't try to resolve it.
      const fs = (await import(/* @vite-ignore */ "node:fs/promises" as string)) as { readFile(p: string | URL): Promise<Uint8Array> };
      return WebAssembly.compile(bytesOf(await fs.readFile(source as string | URL)));
    }
    const response = await (url ? fetch(url) : (source as Response | Promise<Response>));
    if (!response.ok) throw new Error(`could not fetch the module: HTTP ${response.status}`);
    // compileStreaming() refuses any type but exactly "application/wasm" (no parameters, lower case), and
    // compiling the whole body works for every type.
    return typeof WebAssembly.compileStreaming === "function" && response.headers.get("content-type") === "application/wasm"
      ? WebAssembly.compileStreaming(response)
      : WebAssembly.compile(await response.arrayBuffer());
  }

  /**
   * A new instance from a compiled Module (cheap: no compiling), or from any other source (compiled first).
   * Each instance has its own memory and runs one call at a time.
   */
  static async load(source: Source): Promise<Gpress> {
    const instance = await WebAssembly.instantiate(await Gpress.compile(source), {});
    return new Gpress(instance.exports as unknown as Exports);
  }

  /** "major.minor.patch" of the loaded module. */
  get version(): string {
    const n = this.ex.gpress_version_number();
    return `${Math.floor(n / 1_000_000)}.${Math.floor(n / 1000) % 1000}.${n % 1000}`;
  }

  /** The archive format version the module writes and reads, 0 for a module without the archive. */
  get archiveVersion(): number {
    return this.ex.gpress_archive_version();
  }

  /** The parts of the engine the module has ("archive" is missing from the web module). */
  get features(): Feature[] {
    const bits = this.ex.gpress_features();
    return FEATURES.filter((_, i) => (bits >>> i) & 1);
  }

  /**
   * A smaller PDF that any reader opens, or the input untouched. Every object's value is preserved at efforts
   * 0 to 40; from 60 images may be changed, within the floors of the effort levels.
   */
  optimize(input: Uint8Array, options: GpressOptions = {}): GpressResult {
    const o = pack(options);
    return this.run(input, (ptr, len) =>
      this.ex.gpress_optimize(ptr, len, o.effort, o.maxIn, o.maxStream, o.objectsPerStream, o.flags, o.photoPpi, o.artPpi, o.jpegQuality),
    );
  }

  /**
   * A PDF of at most `targetBytes`: optimize() at efforts 0, 20, 40, 60, 80 and 100 in turn (so up to six runs
   * of the engine), returning the first result that is small enough, or else the smallest of the six with
   * `reached` false. Efforts 60 and above are lossy. `options.effort` is not used; `engineEffort` or a lossy
   * override set throws GpressError with code INVALID_OPTIONS.
   */
  optimizeToSize(input: Uint8Array, targetBytes: number, options: GpressOptions = {}): SizedResult {
    const o = pack(options);
    if (o.photoPpi || o.artPpi || o.jpegQuality) {
      throw new GpressError(GpressError.INVALID_OPTIONS, "invalid options: a target size picks the effort itself: lossy overrides must not be set");
    }
    // Every output is under 4 GiB here, so a larger target is as good as that.
    const target = clamp(targetBytes, 0, 0xffffffff);
    let picked = 0;
    const r = this.run(input, (ptr, len) => {
      const code = this.ex.gpress_optimize_to_size(ptr, len, target, o.maxIn, o.maxStream, o.objectsPerStream, o.flags);
      // The outcome also carries the effort (bits 8 to 15) and whether the target was reached (bit 16).
      picked = this.ex.gpress_outcome();
      return code;
    });
    return { ...r, effort: (picked >> 8) & 0xff, reached: (picked >> 16) === 1 };
  }

  /** The gpress archive of any input; expand() restores it byte for byte. */
  compress(input: Uint8Array, options: GpressOptions = {}): GpressResult {
    const o = pack(options);
    return this.run(input, (ptr, len) => this.ex.gpress_compress(ptr, len, o.effort, o.maxIn, o.maxStream, o.maxDecoded, o.flags));
  }

  /** The original bytes from an archive. A damaged archive throws GpressError, never returns a wrong file. */
  expand(archive: Uint8Array, options: ExpandOptions = {}): Uint8Array {
    const maxIn = clamp(options.maxInputMiB ?? 0, 0, 1 << 20);
    const maxLen = clamp(options.maxLenMiB ?? 0, 0, 1 << 24);
    const maxDecoded = clamp(options.maxDecodedMiB ?? 0, 0, 1 << 24);
    return this.run(archive, (ptr, len) => this.ex.gpress_expand(ptr, len, maxIn, maxLen, maxDecoded)).data;
  }

  /** True when the bytes start like a gpress archive. */
  static isArchive(data: Uint8Array): boolean {
    return data.length >= 4 && data[0] === 0x89 && data[1] === 0x47 && data[2] === 0x50 && data[3] === 0x5a;
  }

  private run(input: Uint8Array, call: (ptr: number, len: number) => number): GpressResult {
    if (this.dead) throw this.dead;
    if (input.length > MAX_INPUT_BYTES) {
      throw new GpressError(GpressError.TOO_LARGE, `input is ${input.length} bytes, over the module's limit of ${MAX_INPUT_BYTES}`);
    }
    const ptr = this.ex.gpress_alloc(input.length);
    if (ptr === 0) throw new GpressError(GpressError.TOO_LARGE, "out of memory allocating the input buffer");
    let trapped = false;
    try {
      new Uint8Array(this.ex.memory.buffer, ptr, input.length).set(input);
      let code: number;
      try {
        code = call(ptr, input.length);
      } catch (e) {
        // A trap: a panic in the engine (the module is built with panic = "abort", so nothing catches
        // it inside) or a failed memory growth. The instance is no longer usable.
        trapped = true;
        this.dead = new GpressError(GpressError.INTERNAL, `the module trapped (${e instanceof Error ? e.message : String(e)}); load it again`);
        throw this.dead;
      }
      const message = this.message();
      if (code === GpressError.NEEDS_FEATURE) {
        const bit = this.ex.gpress_needed_feature();
        const feature = FEATURES.find((_, i) => bit === 1 << i);
        if (feature) throw new GpressFeatureError(feature, message || `the module leaves out the ${feature} feature`);
      }
      if (code !== 0) throw new GpressError(code, message || `gpress error ${code}`);
      // Copy out: the module reuses its result buffer on the next call.
      const data = new Uint8Array(this.ex.memory.buffer, this.ex.gpress_out_ptr(), this.ex.gpress_out_len()).slice();
      const outcome: Outcome = (this.ex.gpress_outcome() & 0xff) === 2 ? "untouched" : "smaller";
      return message ? { data, outcome, reason: message } : { data, outcome };
    } finally {
      // After a trap the module is not called again (its state may be inconsistent); the input buffer is
      // left with the dead instance.
      if (!trapped) {
        this.ex.gpress_free_input(ptr, input.length);
        this.ex.gpress_clear_out();
      }
    }
  }

  private message(): string {
    const len = this.ex.gpress_message_len();
    if (len === 0) return "";
    return new TextDecoder().decode(new Uint8Array(this.ex.memory.buffer, this.ex.gpress_message_ptr(), len));
  }
}

/** The options as the module takes them: numbers in MiB (0 = default) and a flags word. */
function pack(options: GpressOptions): Packed {
  return {
    effort: clamp(options.engineEffort ?? options.effort ?? 40, 0, 100),
    maxIn: clamp(options.maxInputMiB ?? 0, 0, 1 << 20),
    maxStream: clamp(options.maxStreamMiB ?? 0, 0, 1 << 20),
    maxDecoded: clamp(options.maxDecodedMiB ?? 0, 0, 1 << 24),
    objectsPerStream: clamp(options.objectsPerStream ?? 0, 0, 1 << 20),
    flags:
      (options.respectConformance === false ? FLAG_IGNORE_CONFORMANCE : 0) |
      (options.flateOverJpeg === false ? FLAG_NO_FLATE_OVER_JPEG : 0) |
      (options.useExistingLibraries === false ? FLAG_NO_EXISTING_LIBRARIES : 0) |
      (options.jpegTables === false ? FLAG_NO_JPEG_TABLES : 0) |
      (options.jbig2 === false ? FLAG_NO_JBIG2 : 0) |
      (options.rewriteEncrypted === false ? FLAG_KEEP_ENCRYPTED : 0) |
      (options.decrypt === false ? FLAG_NO_DECRYPT : 0) |
      (options.engineEffort === undefined ? 0 : FLAG_ENGINE_EFFORT),
    photoPpi: clamp(options.photoPpi ?? 0, 0, 0xffff),
    artPpi: clamp(options.artPpi ?? 0, 0, 0xffff),
    jpegQuality: clamp(options.jpegQuality ?? 0, 0, 0xff),
  };
}

/** "ArrayBuffer", "URL", "WebAssembly.Module" and so on: the same in every realm. */
function tagOf(v: unknown): string {
  return Object.prototype.toString.call(v).slice(8, -1);
}

/**
 * The bytes as compile() takes them: a Uint8Array over them (compile() refuses a DataView), copied when
 * they live in shared memory (which compile() need not take).
 */
function bytesOf(b: ArrayBufferLike | ArrayBufferView): BufferSource {
  const view = ArrayBuffer.isView(b) ? new Uint8Array(b.buffer, b.byteOffset, b.byteLength) : new Uint8Array(b);
  return (tagOf(view.buffer) === "SharedArrayBuffer" ? view.slice() : view) as BufferSource;
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, Math.floor(Number.isFinite(v) ? v : lo)));
}
