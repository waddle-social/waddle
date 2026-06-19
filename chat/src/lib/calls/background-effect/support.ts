/**
 * Capability probe: whether this browser can run the camera background
 * segmentation pipeline at all. Gates the whole settings control, the first of
 * two layers (the second is the runtime fail-open guard in the reconciler).
 *
 * Mirrors `@livekit/track-processors`' own `supportsBackgroundProcessors()`
 * (`BackgroundProcessor.isSupported && ProcessorWrapper.isSupported`) but is
 * replicated here so the probe — run at settings-render time — never imports the
 * library runtime (and its MediaPipe assets) before the user opts in.
 *
 * Pure over an injected env so it is unit-tested without touching globals.
 */

export type BackgroundEffectSupportEnv = {
  /**
   * The WebGL2 compositing requirements MediaPipe's segmenter needs:
   * `OffscreenCanvas`, `VideoFrame`, `createImageBitmap`, and a WebGL2 context.
   */
  hasSegmentationCompositing: boolean;
  /**
   * A way to pump camera frames through the transformer: the modern
   * `MediaStreamTrackProcessor`/`Generator` pair, or the `canvas.captureStream`
   * fallback.
   */
  hasFramePipeline: boolean;
};

export type BackgroundEffectSupport = { available: true } | { available: false; reason: string };

const NO_COMPOSITING = "Your browser can't run the background segmentation (needs WebGL2).";
const NO_PIPELINE = "Your browser can't process the camera frames for backgrounds.";

/** Whether background effects can run in the current environment. */
export function backgroundEffectSupport(env: BackgroundEffectSupportEnv): BackgroundEffectSupport {
  if (!env.hasSegmentationCompositing) return { available: false, reason: NO_COMPOSITING };
  if (!env.hasFramePipeline) return { available: false, reason: NO_PIPELINE };
  return { available: true };
}

/** Read the real environment. Thin; the decision logic lives in the pure probe. */
export function currentBackgroundEffectSupportEnv(): BackgroundEffectSupportEnv {
  const hasSegmentationCompositing =
    typeof OffscreenCanvas !== "undefined" &&
    typeof VideoFrame !== "undefined" &&
    typeof createImageBitmap !== "undefined" &&
    hasWebgl2();
  const hasModernPipeline =
    "MediaStreamTrackGenerator" in globalThis && "MediaStreamTrackProcessor" in globalThis;
  const hasFallbackPipeline =
    typeof HTMLCanvasElement !== "undefined" &&
    typeof VideoFrame !== "undefined" &&
    "captureStream" in HTMLCanvasElement.prototype;
  return {
    hasSegmentationCompositing,
    hasFramePipeline: hasModernPipeline || hasFallbackPipeline,
  };
}

function hasWebgl2(): boolean {
  if (typeof document === "undefined") return false;
  try {
    return !!document.createElement("canvas").getContext("webgl2");
  } catch {
    return false;
  }
}
