/**
 * Pure publish-option builder for video tracks. Given the track source and the
 * device's codec capability, it returns the typed LiveKit capture + publish
 * option objects the engine forwards verbatim — the engine stays a thin
 * applier. Source-aware so the camera slice reuses the same rails.
 */

import type {
  ScalabilityMode,
  ScreenShareCaptureOptions,
  TrackPublishOptions,
  VideoEncoding,
} from "livekit-client";
import type { VideoCodecSupport } from "./support";

export type VideoPublishSource = "camera" | "screen";

/**
 * SVC mode for the VP9 screen-share. `L3T3_KEY` is LiveKit's SVC default and
 * the right fit for screen content: three spatial layers let small-tile
 * subscribers pull a lower resolution, and the key-picture prediction makes
 * the upper layers decode from key frames — so they survive lower-layer loss,
 * which matters for crisp text.
 */
const SCREEN_SHARE_SVC_MODE: ScalabilityMode = "L3T3_KEY";

/**
 * Raised focal-stream ceiling for the shared screen. ~5 Mbps on the top SVC
 * layer keeps dense code/text crisp (vs. the old 1080p15 preset's 2.5 Mbps);
 * 30 fps covers smooth scrolling without paying for motion the content
 * rarely has.
 */
const SCREEN_SHARE_ENCODING: VideoEncoding = {
  maxBitrate: 5_000_000,
  maxFramerate: 30,
};

/**
 * Capture ceiling for the shared screen: ~1440p. A `max` constraint caps a
 * larger display without upscaling a smaller source (not force-downscaled).
 * `contentHint: "detail"` tells the encoder to favour sharpness over motion
 * smoothness — the right call for text and code.
 */
const SCREEN_SHARE_CAPTURE: ScreenShareCaptureOptions = {
  resolution: { width: 2560, height: 1440, frameRate: 30 },
  contentHint: "detail",
};

export type VideoPublishPlan = {
  /** Capture-side options (resolution cap, contentHint). `null` when the
   *  source uses the room's capture defaults (camera). */
  capture: ScreenShareCaptureOptions | null;
  /** Publish-side options forwarded to `setScreenShareEnabled` /
   *  `setCameraEnabled`. */
  publish: TrackPublishOptions;
};

function screenSharePlan(capability: VideoCodecSupport): VideoPublishPlan {
  // Capable devices get VP9 + SVC + an explicit VP8 backup; incapable devices
  // fall back to VP8 — but at the SAME raised bitrate and resolution, so the
  // fallback is first-class, not merely working.
  const codec: TrackPublishOptions = capability.vp9.available
    ? {
        videoCodec: "vp9",
        scalabilityMode: SCREEN_SHARE_SVC_MODE,
        backupCodec: { codec: "vp8" },
      }
    : { videoCodec: "vp8" };
  return {
    capture: SCREEN_SHARE_CAPTURE,
    publish: {
      ...codec,
      screenShareEncoding: SCREEN_SHARE_ENCODING,
      degradationPreference: "maintain-resolution",
    },
  };
}

/**
 * Camera publish plan. This slice only builds the codec rails; the camera
 * VP9 treatment is a later slice, so today it preserves the existing
 * behavior — a talking head keeps motion smooth and sheds resolution
 * (`maintain-framerate`) — and lets the room's `videoCaptureDefaults` govern
 * capture (`capture: null`).
 */
function cameraPlan(): VideoPublishPlan {
  return { capture: null, publish: { degradationPreference: "maintain-framerate" } };
}

export function videoPublishPlan(args: {
  source: VideoPublishSource;
  capability: VideoCodecSupport;
}): VideoPublishPlan {
  return args.source === "screen" ? screenSharePlan(args.capability) : cameraPlan();
}
