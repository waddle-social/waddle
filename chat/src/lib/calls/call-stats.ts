/**
 * Pure WebRTC-stats parsing for the call diagnostics section.
 *
 * LiveKit exposes `getRTCStatsReport()` on each `LocalTrack` /
 * `RemoteTrack` (the diagnostics poller calls it on the track); this
 * module turns one such `RTCStatsReport` into a small, display-ready
 * summary. Bitrate is a rate, so it needs two samples — the caller keeps
 * the previous `CallStatSample` per track and passes it back in.
 *
 * Everything here is pure (no timers, no LiveKit, no DOM beyond the
 * standard `RTCStatsReport` shape) so it is unit-testable in isolation;
 * the polling lifecycle lives in the dialog that owns it.
 */

export type CallStatDirection = "send" | "recv";

/**
 * ICE candidate type of the succeeded pair's local candidate. `host` is a
 * direct path, `srflx` a STUN-discovered reflexive one, `prflx` a
 * peer-reflexive one, and `relay` a TURN-relayed path (the costly fallback).
 */
export type IceCandidateType = "host" | "srflx" | "prflx" | "relay";

/**
 * Transport of the succeeded path. For a relay candidate this is the
 * client↔TURN leg (`relayProtocol`), so a TURN/TCP or TURN/TLS fallback —
 * the silent "stuck on TCP relay" case — reads as `tcp`.
 */
export type IceTransport = "udp" | "tcp";

/** Byte/timestamp pair carried between polls to derive a bitrate. */
export type CallStatSample = { bytes: number; timestampMs: number };

export type VideoStatsSummary = {
  /** "1920×1080", or null when the track isn't reporting frames yet. */
  resolution: string | null;
  fps: number | null;
  bitrateKbps: number | null;
  packetLossPct: number | null;
  rttMs: number | null;
  /**
   * Actually-negotiated video codec for the track (e.g. "VP9", "VP8",
   * "H264"), derived from the live report's `codec` entry — null until a
   * codec is reported. This is the real wire codec, not the configured
   * preference: the lever PRD (#995) verifies the VP9-vs-VP8 mix against it.
   */
  codec: string | null;
  /**
   * Local-candidate type of the succeeded ICE pair (host/srflx/prflx/relay),
   * or null until a pair is selected. Exposes how often calls fall back to a
   * TURN relay.
   */
  iceCandidateType: IceCandidateType | null;
  /**
   * Transport of the succeeded ICE path (udp/tcp), or null. For relay it is
   * the TURN-leg protocol, so a TCP/TLS relay reads as `tcp`.
   */
  iceTransport: IceTransport | null;
};

/**
 * Audio counterpart of {@link VideoStatsSummary}: the negotiated audio codec
 * (e.g. "opus"), the derived send/recv bitrate, and the succeeded ICE path.
 * Resolution/fps/loss are video-only concepts and intentionally absent — the
 * audio lever's signal is bitrate (banded via {@link audioBitrateBand}).
 */
type AudioStatsSummary = {
  codec: string | null;
  bitrateKbps: number | null;
  packetLossPct: number | null;
  rttMs: number | null;
  iceCandidateType: IceCandidateType | null;
  iceTransport: IceTransport | null;
};

/** A single diagnostics row: one video track of one participant. */
export type CallStatRow = VideoStatsSummary & {
  key: string;
  label: string;
  sourceLabel: string;
  direction: CallStatDirection;
};

/** Display-ready strings for a row; null metrics render as an em dash. */
export type VideoStatsDisplay = {
  resolution: string;
  fps: string;
  bitrate: string;
  loss: string;
  rtt: string;
  /** Negotiated codec, e.g. "VP9", or an em dash. */
  codec: string;
  /** ICE path as "type · transport" (e.g. "relay · tcp"), or an em dash. */
  icePath: string;
};

/**
 * Loose view over the RTCStats entries we read. The DOM lib types don't
 * cover every field consistently across browsers (e.g. `framesPerSecond`,
 * `frameWidth`), so we read through optional numbers rather than fight
 * the partial typings — values that are absent simply stay null.
 */
