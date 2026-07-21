import { describe, expect, test } from "bun:test";
import {
  audioBitrateBand,
  describeVideoStats,
  formatBitrate,
  summarizeAudioStats,
  summarizeVideoStats,
  videoResolutionBand,
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

  test("ignores a codecId that resolves to a non-codec entry", () => {
    const out = {
      type: "outbound-rtp",
      kind: "video",
      bytesSent: 1,
      timestamp: 1,
      codecId: "not-a-codec",
    };
    const stray = { type: "track", id: "not-a-codec", mimeType: "video/HACK" };
    const { summary } = summarizeVideoStats(report([out, stray]), "send");
    expect(summary.codec).toBeNull();
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

  test("prefers the transport's selectedCandidatePairId over a bare nominated flag", () => {
    // The spec allows several pairs to be `nominated`; only the one named by
    // RTCTransportStats.selectedCandidatePairId is the path media flows over.
    const relayPair = {
      type: "candidate-pair",
      id: "cp-relay",
      nominated: true,
      state: "succeeded",
      currentRoundTripTime: 0.09,
      localCandidateId: "lc-relay",
    };
    const hostPair = {
      type: "candidate-pair",
      id: "cp-host",
      nominated: true,
      state: "succeeded",
      currentRoundTripTime: 0.01,
      localCandidateId: "lc-host",
    };
    const transport = { type: "transport", id: "T1", selectedCandidatePairId: "cp-host" };
    const host = { type: "local-candidate", id: "lc-host", candidateType: "host", protocol: "udp" };
    const relay = {
      type: "local-candidate",
      id: "lc-relay",
      candidateType: "relay",
      protocol: "udp",
      relayProtocol: "tcp",
    };
    // Relay first, so a naive `find(nominated)` would wrongly pick it.
    const { summary } = summarizeVideoStats(
      report([inbound, relayPair, hostPair, transport, host, relay]),
      "recv",
    );
    expect(summary.iceCandidateType).toBe("host");
    expect(summary.iceTransport).toBe("udp");
    expect(summary.rttMs).toBe(10); // RTT from the actually-selected pair
  });

  test("falls back to Firefox's `selected` flag when no transport entry is present", () => {
    const other = {
      type: "candidate-pair",
      id: "cp-1",
      currentRoundTripTime: 0.2,
      localCandidateId: "lc-host",
    };
    const active = {
      type: "candidate-pair",
      id: "cp-2",
      selected: true,
      currentRoundTripTime: 0.02,
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
    const { summary } = summarizeVideoStats(report([inbound, other, active, host, relay]), "recv");
    expect(summary.iceCandidateType).toBe("relay");
    expect(summary.rttMs).toBe(20);
  });

  test("falls through to the quality chain when selectedCandidatePairId names a missing pair", () => {
    const pair = {
      type: "candidate-pair",
      id: "cp-real",
      state: "succeeded",
      currentRoundTripTime: 0.03,
      localCandidateId: "lc-host",
    };
    const transport = { type: "transport", id: "T1", selectedCandidatePairId: "cp-gone" };
    const host = { type: "local-candidate", id: "lc-host", candidateType: "host", protocol: "udp" };
    const { summary } = summarizeVideoStats(report([inbound, pair, transport, host]), "recv");
    expect(summary.iceCandidateType).toBe("host");
    expect(summary.rttMs).toBe(30);
  });

  test("borrows a sibling pair's RTT when the selected pair reports none this tick", () => {
    const selectedPair = { type: "candidate-pair", id: "cp-sel", localCandidateId: "lc-host" };
    const sibling = { type: "candidate-pair", id: "cp-other", currentRoundTripTime: 0.04 };
    const transport = { type: "transport", id: "T1", selectedCandidatePairId: "cp-sel" };
    const host = { type: "local-candidate", id: "lc-host", candidateType: "host", protocol: "udp" };
    const { summary } = summarizeVideoStats(
      report([inbound, selectedPair, sibling, transport, host]),
      "recv",
    );
    expect(summary.iceCandidateType).toBe("host"); // path from the selected pair
    expect(summary.rttMs).toBe(40); // RTT borrowed from the sibling
  });

  test("does not borrow RTT from a failed pair when a non-failed pair reports one", () => {
    // A failed pair can still carry an RTT; the borrow must skip it rather than
    // surface a misleading round-trip from a path media never settled on.
    const selectedPair = {
      type: "candidate-pair",
      id: "cp-sel",
      state: "succeeded",
      localCandidateId: "lc-host",
    };
    const failed = { type: "candidate-pair", id: "cp-failed", state: "failed", currentRoundTripTime: 0.5 };
    const healthy = { type: "candidate-pair", id: "cp-ok", currentRoundTripTime: 0.03 };
    const transport = { type: "transport", id: "T1", selectedCandidatePairId: "cp-sel" };
    const host = { type: "local-candidate", id: "lc-host", candidateType: "host", protocol: "udp" };
    const { summary } = summarizeVideoStats(
      report([inbound, failed, selectedPair, healthy, transport, host]),
      "recv",
    );
    expect(summary.rttMs).toBe(30); // from the healthy pair, not the failed 500ms one
  });

  test("a relay candidate with no relayProtocol reports a null transport, not a misleading udp", () => {
    // A relay candidate's own `protocol` is the server↔peer leg (≈always udp);
    // borrowing it would hide a real TURN/TCP leg. Absent relayProtocol → null.
    const pair = {
      type: "candidate-pair",
      nominated: true,
      state: "succeeded",
      currentRoundTripTime: 0.03,
      localCandidateId: "lc-relay",
    };
    const local = { type: "local-candidate", id: "lc-relay", candidateType: "relay", protocol: "udp" };
    const { summary } = summarizeVideoStats(report([inbound, pair, local]), "recv");
    expect(summary.iceCandidateType).toBe("relay");
    expect(summary.iceTransport).toBeNull();
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

describe("audioBitrateBand — coarse band over the measured on-the-wire audio rate", () => {
  test("a near-silent wire rate (DTX idle / comfort noise) reads 'silent'", () => {
    expect(audioBitrateBand(0)).toBe("silent");
    expect(audioBitrateBand(12)).toBe("silent"); // boundary
  });

  test("a light / quiet / intermittent wire rate reads 'standard'", () => {
    expect(audioBitrateBand(13)).toBe("standard"); // just past silence
    expect(audioBitrateBand(48)).toBe("standard");
    expect(audioBitrateBand(56)).toBe("standard"); // boundary
  });

  test("a sustained active-speaker wire rate reads 'high' (the raised default's signal)", () => {
    // Wire rate, not encoder target: a 64k Opus stream + RED + RTP overhead
    // measures above the encoder cap, so a real active speaker clears the floor.
    expect(audioBitrateBand(57)).toBe("high"); // just past the floor
    expect(audioBitrateBand(72)).toBe("high");
  });

  test("an unmeasured bitrate (null, first poll) has no band yet", () => {
    expect(audioBitrateBand(null)).toBeNull();
  });
});

describe("videoResolutionBand — coarse band over the negotiated top-layer resolution", () => {
  test("the camera's new 720p cap and the old 1080p baseline land in distinct bands", () => {
    // The egress signal for the #1001 camera lever: a fleet-wide shift from
    // "1080p" to "720p" is what proves the cap took effect and reduced egress.
    expect(videoResolutionBand("1280×720")).toBe("720p");
    expect(videoResolutionBand("1920×1080")).toBe("1080p");
  });

  test("the lower SVC layers a small tile pulls read as their own bands", () => {
    expect(videoResolutionBand("320×180")).toBe("180p");
    expect(videoResolutionBand("640×360")).toBe("360p");
  });

  test("a higher-than-1080p share (screen QHD) reads '1440p'", () => {
    expect(videoResolutionBand("2560×1440")).toBe("1440p");
  });

  test("an unreported resolution (null, first poll) has no band yet", () => {
    expect(videoResolutionBand(null)).toBeNull();
  });

  test("bands a real summarizeVideoStats resolution — guards the W×H separator coupling", () => {
    // End-to-end: the band classifier splits on the exact separator
    // summarizeVideoStats emits. If either side ever diverges (e.g. ASCII 'x'
    // vs U+00D7), every band would silently go null — this catches that.
    const camera720 = {
      type: "outbound-rtp",
      kind: "video",
      frameWidth: 1280,
      frameHeight: 720,
      framesPerSecond: 30,
      bytesSent: 500_000,
      packetsSent: 500,
      timestamp: 10_000,
    };
    const { summary } = summarizeVideoStats(report([camera720]), "send");
    expect(videoResolutionBand(summary.resolution)).toBe("720p");
  });
});

describe("summarizeAudioStats — outbound (send) audio", () => {
  const outbound = {
    type: "outbound-rtp",
    kind: "audio",
    bytesSent: 100_000,
    timestamp: 10_000,
    codecId: "RTCCodec_opus",
  };
  const opus = { type: "codec", id: "RTCCodec_opus", mimeType: "audio/opus" };

  test("reads the negotiated codec; bitrate is null on the first sample", () => {
    const { summary, sample } = summarizeAudioStats(report([outbound, opus]), "send");
    expect(summary.codec).toBe("opus");
    expect(summary.bitrateKbps).toBeNull(); // needs two samples
    expect(sample).toEqual({ bytes: 100_000, timestampMs: 10_000 });
  });

  test("derives the send bitrate from the byte/timestamp delta against the previous sample", () => {
    const prev: CallStatSample = { bytes: 100_000, timestampMs: 10_000 };
    // +8,000 bytes = 64,000 bits over 1.0s = 64 kbps on the wire → a sustained
    // active speaker on the raised default, so the band reads 'high'.
    const next = { ...outbound, bytesSent: 108_000, timestamp: 11_000 };
    const { summary } = summarizeAudioStats(report([next, opus]), "send", prev);
    expect(summary.bitrateKbps).toBe(64);
    expect(audioBitrateBand(summary.bitrateKbps)).toBe("high");
  });

  test("only looks at audio RTP — a co-reported video stream never bleeds in", () => {
    const prev: CallStatSample = { bytes: 100_000, timestampMs: 10_000 };
    const video = {
      type: "outbound-rtp",
      kind: "video",
      bytesSent: 5_000_000,
      timestamp: 11_000,
    };
    const next = { ...outbound, bytesSent: 108_000, timestamp: 11_000 };
    const { summary } = summarizeAudioStats(report([next, video, opus]), "send", prev);
    expect(summary.bitrateKbps).toBe(64); // not inflated by the 5 Mbps video
  });

  test("a genuine idle tick (bytes unchanged, clock advanced) reads 0 → 'silent'", () => {
    const prev: CallStatSample = { bytes: 100_000, timestampMs: 10_000 };
    const next = { ...outbound, bytesSent: 100_000, timestamp: 11_000 };
    const { summary } = summarizeAudioStats(report([next, opus]), "send", prev);
    expect(summary.bitrateKbps).toBe(0);
    expect(audioBitrateBand(summary.bitrateKbps)).toBe("silent");
  });

  test("a byte-counter reset (negative delta) is unmeasured (null), not a false 'silent'", () => {
    // A WebRTC counter reset (track replaced / ICE restart) must not beacon as
    // DTX silence — it is no measurement this tick, matching the first-poll null.
    const prev: CallStatSample = { bytes: 500_000, timestampMs: 10_000 };
    const next = { ...outbound, bytesSent: 1_000, timestamp: 11_000 };
    const { summary } = summarizeAudioStats(report([next, opus]), "send", prev);
    expect(summary.bitrateKbps).toBeNull();
    expect(audioBitrateBand(summary.bitrateKbps)).toBeNull();
  });
});

describe("summarizeAudioStats — inbound (recv) audio and ICE path", () => {
  test("derives the recv bitrate and reads the succeeded ICE candidate path", () => {
    const prev: CallStatSample = { bytes: 50_000, timestampMs: 5_000 };
    const inbound = {
      type: "inbound-rtp",
      kind: "audio",
      bytesReceived: 56_000, // +6,000 bytes = 48 kbps over 1.0s
      timestamp: 6_000,
      codecId: "c-opus",
    };
    const codec = { type: "codec", id: "c-opus", mimeType: "audio/opus" };
    const transport = { type: "transport", selectedCandidatePairId: "pair-1" };
    const pair = { type: "candidate-pair", id: "pair-1", state: "succeeded", localCandidateId: "lc-1" };
    const local = { type: "local-candidate", id: "lc-1", candidateType: "host", protocol: "udp" };
    const { summary } = summarizeAudioStats(
      report([inbound, codec, transport, pair, local]),
      "recv",
      prev,
    );
    expect(summary.bitrateKbps).toBe(48);
    expect(audioBitrateBand(summary.bitrateKbps)).toBe("standard");
    expect(summary.codec).toBe("opus");
    expect(summary.iceCandidateType).toBe("host");
    expect(summary.iceTransport).toBe("udp");
  });

  test("a track not yet flowing yields all-null fields", () => {
    const { summary } = summarizeAudioStats(report([]), "recv");
    expect(summary).toEqual({
      codec: null,
      bitrateKbps: null,
      packetLossPct: null,
      rttMs: null,
      iceCandidateType: null,
      iceTransport: null,
    });
  });

  test("captures packet loss and RTT for audio-only calls", () => {
    const inbound = {
      type: "inbound-rtp",
      kind: "audio",
      packetsReceived: 95,
      packetsLost: 5,
    };
    const pair = {
      type: "candidate-pair",
      state: "succeeded",
      currentRoundTripTime: 0.18,
    };

    const { summary } = summarizeAudioStats(report([inbound, pair]), "recv");

    expect(summary.packetLossPct).toBe(5);
    expect(summary.rttMs).toBe(180);
  });
});
