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

  test("aggregates simulcast layers: summed bitrate/loss, top-layer resolution", () => {
    // A 1080p simulcast publish reports one outbound-rtp PER layer; the
    // summary must sum byte/packet counters and show the TOP layer's size,
    // not whichever entry happens to be iterated last.
    const layers = [
      { type: "outbound-rtp", kind: "video", frameWidth: 480, frameHeight: 270, framesPerSecond: 30, bytesSent: 100_000, packetsSent: 100, timestamp: 11_000 },
      { type: "outbound-rtp", kind: "video", frameWidth: 960, frameHeight: 540, framesPerSecond: 30, bytesSent: 300_000, packetsSent: 300, timestamp: 11_000 },
      { type: "outbound-rtp", kind: "video", frameWidth: 1920, frameHeight: 1080, framesPerSecond: 30, bytesSent: 800_000, packetsSent: 800, timestamp: 11_000 },
    ];
    const remoteInbounds = [
      { type: "remote-inbound-rtp", kind: "video", packetsLost: 10 },
      { type: "remote-inbound-rtp", kind: "video", packetsLost: 5 },
    ];
    // Summed bytes = 1,200,000; prev summed = 1,050,000 → +150,000 bytes =
    // 1,200,000 bits over 1.0s = 1200 kbps. Loss = 15 / 1200 sent = 1.25%,
    // rounded to 1 decimal → 1.3%.
    const prev: CallStatSample = { bytes: 1_050_000, timestampMs: 10_000 };
    const { summary, sample } = summarizeVideoStats(report([...layers, ...remoteInbounds]), "send", prev);
    expect(summary.resolution).toBe("1920×1080");
    expect(summary.bitrateKbps).toBe(1200);
    expect(summary.packetLossPct).toBe(1.3);
    expect(sample).toEqual({ bytes: 1_200_000, timestampMs: 11_000 });
  });
});

describe("summarizeVideoStats — negotiated codec", () => {
  test("reads the codec name from the track's codec entry, dropping the media prefix", () => {
    const out = {
      type: "outbound-rtp",
      kind: "video",
      frameWidth: 1920,
      frameHeight: 1080,
      framesPerSecond: 30,
      bytesSent: 1_000_000,
      packetsSent: 1000,
      timestamp: 10_000,
      codecId: "RTCCodec_1_outbound_98",
    };
    const codec = { type: "codec", id: "RTCCodec_1_outbound_98", mimeType: "video/VP9" };
    const { summary } = summarizeVideoStats(report([out, codec]), "send");
    expect(summary.codec).toBe("VP9");
  });

  test("codec is null when no codec entry is referenced", () => {
    const out = {
      type: "outbound-rtp",
      kind: "video",
      frameWidth: 1280,
      frameHeight: 720,
      bytesSent: 1,
      timestamp: 1,
    };
    const { summary } = summarizeVideoStats(report([out]), "send");
    expect(summary.codec).toBeNull();
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
      codec: null,
      iceCandidateType: null,
      iceTransport: null,
    });
    expect(describeVideoStats(summary)).toEqual({
      resolution: "—",
      fps: "—",
      bitrate: "—",
      loss: "—",
      rtt: "—",
      codec: "—",
      icePath: "—",
    });
  });
});

describe("describeVideoStats — codec and ICE path", () => {
  const base = {
    resolution: "1280×720",
    fps: 30,
    bitrateKbps: 800,
    packetLossPct: 0,
    rttMs: 12,
  };

  test("shows the codec name and a 'type · transport' ICE path", () => {
    const display = describeVideoStats({
      ...base,
      codec: "VP9",
      iceCandidateType: "relay",
      iceTransport: "tcp",
    });
    expect(display.codec).toBe("VP9");
    expect(display.icePath).toBe("relay · tcp");
  });

  test("renders an em dash for a missing codec and falls back to type alone when transport is unknown", () => {
    const display = describeVideoStats({
      ...base,
      codec: null,
      iceCandidateType: "host",
      iceTransport: null,
    });
    expect(display.codec).toBe("—");
    expect(display.icePath).toBe("host");
  });
});

