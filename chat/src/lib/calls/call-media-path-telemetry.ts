import type {
  AudioBitrateBand,
  CallStatDirection,
  IceCandidateType,
  IceTransport,
  VideoResolutionBand,
} from "./call-stats";

/**
 * Fleet-measurement telemetry for the media path one call track actually got:
 * the negotiated video codec and the succeeded ICE candidate-pair, tagged with
 * direction (publish vs subscribe) and source (camera vs screen). It is the
 * baseline the codec / Opus / ICE levers in #995 verify against, and it is what
 * surfaces the silent "stuck on TCP relay" rate.
 *
 * Pure and Faro-free: the call engine samples `summarizeVideoStats` and feeds
 * snapshots in; `telemetry.ts` owns the actual `pushEvent`. Observability only —
 * no XMPP/Jingle wire effect.
 */

/** What a call track is carrying its media as. */
type CallMediaPathSource = "camera" | "screen" | "microphone";

export type TelemetryMediaCodec =
  | "AV1"
  | "G722"
  | "H264"
  | "PCMA"
  | "PCMU"
  | "VP8"
  | "VP9"
  | "opus"
  | "red"
  | "unknown";

export function telemetryMediaCodec(value: string | null): TelemetryMediaCodec {
  const name = (value ?? "").split("/").at(-1)?.toLowerCase();
  switch (name) {
    case "av1": return "AV1";
    case "g722": return "G722";
    case "h264": return "H264";
    case "pcma": return "PCMA";
    case "pcmu": return "PCMU";
    case "vp8": return "VP8";
    case "vp9": return "VP9";
    case "opus": return "opus";
    case "red": return "red";
    default: return "unknown";
  }
}

/**
 * One observed media path for a single track. `codec` and the two ICE fields
 * are null until the report names them; the sampler skips fully-empty
 * snapshots so a not-yet-negotiated track never beacons.
 *
 * `audioBitrateBand` is the active-speaker bitrate bucket — non-null only for
 * `microphone` snapshots, so the raised ~64k Opus default is provable fleet-wide
 * while a continuously-varying send bitrate stays low-cardinality and the beacon
 * de-dupes. Always null for video.
 *
 * `videoResolutionBand` is the symmetric video signal — the negotiated top-layer
 * resolution bucket, non-null only for `camera`/`screen` snapshots. A fleet-wide
 * shift from `1080p` to `720p` is what proves the #1001 camera cap took effect
 * and reduced camera egress/decode. Always null for audio.
 */
export type CallMediaPathSnapshot = {
  direction: CallStatDirection;
  source: CallMediaPathSource;
  codec: TelemetryMediaCodec | null;
  iceCandidateType: IceCandidateType | null;
  iceTransport: IceTransport | null;
  audioBitrateBand: AudioBitrateBand | null;
  videoResolutionBand: VideoResolutionBand | null;
};

/**
 * Map a snapshot to the flat, string-valued attribute record beaconed to
 * Grafana Faro (#996). Low-cardinality and PII-free: direction, source, the
 * codec name, and the ICE candidate type + transport — never a participant
 * identity or JID. A null field becomes `"unknown"` so a partially-negotiated
 * path is still countable rather than silently dropped.
 */
export function callMediaPathEventAttributes(
  snapshot: CallMediaPathSnapshot,
): Record<string, string> {
  return {
    direction: snapshot.direction,
    source: snapshot.source,
    codec: snapshot.codec ?? "unknown",
    ice_candidate_type: snapshot.iceCandidateType ?? "unknown",
    ice_transport: snapshot.iceTransport ?? "unknown",
    // Audio-only: emitted only for a microphone path that has a measured band,
    // so video events keep their exact shape and never carry a meaningless
    // audio attribute.
    ...(snapshot.audioBitrateBand ? { audio_bitrate_band: snapshot.audioBitrateBand } : {}),
    // Video-only: the symmetric resolution bucket, emitted only for a camera /
    // screen path that has a measured band, so audio events never carry a
    // meaningless video attribute.
    ...(snapshot.videoResolutionBand
      ? { video_resolution_band: snapshot.videoResolutionBand }
      : {}),
  };
}

/** Stateful, per-call de-duplicating beacon over observed media paths. */
export type CallMediaPathBeacon = {
  /** Report `snapshot` unless an equal one already beaconed this call. */
  observe(snapshot: CallMediaPathSnapshot): void;
  /** Forget the paths seen this call so the next call re-arms from scratch. */
  reset(): void;
};

/** Value-equality of two snapshots — any field differing is a new path. */
function sameCallMediaPath(a: CallMediaPathSnapshot, b: CallMediaPathSnapshot): boolean {
  return (
    a.direction === b.direction &&
    a.source === b.source &&
    a.codec === b.codec &&
    a.iceCandidateType === b.iceCandidateType &&
    a.iceTransport === b.iceTransport &&
    a.audioBitrateBand === b.audioBitrateBand &&
    a.videoResolutionBand === b.videoResolutionBand
  );
}

/**
 * Wrap a `report` sink so each *distinct* media path beacons at most once per
 * call: a recompute that yields the same path collapses to nothing, while a
 * codec switch (VP9 → VP8 backup) or an ICE re-route (relay → host) emits a
 * fresh event. `reset()` is called when a call ends so the next call starts
 * from an empty seen-set.
 *
 * The seen-set is bounded (a few directions × sources × codecs × paths), so a
 * linear scan with value-equality is both simplest and correct.
 */
export function createCallMediaPathBeacon(
  report: (snapshot: CallMediaPathSnapshot) => void,
): CallMediaPathBeacon {
  let seen: CallMediaPathSnapshot[] = [];
  return {
    observe(snapshot) {
      if (seen.some((prior) => sameCallMediaPath(prior, snapshot))) return;
      seen.push(snapshot);
      report(snapshot);
    },
    reset() {
      seen = [];
    },
  };
}