type RtpLike = {
  id?: string;
  type?: string;
  kind?: string;
  mediaType?: string;
  frameWidth?: number;
  frameHeight?: number;
  framesPerSecond?: number;
  bytesSent?: number;
  bytesReceived?: number;
  packetsSent?: number;
  packetsReceived?: number;
  packetsLost?: number;
  roundTripTime?: number;
  currentRoundTripTime?: number;
  nominated?: boolean;
  timestamp?: number;
  /** RTP entries point at their `codec` entry by id. */
  codecId?: string;
  /** `codec` entries carry the negotiated MIME, e.g. "video/VP9". */
  mimeType?: string;
  /** `transport` entries name the in-use pair — the spec-correct selector. */
  selectedCandidatePairId?: string;
  /** `candidate-pair` fields used to find and qualify the active path. */
  selected?: boolean;
  state?: string;
  localCandidateId?: string;
  /** `local-candidate` fields describing the path. */
  candidateType?: string;
  protocol?: string;
  relayProtocol?: string;
};

/**
 * Coarse band over the *measured on-the-wire* audio send/recv rate (kbps).
 * Important: this is the wire rate, which sits ABOVE the Opus encoder target —
 * it includes RED redundancy and RTP/SRTP overhead — and Opus is VBR, so quiet
 * speech measures lower than loud speech on the same encoder cap. So the bands
 * describe observed activity, not the configured preset:
 *  - `silent`   — DTX idle / comfort-noise floor; an idle participant costs
 *                 ~nothing, so the raised default never burdens listeners.
 *  - `standard` — light / quiet / intermittent speech, or a lower-rate path.
 *  - `high`     — sustained active speech. On the raised ~64k default an active
 *                 speaker's wire rate clearly clears the floor, so a fleet-wide
 *                 shift toward `high` is the signal that the raise took effect.
 * Three buckets keep it low-cardinality so the de-duping media-path beacon emits
 * at most a handful of audio events per call rather than one per fluctuating
 * sample. `null` (an unmeasured first poll) has no band yet. Thresholds are
 * coarse operational buckets meant to be validated/tuned against Faro
 * (telemetry-first), NOT exact encoder-target cutoffs.
 */
export type AudioBitrateBand = "silent" | "standard" | "high";

/** ≤ this reads as `silent`: DTX comfort-noise / keepalive, not real speech. */
const AUDIO_SILENCE_CEILING_KBPS = 12;
/** > this reads as `high`: a sustained active speaker on the raised 64k default
 *  clears this wire-rate floor (encoder target + RED + overhead). */
const AUDIO_RAISED_FLOOR_KBPS = 56;

export function audioBitrateBand(bitrateKbps: number | null): AudioBitrateBand | null {
  if (bitrateKbps === null) return null;
  if (bitrateKbps <= AUDIO_SILENCE_CEILING_KBPS) return "silent";
  if (bitrateKbps <= AUDIO_RAISED_FLOOR_KBPS) return "standard";
  return "high";
}

/**
 * Video counterpart of {@link audioBitrateBand}: a coarse, low-cardinality band
 * over the negotiated top-layer resolution. It is the fleet-wide signal for the
 * resolution levers in #995 — a shift from `1080p` to `720p` is what proves the
 * #1001 camera cap took effect and reduced camera egress/decode. Banding the
 * height (not beaconing the raw "W×H") keeps the de-duping media-path beacon to
 * a handful of events per call rather than one per SVC-layer flip. `null` (an
 * unreported first poll) has no band yet.
 */
export type VideoResolutionBand = "180p" | "360p" | "540p" | "720p" | "1080p" | "1440p";

export function videoResolutionBand(resolution: string | null): VideoResolutionBand | null {
  if (resolution === null) return null;
  // `summarizeVideoStats` formats resolution as "W×H" (U+00D7); band on height.
  const height = Number.parseInt(resolution.split("×")[1] ?? "", 10);
  if (!Number.isFinite(height) || height <= 0) return null;
  if (height <= 180) return "180p";
  if (height <= 360) return "360p";
  if (height <= 540) return "540p";
  if (height <= 720) return "720p";
  if (height <= 1080) return "1080p";
  return "1440p";
}

