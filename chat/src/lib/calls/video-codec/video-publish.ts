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
  VideoResolution,
} from "livekit-client";
import type { VideoCodecSupport } from "./support";
import type { ScreenCaptureEnv } from "./screen-capture-env";

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
 * force-downscaled).
 */
const SCREEN_SHARE_RESOLUTION: VideoResolution = { width: 2560, height: 1440, frameRate: 30 };

type VideoPublishPlan = {
  /** Capture-side options (resolution cap, contentHint). `null` when the
   *  source uses the room's capture defaults (camera). */
  capture: ScreenShareCaptureOptions | null;
  /** Publish-side options forwarded to `setScreenShareEnabled` /
   *  `setCameraEnabled`. */
  publish: TrackPublishOptions;
};

/**
 * Per-codec publish options for the screen share, gated on what the device can
 * actually do (never force a codec the probe reports unavailable):
 *  - VP9-capable → VP9 + SVC, plus an explicit VP8 backup published *alongside*
 *    via multi-codec simulcast (`SIMULCAST`) rather than the default regression
 *    policy. Under regression a single VP8-only (iOS) subscriber forces the
 *    whole room down to VP8; with simulcast capable subscribers keep VP9 —
 *    "everybody good, capable devices better" — at the cost of a second encode
 *    on the (already capability-gated) publisher. The explicit `backupCodec` is
 *    load-bearing: simulcast only emits the second codec when it is set.
 *  - VP8-only → VP8 at the same raised ceiling (first-class fallback).
 *  - neither reported available (e.g. no `getCapabilities`) → leave `videoCodec`
 *    unset so LiveKit picks its own working default rather than us forcing one.
 */
function screenShareCodec(capability: VideoCodecSupport): TrackPublishOptions {
  if (capability.vp9.available) {
    return {
      videoCodec: "vp9",
      scalabilityMode: SCREEN_SHARE_SVC_MODE,
      ...(capability.vp8.available
        ? { backupCodec: { codec: "vp8" }, backupCodecPolicy: BackupCodecPolicy.SIMULCAST }
        : {}),
    };
  }
  if (capability.vp8.available) return { videoCodec: "vp8" };
  return {};
}

function screenSharePlan(
  capability: VideoCodecSupport,
  screen: ScreenCaptureEnv,
): VideoPublishPlan {
  // `contentHint: "detail"` favours sharpness over motion smoothness — the
  // right call for text/code. It is honoured on the VP8 path; for VP9 SVC
  // LiveKit force-sets the track's hint to "motion" (a Chrome screenshare-SVC
  // workaround), so it is harmless there. The resolution cap is omitted when
  // the browser can't be constrained (Safari 17, see ScreenCaptureEnv).
  const capture: ScreenShareCaptureOptions = {
    contentHint: "detail",
    ...(screen.canConstrainResolution ? { resolution: { ...SCREEN_SHARE_RESOLUTION } } : {}),
  };
  return {
    capture,
    publish: {
      ...screenShareCodec(capability),
      screenShareEncoding: { ...SCREEN_SHARE_ENCODING },
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
  /** Screen-capture environment; defaults to "can constrain" (the common,
   *  non-Safari-17 case). Ignored for the camera source. */
  screenCapture?: ScreenCaptureEnv;
}): VideoPublishPlan {
  if (args.source !== "screen") return cameraPlan();
  return screenSharePlan(args.capability, args.screenCapture ?? { canConstrainResolution: true });
}
