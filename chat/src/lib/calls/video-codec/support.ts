/**
 * Capability probe: which video codecs this device can actually publish.
 *
 * Mirrors the AI-noise-filter support probe: pure over an injected env so it
 * is unit-tested without touching globals, with a thin reader that reads the
 * real `RTCRtpSender` / `RTCRtpReceiver` capabilities at the edge.
 *
 * A codec is "available" only when the device can BOTH encode it (to publish)
 * and decode it (to subscribe to peers using it) — the bidirectional reading
 * of the WebRTC sender/receiver capabilities. iOS, which has no VP9 encoder,
 * therefore reports VP9 unavailable and falls back to the VP8 backup.
 */

export const PROBED_VIDEO_CODECS = ["vp9", "vp8"] as const;
export type ProbedVideoCodec = (typeof PROBED_VIDEO_CODECS)[number];

export type VideoCodecSupportEnv = {
  /** Lowercased mimeTypes from `RTCRtpSender.getCapabilities('video')`. */
  encode: readonly string[];
  /** Lowercased mimeTypes from `RTCRtpReceiver.getCapabilities('video')`. */
  decode: readonly string[];
};

type VideoCodecAvailability = { available: true } | { available: false; reason: string };

export type VideoCodecSupport = Record<ProbedVideoCodec, VideoCodecAvailability>;

function availability(codec: ProbedVideoCodec, env: VideoCodecSupportEnv): VideoCodecAvailability {
  const mime = `video/${codec}`;
  const canEncode = env.encode.includes(mime);
  return canEncode ? { available: true } : { available: false, reason: `This browser can't encode ${codec.toUpperCase()}.` };
}

/** Per-codec availability for the current environment. */
export function videoCodecSupport(env: VideoCodecSupportEnv): VideoCodecSupport {
  return {
    vp9: availability("vp9", env),
    vp8: availability("vp8", env),
  };
}
