import type { CameraBackgroundOps } from "./ops";

/**
 * The real camera-background ops, reached through a dynamic `import()` so the
 * `@livekit/track-processors` runtime, the MediaPipe segmentation assets, and
 * the custom-image store are pulled only when an effect is actually applied — a
 * user who never enables a background downloads none of those bytes. Mirrors how
 * the AI-noise registry defers each model's wasm behind a lazy import.
 */
export const cameraBackgroundOps: CameraBackgroundOps = {
  create: async (effect) =>
    (await import("./processor")).createCameraBackgroundProcessor(effect),
  switch: async (processor, effect) =>
    (await import("./processor")).switchCameraBackground(processor, effect),
};
