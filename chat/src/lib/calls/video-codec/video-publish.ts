/**
 * Pure publish-option builder for video tracks. Given the track source and the
 * device's codec capability, it returns the typed LiveKit capture + publish
 * option objects the engine forwards verbatim — the engine stays a thin
 * applier. Source-aware so the camera slice reuses the same rails.
 */

import { BackupCodecPolicy } from "livekit-client";
import type {
  ScalabilityMode,
  ScreenShareCaptureOptions,
  TrackPublishOptions,
  VideoEncoding,
} from "livekit-client";
import type { VideoCodecSupport } from "./support";

type VideoPublishSource = "camera" | "screen";

/**
 * SVC mode for the VP9 screen-share: `L1T3` — one spatial layer, three
 * temporal layers (temporal-only scalability). We state it explicitly to
 * document the negotiated shape, but note LiveKit *enforces* `L1T3` for any
 * VP9 SVC screen-share track regardless of what we request: Chrome can't
 * encode multiple spatial layers for screen content (it collapses the
 * publish resolution), so spatial SVC (`L3T3*`) is off the table here.
 * Temporal SVC still buys per-subscriber framerate adaptation and VP9's
 * efficiency over VP8.
 */
const SCREEN_SHARE_SVC_MODE: ScalabilityMode = "L1T3";

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
 * Capture ceiling for the shared screen: ~1440p (2560×1440, the 16:9 QHD
 * frame). LiveKit maps `resolution` to an `ideal`/`max` capture constraint,
 * so it caps a larger display without upscaling a smaller source (not
 * force-downscaled). `contentHint: "detail"` favours sharpness over motion
 * smoothness — the right call for text and code. Note this hint only takes
 * effect on the VP8 fallback path: for the VP9 SVC path LiveKit force-sets
 * the track's contentHint to `"motion"` (a Chrome screenshare-SVC
 * workaround), so VP9 screen-share rides the camera-style encode path. We
 * still request `"detail"` because it is honoured for the VP8 fallback and
 * harmless for VP9.
 */
const SCREEN_SHARE_CAPTURE: ScreenShareCaptureOptions = {
  resolution: { width: 2560, height: 1440, frameRate: 30 },
  contentHint: "detail",
};

type VideoPublishPlan = {
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
        // Send VP9 and the VP8 backup at the same time (multi-codec simulcast)
        // rather than the default regression policy. Under regression a single
        // VP8-only (iOS) subscriber forces the whole room down to VP8; with
        // simulcast, capable subscribers keep VP9 — "everybody good, capable
        // devices better" — at the cost of a second encode on the (already
        // capability-gated) publisher.
        backupCodec: { codec: "vp8" },
        backupCodecPolicy: BackupCodecPolicy.SIMULCAST,
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
