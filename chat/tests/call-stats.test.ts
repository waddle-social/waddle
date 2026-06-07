import { describe, expect, test } from "bun:test";
import {
  describeVideoStats,
  formatBitrate,
  summarizeVideoStats,
  type CallStatSample,
} from "../src/lib/calls/call-stats";

/**
 * Build a stand-in `RTCStatsReport`. The real report is a
 * `ReadonlyMap<string, any>`, and `summarizeVideoStats` only uses
 * `forEach`, so a plain `Map` is a faithful structural stub.
 */
function report(entries: Array<Record<string, unknown>>): RTCStatsReport {
  const map = new Map<string, unknown>();
  entries.forEach((entry, index) => map.set(`${entry.type}-${index}`, entry));
  return map as unknown as RTCStatsReport;
}

describe("summarizeVideoStats — outbound (send) video", () => {
  const outbound = {
    type: "outbound-rtp",
    kind: "video",
    frameWidth: 1920,
    frameHeight: 1080,
    framesPerSecond: 30,
    bytesSent: 1_000_000,
    packetsSent: 1000,
    timestamp: 10_000,
  };
  const remoteInbound = {
    type: "remote-inbound-rtp",
    kind: "video",
    packetsLost: 5,
    roundTripTime: 0.024,
  };

  test("reads resolution, fps, loss and rtt; bitrate is null on the first sample", () => {
    const { summary, sample } = summarizeVideoStats(report([outbound, remoteInbound]), "send");
    expect(summary.resolution).toBe("1920×1080");
    expect(summary.fps).toBe(30);
    expect(summary.bitrateKbps).toBeNull(); // needs two samples
    expect(summary.packetLossPct).toBe(0.5); // 5 / 1000 * 100
    expect(summary.rttMs).toBe(24); // 0.024s prefers the remote-inbound RTT
    expect(sample).toEqual({ bytes: 1_000_000, timestampMs: 10_000 });
  });

  test("derives bitrate from the byte/timestamp delta against the previous sample", () => {
    const prev: CallStatSample = { bytes: 1_000_000, timestampMs: 10_000 };
    const next = { ...outbound, bytesSent: 1_375_000, timestamp: 11_000 };
    // +375,000 bytes = 3,000,000 bits over 1.0s = 3000 kbps.
    const { summary } = summarizeVideoStats(report([next, remoteInbound]), "send", prev);
    expect(summary.bitrateKbps).toBe(3000);
    expect(describeVideoStats(summary).bitrate).toBe("3.0 Mbps");
  });
});

describe("summarizeVideoStats — inbound (recv) video", () => {
  test("computes packet loss from received vs lost and rtt from the candidate pair", () => {
    const inbound = {
      type: "inbound-rtp",
      kind: "video",
      frameWidth: 1280,
      frameHeight: 720,
      framesPerSecond: 30,
      bytesReceived: 500_000,
      packetsReceived: 990,
      packetsLost: 10,
      timestamp: 5_000,
    };
    const candidatePair = { type: "candidate-pair", currentRoundTripTime: 0.06 };
    const { summary } = summarizeVideoStats(report([inbound, candidatePair]), "recv");
    expect(summary.resolution).toBe("1280×720");
    expect(summary.packetLossPct).toBe(1); // 10 / (10 + 990) * 100
    expect(summary.rttMs).toBe(60);
  });

  test("a track not yet reporting frames yields nulls, rendered as em dashes", () => {
    const { summary } = summarizeVideoStats(report([]), "recv");
    expect(summary).toEqual({
      resolution: null,
      fps: null,
      bitrateKbps: null,
      packetLossPct: null,
      rttMs: null,
    });
    expect(describeVideoStats(summary)).toEqual({
      resolution: "—",
      fps: "—",
      bitrate: "—",
      loss: "—",
      rtt: "—",
    });
  });
});

describe("formatBitrate", () => {
  test("switches to Mbps at/above 1000 kbps and renders null as an em dash", () => {
    expect(formatBitrate(null)).toBe("—");
    expect(formatBitrate(0)).toBe("0 kbps");
    expect(formatBitrate(450)).toBe("450 kbps");
    expect(formatBitrate(1000)).toBe("1.0 Mbps");
    expect(formatBitrate(2800)).toBe("2.8 Mbps");
  });
});
