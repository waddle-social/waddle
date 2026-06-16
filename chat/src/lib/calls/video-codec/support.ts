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

type ProbedVideoCodec = "vp9" | "vp8";

export type VideoCodecSupportEnv = {
  /** Lowercased mimeTypes from `RTCRtpSender.getCapabilities('video')`. */
  encode: readonly string[];
  /** Lowercased mimeTypes from `RTCRtpReceiver.getCapabilities('video')`. */
  decode: readonly string[];
};

type VideoCodecAvailability = { available: true } | { available: false; reason: string };

export type VideoCodecSupport = Record<ProbedVideoCodec, VideoCodecAvailability>;

function advertises(mimeTypes: readonly string[], codec: ProbedVideoCodec): boolean {
  const mime = `video/${codec}`;
  return mimeTypes.some((m) => m.toLowerCase() === mime);
}

function availability(codec: ProbedVideoCodec, env: VideoCodecSupportEnv): VideoCodecAvailability {
  const name = codec.toUpperCase();
  if (!advertises(env.encode, codec)) {
    return { available: false, reason: `This browser can't encode ${name}.` };
  }
  if (!advertises(env.decode, codec)) {
    return { available: false, reason: `This browser can't decode ${name}.` };
  }
  return { available: true };
}

/** Per-codec availability for the current environment. */
export function videoCodecSupport(env: VideoCodecSupportEnv): VideoCodecSupport {
  return {
    vp9: availability("vp9", env),
    vp8: availability("vp8", env),
  };
}

/**
 * The `static getCapabilities('video')` surface of `RTCRtpSender` /
 * `RTCRtpReceiver`. Narrowed to what the probe reads and made injectable so
 * the edge reader is testable without real WebRTC globals.
 */
type RtpCapabilitiesSource = {
  getCapabilities?: (kind: "video") => { codecs?: { mimeType?: string }[] } | null;
};

function capabilityMimeTypes(source: RtpCapabilitiesSource | undefined): string[] {
  const codecs = source?.getCapabilities?.("video")?.codecs ?? [];
  return codecs.map((c) => c.mimeType ?? "").filter((m) => m.length > 0);
}

/** Read the real environment. Thin; the decision logic lives in the pure probe. */
export function currentVideoCodecSupportEnv(
  env: { sender?: RtpCapabilitiesSource; receiver?: RtpCapabilitiesSource } = {
    sender: typeof RTCRtpSender === "undefined" ? undefined : RTCRtpSender,
    receiver: typeof RTCRtpReceiver === "undefined" ? undefined : RTCRtpReceiver,
  },
): VideoCodecSupportEnv {
  return {
    encode: capabilityMimeTypes(env.sender),
    decode: capabilityMimeTypes(env.receiver),
  };
}
