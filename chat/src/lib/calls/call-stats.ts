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
  /** `candidate-pair` fields used to find and qualify the active path. */
  selected?: boolean;
  state?: string;
  localCandidateId?: string;
  /** `local-candidate` fields describing the path. */
  candidateType?: string;
  protocol?: string;
  relayProtocol?: string;
};

/** Strip the "video/" prefix off a codec MIME, e.g. "video/VP9" → "VP9". */
function codecName(mimeType: string | undefined): string | null {
  if (!mimeType) return null;
  const slash = mimeType.indexOf("/");
  const name = slash >= 0 ? mimeType.slice(slash + 1) : mimeType;
  return name.length > 0 ? name : null;
}

/**
 * Pick the active candidate pair: the nominated/selected/succeeded one if
 * present, else any pair carrying an RTT (a path that at least probed), else
 * the first. A failed pair can still report fields, so it must never shadow
 * the path media is actually flowing over.
 */
function selectCandidatePair(pairs: RtpLike[]): RtpLike | undefined {
  return (
    pairs.find((pair) => pair.nominated) ??
    pairs.find((pair) => pair.selected) ??
    pairs.find((pair) => pair.state === "succeeded") ??
    pairs.find((pair) => pair.currentRoundTripTime !== undefined) ??
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
  const rawTransport =
    iceCandidateType === "relay" ? local?.relayProtocol ?? local?.protocol : local?.protocol;
  return { iceCandidateType, iceTransport: normalizeIceTransport(rawTransport) };
}

function isVideo(stat: RtpLike): boolean {
  return stat.kind === "video" || stat.mediaType === "video";
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
    }
  });
  const candidatePair = selectCandidatePair(candidatePairs);
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
  // names a `codecId` resolves it; prefer the top layer's reference.
  const codecRef = top?.codecId ?? mains.find((main) => main.codecId !== undefined)?.codecId;
  const codec = codecRef !== undefined ? codecName(byId.get(codecRef)?.mimeType) : null;

  // Bytes summed across all layers; timestamp is the newest layer's.
  const bytesField = direction === "send" ? "bytesSent" : "bytesReceived";
  let bytes: number | undefined;
  let timestampMs: number | undefined;
  for (const main of mains) {
    const layerBytes = main[bytesField];
    if (layerBytes !== undefined) bytes = (bytes ?? 0) + layerBytes;
    if (main.timestamp !== undefined && (timestampMs === undefined || main.timestamp > timestampMs)) {
      timestampMs = main.timestamp;
    }
  }
  const sample: CallStatSample | null =
    bytes !== undefined && timestampMs !== undefined ? { bytes, timestampMs } : null;

  let bitrateKbps: number | null = null;
  if (sample && prev && sample.timestampMs > prev.timestampMs) {
    const deltaBits = (sample.bytes - prev.bytes) * 8;
    const deltaSec = (sample.timestampMs - prev.timestampMs) / 1000;
    const kbps = deltaBits / deltaSec / 1000;
    bitrateKbps = kbps > 0 ? Math.round(kbps) : 0;
  }

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
  if (rttSec === undefined) rttSec = candidatePair?.currentRoundTripTime;
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