describe("summarizeVideoStats — ICE candidate path", () => {
  const inbound = {
    type: "inbound-rtp",
    kind: "video",
    frameWidth: 1280,
    frameHeight: 720,
    bytesReceived: 1,
    timestamp: 1,
  };

  test("a relay candidate over a TCP TURN leg reports relay + tcp (from relayProtocol)", () => {
    // The candidate's own `protocol` is udp (relayed media always leaves the
    // TURN server over UDP); the slow leg that "stuck on TCP relay" means is
    // the client↔TURN protocol, carried by `relayProtocol`.
    const pair = {
      type: "candidate-pair",
      nominated: true,
      currentRoundTripTime: 0.03,
      localCandidateId: "lc-relay",
    };
    const local = {
      type: "local-candidate",
      id: "lc-relay",
      candidateType: "relay",
      protocol: "udp",
      relayProtocol: "tcp",
    };
    const { summary } = summarizeVideoStats(report([inbound, pair, local]), "recv");
    expect(summary.iceCandidateType).toBe("relay");
    expect(summary.iceTransport).toBe("tcp");
  });

  test("a direct host candidate reports host + udp (from the candidate protocol)", () => {
    const pair = {
      type: "candidate-pair",
      nominated: true,
      currentRoundTripTime: 0.01,
      localCandidateId: "lc-host",
    };
    const local = { type: "local-candidate", id: "lc-host", candidateType: "host", protocol: "udp" };
    const { summary } = summarizeVideoStats(report([inbound, pair, local]), "recv");
    expect(summary.iceCandidateType).toBe("host");
    expect(summary.iceTransport).toBe("udp");
  });

  test("a server-reflexive candidate reports srflx", () => {
    const pair = {
      type: "candidate-pair",
      nominated: true,
      currentRoundTripTime: 0.02,
      localCandidateId: "lc-srflx",
    };
    const local = { type: "local-candidate", id: "lc-srflx", candidateType: "srflx", protocol: "udp" };
    const { summary } = summarizeVideoStats(report([inbound, pair, local]), "recv");
    expect(summary.iceCandidateType).toBe("srflx");
    expect(summary.iceTransport).toBe("udp");
  });

  test("reads the path from the nominated pair, ignoring an also-present failed pair", () => {
    const failed = {
      type: "candidate-pair",
      state: "failed",
      localCandidateId: "lc-host",
    };
    const nominated = {
      type: "candidate-pair",
      nominated: true,
      currentRoundTripTime: 0.05,
      localCandidateId: "lc-relay",
    };
    const host = { type: "local-candidate", id: "lc-host", candidateType: "host", protocol: "udp" };
    const relay = {
      type: "local-candidate",
      id: "lc-relay",
      candidateType: "relay",
      protocol: "udp",
      relayProtocol: "udp",
    };
    const { summary } = summarizeVideoStats(report([inbound, failed, nominated, host, relay]), "recv");
    expect(summary.iceCandidateType).toBe("relay");
    expect(summary.iceTransport).toBe("udp");
    expect(summary.rttMs).toBe(50); // RTT still comes from the nominated pair
  });

  test("a TLS TURN leg counts as tcp transport (non-UDP relay)", () => {
    const pair = {
      type: "candidate-pair",
      nominated: true,
      currentRoundTripTime: 0.04,
      localCandidateId: "lc-tls",
    };
    const local = {
      type: "local-candidate",
      id: "lc-tls",
      candidateType: "relay",
      protocol: "udp",
      relayProtocol: "tls",
    };
    const { summary } = summarizeVideoStats(report([inbound, pair, local]), "recv");
    expect(summary.iceCandidateType).toBe("relay");
    expect(summary.iceTransport).toBe("tcp");
  });

  test("ICE fields are null when no candidate pair is reported", () => {
    const { summary } = summarizeVideoStats(report([inbound]), "recv");
    expect(summary.iceCandidateType).toBeNull();
    expect(summary.iceTransport).toBeNull();
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