/** Strip the "video/" prefix off a codec MIME, e.g. "video/VP9" → "VP9". */
function codecName(mimeType: string | undefined): string | null {
  if (!mimeType) return null;
  const slash = mimeType.indexOf("/");
  const name = slash >= 0 ? mimeType.slice(slash + 1) : mimeType;
  return name.length > 0 ? name : null;
}

/**
 * Pick the active candidate pair. The spec-correct selector is the transport's
 * `selectedCandidatePairId`; without it we fall back to Firefox's `selected`
 * flag, then a nominated *and* succeeded pair, then any succeeded pair, then a
 * pair carrying an RTT, then a bare-nominated one, then the first. `nominated`
 * alone is unreliable — several pairs can be nominated and a failed pair can
 * stay nominated — so it must never shadow the path media is flowing over.
 */
function selectCandidatePair(pairs: RtpLike[], selectedId: string | undefined): RtpLike | undefined {
  if (selectedId !== undefined) {
    const selected = pairs.find((pair) => pair.id === selectedId);
    if (selected) return selected;
  }
  return (
    pairs.find((pair) => pair.selected) ??
    pairs.find((pair) => pair.nominated && pair.state === "succeeded") ??
    pairs.find((pair) => pair.state === "succeeded") ??
    pairs.find((pair) => pair.currentRoundTripTime !== undefined) ??
    pairs.find((pair) => pair.nominated) ??
    pairs[0]
  );
}

/** Narrow a raw `candidateType` to the types we report, else null. */
function normalizeIceCandidateType(value: string | undefined): IceCandidateType | null {
  return value === "host" || value === "srflx" || value === "prflx" || value === "relay"
    ? value
    : null;
}

/**
 * Narrow a raw candidate/relay protocol to udp/tcp. `tls` is TCP-based, so
 * it folds into `tcp`: operationally it is the same non-UDP relay leg.
 */
function normalizeIceTransport(value: string | undefined): IceTransport | null {
  const normalized = value?.toLowerCase();
  if (normalized === "udp") return "udp";
  if (normalized === "tcp" || normalized === "tls") return "tcp";
  return null;
}

/**
 * Resolve the succeeded pair's local candidate to a (type, transport) pair.
 * Relay transport comes from `relayProtocol` (the client↔TURN leg), which is
 * what reveals a TCP/TLS relay; other types use the candidate's `protocol`.
 */
function iceCandidatePath(
  candidatePair: RtpLike | undefined,
  byId: Map<string, RtpLike>,
): { iceCandidateType: IceCandidateType | null; iceTransport: IceTransport | null } {
  const localId = candidatePair?.localCandidateId;
  const local = localId !== undefined ? byId.get(localId) : undefined;
  const iceCandidateType = normalizeIceCandidateType(local?.candidateType);
  // For relay, only `relayProtocol` (the client↔TURN leg) is meaningful — the
  // candidate's own `protocol` is the server↔peer leg (≈always udp) and would
  // mask a TURN/TCP fallback. Absent relayProtocol → null, never a false udp.
  const rawTransport = iceCandidateType === "relay" ? local?.relayProtocol : local?.protocol;
  return { iceCandidateType, iceTransport: normalizeIceTransport(rawTransport) };
}

function isVideo(stat: RtpLike): boolean {
  return stat.kind === "video" || stat.mediaType === "video";
}

function isAudio(stat: RtpLike): boolean {
  return stat.kind === "audio" || stat.mediaType === "audio";
}

/**
 * Derive a kbps rate from the byte/timestamp delta between two samples. Null
 * (unmeasured) when there is no prior sample (first poll), the clock did not
 * advance, or the byte counter went *backwards* — a counter reset (track
 * replaced / ICE restart) is no measurement this tick, not a genuinely idle
 * one, so it must not collapse to 0 (which an audio band would read as DTX
 * silence). A genuine idle tick (bytes unchanged, clock advanced) is a real 0.
 */
