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
// Resolve via the package's manifest so this works regardless of how the
// transitive dependency is hoisted in the bun store.
const visionPkg = require.resolve("@mediapipe/tasks-vision/package.json");
const wasmSrc = resolve(dirname(visionPkg), "wasm");
const destDir = resolve(import.meta.dirname, "../public/mediapipe/tasks-vision");

mkdirSync(destDir, { recursive: true });
cpSync(wasmSrc, destDir, { recursive: true });
console.log(`[bg-assets] copied MediaPipe tasks-vision wasm → ${destDir}`);
