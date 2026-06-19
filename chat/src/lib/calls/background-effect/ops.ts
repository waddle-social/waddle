/**
 * The library + asset boundary the call engine drives the camera background
 * through, behind a two-method interface so the engine stays free of
 * `@livekit/track-processors`, MediaPipe, and IndexedDB.
 *
 * The real implementation lives in `processor.ts` (heavy: it pulls the library
 * runtime + segmentation assets + the custom-image store) and is reached lazily
 * via `registry.ts`; the engine test injects a fake. This mirrors how the
 * AI-noise engine injects `makeAiNoiseProcessor`.
 */

import type { Track, TrackProcessor, VideoProcessorOptions } from "livekit-client";
import type { ActiveBackgroundEffect } from "./effect-id";

/** A LiveKit video `TrackProcessor` we attach to the local camera. */
export type VideoBackgroundProcessor = TrackProcessor<Track.Kind.Video, VideoProcessorOptions>;

/**
 * Create and switch operations over a camera background processor. `create`
 * builds a fresh processor for an effect (resolving self-hosted assets and the
 * replacement image's URL); `switch` re-points an already-attached processor at
 * a new effect *in place* — the library's artifact-free mode change.
 */
export interface CameraBackgroundOps {
  create(effect: ActiveBackgroundEffect): Promise<VideoBackgroundProcessor>;
  switch(processor: VideoBackgroundProcessor, effect: ActiveBackgroundEffect): Promise<void>;
}