function bitrateFromSamples(
  sample: CallStatSample | null,
  prev: CallStatSample | undefined,
): number | null {
  if (!sample || !prev || sample.timestampMs <= prev.timestampMs) return null;
  if (sample.bytes < prev.bytes) return null;
  const deltaBits = (sample.bytes - prev.bytes) * 8;
  const deltaSec = (sample.timestampMs - prev.timestampMs) / 1000;
  return Math.round(deltaBits / deltaSec / 1000);
}

/**
 * Sum a stream's byte counter across all of its RTP entries (one per simulcast
 * layer for video; a single entry for audio) and take the newest timestamp.
 * Null when no entry reported its bytes. Shared by the video and audio
 * summarizers.
 */
function sampleFromRtp(mains: RtpLike[], bytesField: "bytesSent" | "bytesReceived"): CallStatSample | null {
  let bytes: number | undefined;
  let timestampMs: number | undefined;
  for (const main of mains) {
    const layerBytes = main[bytesField];
    if (layerBytes !== undefined) bytes = (bytes ?? 0) + layerBytes;
    if (main.timestamp !== undefined && (timestampMs === undefined || main.timestamp > timestampMs)) {
      timestampMs = main.timestamp;
    }
  }
  return bytes !== undefined && timestampMs !== undefined ? { bytes, timestampMs } : null;
}

/**
 * Reduce one audio track's `RTCStatsReport` to an {@link AudioStatsSummary} plus
 * the fresh `CallStatSample` to retain. Mirrors {@link summarizeVideoStats} but
 * reads the audio RTP stream: the negotiated codec, the bitrate (diffed against
 * `prev`, null on the first poll), and the succeeded ICE candidate path. Used by
 * the media-path beacon to band the active-speaker bitrate and confirm the
 * raised ~64k Opus default fleet-wide.
 */
export function summarizeAudioStats(
  report: RTCStatsReport,
  direction: CallStatDirection,
  prev?: CallStatSample,
): { summary: AudioStatsSummary; sample: CallStatSample | null } {
  const mainType = direction === "send" ? "outbound-rtp" : "inbound-rtp";
  const mains: RtpLike[] = [];
  const remoteInbounds: RtpLike[] = [];
  const candidatePairs: RtpLike[] = [];
  let selectedPairId: string | undefined;
  const byId = new Map<string, RtpLike>();

  report.forEach((raw: unknown) => {
    const stat = raw as RtpLike;
    if (stat.id !== undefined) byId.set(stat.id, stat);
    if (stat.type === mainType && isAudio(stat)) {
      mains.push(stat);
    } else if (stat.type === "remote-inbound-rtp" && isAudio(stat)) {
      remoteInbounds.push(stat);
    } else if (stat.type === "candidate-pair") {
      candidatePairs.push(stat);
    } else if (stat.type === "transport" && stat.selectedCandidatePairId !== undefined) {
      selectedPairId = stat.selectedCandidatePairId;
    }
  });

  const candidatePair = selectCandidatePair(candidatePairs, selectedPairId);
  const { iceCandidateType, iceTransport } = iceCandidatePath(candidatePair, byId);

  const codecRef = mains.find((main) => main.codecId !== undefined)?.codecId;
  const codecEntry = codecRef !== undefined ? byId.get(codecRef) : undefined;
  const codec = codecEntry?.type === "codec" ? codecName(codecEntry.mimeType) : null;

  const sample = sampleFromRtp(mains, direction === "send" ? "bytesSent" : "bytesReceived");
  const bitrateKbps = bitrateFromSamples(sample, prev);

  const packetLossPct = direction === "recv"
    ? recvPacketLoss(mains)
    : sendPacketLoss(mains, remoteInbounds);
  let rttSec: number | undefined;
  if (direction === "send") {
    for (const remote of remoteInbounds) {
      if (remote.roundTripTime !== undefined) {
        rttSec = rttSec === undefined ? remote.roundTripTime : Math.max(rttSec, remote.roundTripTime);
      }
    }
  }
  if (rttSec === undefined) {
    rttSec = candidatePair?.currentRoundTripTime
      ?? candidatePairs.find(
        (pair) => pair.state !== "failed" && pair.currentRoundTripTime !== undefined,
      )?.currentRoundTripTime;
  }
  const rttMs = rttSec !== undefined ? Math.round(rttSec * 1000) : null;

  return {
    summary: {
      codec,
      bitrateKbps,
      packetLossPct,
      rttMs,
      iceCandidateType,
      iceTransport,
    },
    sample,
  };
}

