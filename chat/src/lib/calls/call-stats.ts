/**
 * Pure WebRTC-stats parsing for the call diagnostics section.
 *
 * LiveKit exposes `publication.getRTCStatsReport()` per track; this
 * module turns one such `RTCStatsReport` into a small, display-ready
 * summary. Bitrate is a rate, so it needs two samples — the caller keeps
 * the previous `CallStatSample` per track and passes it back in.
 *
 * Everything here is pure (no timers, no LiveKit, no DOM beyond the
 * standard `RTCStatsReport` shape) so it is unit-testable in isolation;
 * the polling lifecycle lives in the dialog that owns it.
 */

export type CallStatDirection = "send" | "recv";

/** Byte/timestamp pair carried between polls to derive a bitrate. */
export type CallStatSample = { bytes: number; timestampMs: number };

export type VideoStatsSummary = {
  /** "1920×1080", or null when the track isn't reporting frames yet. */
  resolution: string | null;
  fps: number | null;
  bitrateKbps: number | null;
  packetLossPct: number | null;
  rttMs: number | null;
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
};

/**
 * Loose view over the RTCStats entries we read. The DOM lib types don't
 * cover every field consistently across browsers (e.g. `framesPerSecond`,
 * `frameWidth`), so we read through optional numbers rather than fight
 * the partial typings — values that are absent simply stay null.
 */
type RtpLike = {
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
  timestamp?: number;
};

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
 * Bitrate is computed by diffing byte counters against `prev`; on the
 * first poll (`prev` undefined) it is null and fills in on the next tick.
 */
export function summarizeVideoStats(
  report: RTCStatsReport,
  direction: CallStatDirection,
  prev?: CallStatSample,
): { summary: VideoStatsSummary; sample: CallStatSample | null } {
  const mainType = direction === "send" ? "outbound-rtp" : "inbound-rtp";
  let main: RtpLike | undefined;
  let remoteInbound: RtpLike | undefined;
  let candidatePair: RtpLike | undefined;

  report.forEach((raw: unknown) => {
    const stat = raw as RtpLike;
    if (stat.type === mainType && isVideo(stat)) {
      main = stat;
    } else if (stat.type === "remote-inbound-rtp" && isVideo(stat)) {
      remoteInbound = stat;
    } else if (stat.type === "candidate-pair" && stat.currentRoundTripTime !== undefined) {
      candidatePair = stat;
    }
  });

  const resolution =
    main?.frameWidth && main?.frameHeight ? `${main.frameWidth}×${main.frameHeight}` : null;
  const fps =
    main?.framesPerSecond !== undefined ? Math.round(main.framesPerSecond) : null;

  const bytes = direction === "send" ? main?.bytesSent : main?.bytesReceived;
  const timestampMs = main?.timestamp;
  const sample: CallStatSample | null =
    bytes !== undefined && timestampMs !== undefined ? { bytes, timestampMs } : null;

  let bitrateKbps: number | null = null;
  if (sample && prev && sample.timestampMs > prev.timestampMs) {
    const deltaBits = (sample.bytes - prev.bytes) * 8;
    const deltaSec = (sample.timestampMs - prev.timestampMs) / 1000;
    const kbps = deltaBits / deltaSec / 1000;
    bitrateKbps = kbps > 0 ? Math.round(kbps) : 0;
  }

  let packetLossPct: number | null = null;
  if (direction === "recv" && main?.packetsLost !== undefined && main?.packetsReceived !== undefined) {
    const total = main.packetsLost + main.packetsReceived;
    packetLossPct = total > 0 ? clampPercent((main.packetsLost / total) * 100) : 0;
  } else if (direction === "send" && remoteInbound?.packetsLost !== undefined && main?.packetsSent) {
    packetLossPct = clampPercent((remoteInbound.packetsLost / main.packetsSent) * 100);
  }

  let rttMs: number | null = null;
  const rttSec =
    direction === "send" && remoteInbound?.roundTripTime !== undefined
      ? remoteInbound.roundTripTime
      : candidatePair?.currentRoundTripTime;
  if (rttSec !== undefined) rttMs = Math.round(rttSec * 1000);

  return { summary: { resolution, fps, bitrateKbps, packetLossPct, rttMs }, sample };
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
  };
}
