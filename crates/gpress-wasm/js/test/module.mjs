// The module the tests run against: dist/gpress.wasm (every part of the engine), or the file GPRESS_WASM names
// (relative to the package directory), such as dist/gpress-web.wasm, the web compressor's module:
//
//   npm test                                          (dist/gpress.wasm)
//   GPRESS_WASM=dist/gpress-web.wasm npm test         (npm run test:web)
import { fileURLToPath } from "node:url";

export const moduleUrl = new URL(process.env.GPRESS_WASM || "dist/gpress.wasm", new URL("../", import.meta.url));
export const modulePath = fileURLToPath(moduleUrl);