function clampPercent(value: number): number {
  if (!Number.isFinite(value) || value < 0) return 0;
  if (value > 100) return 100;
  return Math.round(value * 10) / 10;
}

/**
 * Reduce one track's `RTCStatsReport` to a `VideoStatsSummary` plus the
 * fresh `CallStatSample` to retain for next time. `direction` selects the
 * outbound (local publish) vs inbound (remote subscribe) RTP stream.
 *
 * Simulcast matters here: a published camera/screen with simulcast on
 * produces ONE `outbound-rtp` entry per layer, so the byte/packet
 * counters are aggregated across layers (sum) and the resolution/fps are
 * taken from the largest active layer — picking a single arbitrary entry
 * would under-report bitrate and show a random layer's size. A receiver
 * decodes one forwarded layer, so the inbound side is naturally a single
 * entry, but the same aggregation handles it correctly.
 *
 * Bitrate is computed by diffing the summed byte counter against `prev`;
 * on the first poll (`prev` undefined) it is null and fills in next tick.
 */
export function summarizeVideoStats(
  report: RTCStatsReport,
  direction: CallStatDirection,
  prev?: CallStatSample,
): { summary: VideoStatsSummary; sample: CallStatSample | null } {
  const mainType = direction === "send" ? "outbound-rtp" : "inbound-rtp";
  const mains: RtpLike[] = [];
  const remoteInbounds: RtpLike[] = [];
  const candidatePairs: RtpLike[] = [];
  let selectedPairId: string | undefined;
  // Index every entry by its own `id` so RTP→codec and pair→candidate
  // references resolve without depending on how the report Map is keyed.
  const byId = new Map<string, RtpLike>();

  report.forEach((raw: unknown) => {
    const stat = raw as RtpLike;
    if (stat.id !== undefined) byId.set(stat.id, stat);
    if (stat.type === mainType && isVideo(stat)) {
      mains.push(stat);
    } else if (stat.type === "remote-inbound-rtp" && isVideo(stat)) {
      remoteInbounds.push(stat);
    } else if (stat.type === "candidate-pair") {
      candidatePairs.push(stat);
    } else if (stat.type === "transport" && stat.selectedCandidatePairId !== undefined) {
      selectedPairId = stat.selectedCandidatePairId;
    }
  });
  const candidatePair = selectCandidatePair(candidatePairs, selectedPairId);
  const { iceCandidateType, iceTransport } = iceCandidatePath(candidatePair, byId);

  // Resolution + fps from the top (largest-area) active layer.
  let top: RtpLike | undefined;
  for (const main of mains) {
    if (!main.frameWidth || !main.frameHeight) continue;
    const area = main.frameWidth * main.frameHeight;
    const topArea = top?.frameWidth && top?.frameHeight ? top.frameWidth * top.frameHeight : 0;
    if (area > topArea) top = main;
  }
  const resolution =
    top?.frameWidth && top?.frameHeight ? `${top.frameWidth}×${top.frameHeight}` : null;
  const fps = top?.framesPerSecond !== undefined ? Math.round(top.framesPerSecond) : null;

  // All simulcast layers share one codec, so the first RTP entry that
  // names a `codecId` resolves it; prefer the top layer's reference. Pin to a
  // genuine `codec` entry so a stray/duplicate id can't surface a wrong MIME.
  const codecRef = top?.codecId ?? mains.find((main) => main.codecId !== undefined)?.codecId;
  const codecEntry = codecRef !== undefined ? byId.get(codecRef) : undefined;
  const codec = codecEntry?.type === "codec" ? codecName(codecEntry.mimeType) : null;

  // Bytes summed across all simulcast layers; timestamp is the newest layer's.
  const sample = sampleFromRtp(mains, direction === "send" ? "bytesSent" : "bytesReceived");
  const bitrateKbps = bitrateFromSamples(sample, prev);

  const packetLossPct =
    direction === "recv"
      ? recvPacketLoss(mains)
      : sendPacketLoss(mains, remoteInbounds);

  let rttSec: number | undefined;
  if (direction === "send") {
    // RTT is a path property (≈equal across layers); take the largest
    // reported remote-inbound RTT, falling back to the candidate pair.
    for (const remote of remoteInbounds) {
      if (remote.roundTripTime !== undefined) {
        rttSec = rttSec === undefined ? remote.roundTripTime : Math.max(rttSec, remote.roundTripTime);
      }
    }
  }
  // Prefer the active pair's RTT; fall back to any non-failed pair that
  // reported one so a momentarily RTT-less selected pair doesn't blank an
  // otherwise-known RTT — but never surface a failed pair's stale round-trip.
  if (rttSec === undefined) {
    rttSec =
      candidatePair?.currentRoundTripTime ??
      candidatePairs.find(
        (pair) => pair.state !== "failed" && pair.currentRoundTripTime !== undefined,
      )?.currentRoundTripTime;
  }
  const rttMs = rttSec !== undefined ? Math.round(rttSec * 1000) : null;

  return {
    summary: { resolution, fps, bitrateKbps, packetLossPct, rttMs, codec, iceCandidateType, iceTransport },
    sample,
  };
}

