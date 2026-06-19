/**
 * The `@livekit/track-processors` boundary: builds and switches the camera's
 * background `TrackProcessor`, pointed at our SELF-HOSTED MediaPipe assets so no
 * third-party CDN (the library's defaults) is ever hit, and no site-wide
 * cross-origin isolation is required (single WebGL2 + GPU-delegate segmenter).
 *
 * This is the thin library/asset boundary — verified manually, not in unit
 * tests (there is no MediaPipe/WebGL/IndexedDB in the test runtime). The pure
 * reconcile that drives it (`reconcile.ts`) and the engine wiring are tested
 * with a fake `CameraBackgroundOps`.
 */

import {
  BackgroundProcessor,
  type BackgroundProcessorWrapper,
  type SwitchBackgroundProcessorOptions,
} from "@livekit/track-processors";
import { catalogEntry } from "./backgrounds";
import { loadCustomBackground } from "./custom-image-store";
import {
  CAMERA_BACKGROUND_PROCESSOR_NAME,
  type ActiveBackgroundEffect,
  type BackgroundImageRef,
} from "./effect-id";
import type { VideoBackgroundProcessor } from "./ops";

/** Same default the library uses; named here so blur stays consistent. */
const DEFAULT_BLUR_RADIUS = 10;

/**
 * Self-hosted asset locations, served same-origin from `public/`. Without these
 * the library fetches the MediaPipe wasm from jsdelivr and the segmenter model
 * from Google storage — the out-of-band fetch this PR exists to avoid.
 */
const ASSET_PATHS = {
  tasksVisionFileSet: "/mediapipe/tasks-vision",
  modelAssetPath: "/mediapipe/models/selfie_segmenter.tflite",
} as const;

/** Build a fresh processor for `effect`, named so the engine can verify it. */
export async function createCameraBackgroundProcessor(
  effect: ActiveBackgroundEffect,
): Promise<VideoBackgroundProcessor> {
  const mode = await switchOptions(effect);
  const wrapper = BackgroundProcessor(
    {
      ...mode,
      assetPaths: ASSET_PATHS,
      segmenterOptions: { delegate: "GPU" },
    },
    CAMERA_BACKGROUND_PROCESSOR_NAME,
  );
  return wrapper as unknown as VideoBackgroundProcessor;
}

/** Re-point an already-attached processor at `effect` in place (no re-init). */
export async function switchCameraBackground(
  processor: VideoBackgroundProcessor,
  effect: ActiveBackgroundEffect,
): Promise<void> {
  const wrapper = processor as unknown as BackgroundProcessorWrapper;
  await wrapper.switchTo(await switchOptions(effect));
}

/** Map our effect to the library's mode options, resolving any image URL. */
async function switchOptions(
  effect: ActiveBackgroundEffect,
): Promise<SwitchBackgroundProcessorOptions> {
  if (effect.kind === "blur") return { mode: "background-blur", blurRadius: DEFAULT_BLUR_RADIUS };
  return { mode: "virtual-background", imagePath: await resolveImagePath(effect.image) };
}

// One outstanding object URL for the custom image. Revoked when the next custom
// image is resolved so a sequence of uploads leaks at most one URL at a time.
let activeObjectUrl: string | undefined;

async function resolveImagePath(image: BackgroundImageRef): Promise<string> {
  if (image.source === "catalog") return catalogEntry(image.id).assetPath;
  const blob = await loadCustomBackground(image.ref);
  if (!blob) throw new Error("The uploaded background image is no longer available.");
  if (activeObjectUrl) URL.revokeObjectURL(activeObjectUrl);
  activeObjectUrl = URL.createObjectURL(blob);
  return activeObjectUrl;
}
