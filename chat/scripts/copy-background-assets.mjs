/**
 * Copies the MediaPipe `tasks-vision` WASM fileset out of node_modules into
 * `public/mediapipe/tasks-vision/` so the virtual-background segmenter (#1024)
 * loads its runtime from our OWN origin instead of the jsdelivr CDN the
 * `@livekit/track-processors` defaults reach for. Single-thread WASM + WebGL2 →
 * no site-wide cross-origin isolation is needed.
 *
 * Runs as part of `bun run build` (and `dev`). The fileset is build-generated,
 * not committed (see .gitignore); the `.tflite` segmenter model IS committed
 * under `public/mediapipe/models/` since it ships outside node_modules.
 */
import { createRequire } from "node:module";
import { cpSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";

const require = createRequire(import.meta.url);
// Resolve tasks-vision EXACTLY as @livekit/track-processors does, so the hosted
// WASM fileset always matches the MediaPipe JS the runtime actually loads. The
// library pins an exact tasks-vision version; copying a different version's
// fileset (e.g. a newer one hoisted to the top level) fails inside segmenter
// setup. Resolving from the library's scope follows nested-vs-hoisted correctly.
const libRequire = createRequire(require.resolve("@livekit/track-processors/package.json"));
const visionPkg = libRequire.resolve("@mediapipe/tasks-vision/package.json");
const wasmSrc = resolve(dirname(visionPkg), "wasm");
const destDir = resolve(import.meta.dirname, "../public/mediapipe/tasks-vision");

// Only the single-thread loader+binary pairs: FilesetResolver.forVisionTasks
// defaults to threads=false and picks the SIMD `vision_wasm_internal` or, after
// a runtime SIMD probe, the `_nosimd` fallback. The threaded `_module_internal`
// variant (~11 MB) is never selected, so we don't ship it.
const FILES = [
  "vision_wasm_internal.js",
  "vision_wasm_internal.wasm",
  "vision_wasm_nosimd_internal.js",
  "vision_wasm_nosimd_internal.wasm",
];

mkdirSync(destDir, { recursive: true });
for (const file of FILES) {
  cpSync(resolve(wasmSrc, file), resolve(destDir, file));
}
console.log(`[bg-assets] copied ${FILES.length} MediaPipe tasks-vision wasm files → ${destDir}`);