/** Inbound loss: summed lost / (lost + received) across decoded entries. */
function recvPacketLoss(mains: RtpLike[]): number | null {
  let lost = 0;
  let received = 0;
  let have = false;
  for (const main of mains) {
    if (main.packetsLost !== undefined) {
      lost += main.packetsLost;
      have = true;
    }
    if (main.packetsReceived !== undefined) {
      received += main.packetsReceived;
      have = true;
    }
  }
  if (!have) return null;
  const total = lost + received;
  return total > 0 ? clampPercent((lost / total) * 100) : 0;
}

/** Outbound loss: summed remote-reported lost / summed packets sent. */
function sendPacketLoss(mains: RtpLike[], remoteInbounds: RtpLike[]): number | null {
  let lost = 0;
  let haveLost = false;
  for (const remote of remoteInbounds) {
    if (remote.packetsLost !== undefined) {
      lost += remote.packetsLost;
      haveLost = true;
    }
  }
  let sent = 0;
  for (const main of mains) {
    if (main.packetsSent !== undefined) sent += main.packetsSent;
  }
  if (!haveLost || sent <= 0) return null;
  return clampPercent((lost / sent) * 100);
}

/** Format kbps as a compact "x.y Mbps" / "n kbps" string. */
export function formatBitrate(bitrateKbps: number | null): string {
  if (bitrateKbps === null) return "—";
  if (bitrateKbps >= 1000) return `${(bitrateKbps / 1000).toFixed(1)} Mbps`;
  return `${bitrateKbps} kbps`;
}

/** Turn a numeric summary into display strings (null → em dash). */
export function describeVideoStats(summary: VideoStatsSummary): VideoStatsDisplay {
  return {
    resolution: summary.resolution ?? "—",
    fps: summary.fps !== null ? `${summary.fps} fps` : "—",
    bitrate: formatBitrate(summary.bitrateKbps),
    loss: summary.packetLossPct !== null ? `${summary.packetLossPct}%` : "—",
    rtt: summary.rttMs !== null ? `${summary.rttMs} ms` : "—",
    codec: summary.codec ?? "—",
    icePath: formatIcePath(summary.iceCandidateType, summary.iceTransport),
  };
}

/** "type · transport" (e.g. "relay · tcp"); type alone if transport is unknown. */
function formatIcePath(
  candidateType: IceCandidateType | null,
  transport: IceTransport | null,
): string {
  if (candidateType === null) return "—";
  return transport !== null ? `${candidateType} · ${transport}` : candidateType;
}
