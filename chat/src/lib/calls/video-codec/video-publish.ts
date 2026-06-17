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
  VideoCaptureOptions,
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

/**
 * Camera plan: the 720p capture cap plus the codec/SVC/encoding publish
 * options, both forwarded verbatim to `setCameraEnabled(enabled, capture,
 * publish)`. `capture` is always present — the builder owns the cap so the
 * engine stays thin — and carries only `resolution`, so the room's
 * `videoCaptureDefaults.deviceId` survives LiveKit's merge.
 */
export type CameraPublishPlan = {
  capture: VideoCaptureOptions;
  publish: TrackPublishOptions;
};

/**
 * Screen-share plan: capture-side options (resolution cap, contentHint),
 * `null` only when the browser can't be constrained (Safari 17), forwarded to
 * `setScreenShareEnabled`.
 */
type ScreenPublishPlan = {
  capture: ScreenShareCaptureOptions | null;
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
): ScreenPublishPlan {
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
 * SVC mode for the VP9 camera: `L3T3` — three spatial layers (the 720/360/180
 * ladder) and three temporal layers. Unlike the screen share (which Chrome
 * collapses to temporal-only `L1T3`), camera content encodes spatial layers
 * fine, so subscribers adapt down the full ladder: a small tile pulls 180p, a
 * spotlight the 720p top layer, and the SFU drops layers per subscriber.
 */
const CAMERA_SVC_MODE: ScalabilityMode = "L3T3";

/**
 * Top-layer ceiling for the camera: the 720p band (~1.7 Mbps, 30 fps), the
 * VP8 `h720` preset bitrate. Deliberately below the old 1080p `h720`→`h1080`
 * ~3 Mbps ceiling — camera tiles render small in a screen-share-focal call, so
 * the spare encode/egress/decode budget goes to the focal screen and audio.
 * Governs both the VP9 primary and the VP8 backup so neither exceeds 720p.
 */
const CAMERA_ENCODING: VideoEncoding = {
  maxBitrate: 1_700_000,
  maxFramerate: 30,
};

/**
 * Capture cap for the camera: 720p (1280×720), the top SVC spatial layer.
 * Replaces the old 1080p ceiling. Carried as a per-publish `VideoCaptureOptions`
 * so the builder — not the engine — owns the cap; LiveKit merges it over the
 * room's `videoCaptureDefaults` without overwriting the camera `deviceId`.
 */
const CAMERA_RESOLUTION: VideoResolution = { width: 1280, height: 720, frameRate: 30 };

/**
 * Per-codec publish options for the camera, gated on the probe exactly like the
 * screen share: VP9 + SVC with an explicit VP8 backup published alongside via
 * multi-codec simulcast (so a VP8-only iOS subscriber can't drag capable
 * viewers down to VP8); VP8-only fallback; nothing forced when neither is
 * available.
 *
 * Unlike `screenShareCodec`, the backup here carries its own `encoding`: the
 * top-level `videoEncoding` bounds only the primary codec's layers, not the
 * simulcast backup track, so the VP8 backup needs the 720p cap set explicitly
 * or it would re-derive an uncapped (1080p-band) ceiling. The screen share
 * doesn't need this because its cap rides the `screenShareEncoding` field.
 */
function cameraCodec(capability: VideoCodecSupport): TrackPublishOptions {
  if (capability.vp9.available) {
    return {
      videoCodec: "vp9",
      scalabilityMode: CAMERA_SVC_MODE,
      ...(capability.vp8.available
        ? {
            backupCodec: { codec: "vp8", encoding: { ...CAMERA_ENCODING } },
            backupCodecPolicy: BackupCodecPolicy.SIMULCAST,
          }
        : {}),
    };
  }
  if (capability.vp8.available) return { videoCodec: "vp8" };
  return {};
}

function cameraPlan(capability: VideoCodecSupport): CameraPublishPlan {
  return {
    capture: { resolution: { ...CAMERA_RESOLUTION } },
    publish: {
      ...cameraCodec(capability),
      videoEncoding: { ...CAMERA_ENCODING },
      degradationPreference: "maintain-framerate",
    },
  };
}

export function videoPublishPlan(args: {
  source: "camera";
  capability: VideoCodecSupport;
}): CameraPublishPlan;
export function videoPublishPlan(args: {
  source: "screen";
  capability: VideoCodecSupport;
  /** Screen-capture environment; defaults to "can constrain" (the common,
   *  non-Safari-17 case). */
  screenCapture?: ScreenCaptureEnv;
}): ScreenPublishPlan;
export function videoPublishPlan(args: {
  source: VideoPublishSource;
  capability: VideoCodecSupport;
  screenCapture?: ScreenCaptureEnv;
}): CameraPublishPlan | ScreenPublishPlan {
  if (args.source !== "screen") return cameraPlan(args.capability);
  return screenSharePlan(args.capability, args.screenCapture ?? { canConstrainResolution: true });
}
