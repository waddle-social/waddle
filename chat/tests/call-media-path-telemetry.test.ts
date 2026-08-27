import { describe, expect, test } from "bun:test";
import {
  callMediaPathEventAttributes,
  createCallMediaPathBeacon,
  telemetryMediaCodec,
  type CallMediaPathSnapshot,
} from "../src/lib/calls/call-media-path-telemetry";

const RELAY_TCP: CallMediaPathSnapshot = {
  direction: "send",
  source: "screen",
  codec: "VP9",
  iceCandidateType: "relay",
  iceTransport: "tcp",
  audioBitrateBand: null,
  videoResolutionBand: "1440p",
};

describe("callMediaPathEventAttributes", () => {
  test("normalizes arbitrary runtime codec names to the closed unknown value", () => {
    expect(telemetryMediaCodec("private-codec-with-user-tag")).toBe("unknown");
    expect(telemetryMediaCodec("video/vp9")).toBe("VP9");
  });

  test("maps a snapshot to flat, low-cardinality attributes", () => {
    expect(callMediaPathEventAttributes(RELAY_TCP)).toEqual({
      direction: "send",
      source: "screen",
      codec: "VP9",
      ice_candidate_type: "relay",
      ice_transport: "tcp",
      video_resolution_band: "1440p",
    });
  });

  test("renders unknown fields as 'unknown' rather than dropping them", () => {
    expect(
      callMediaPathEventAttributes({
        direction: "recv",
        source: "camera",
        codec: null,
        iceCandidateType: null,
        iceTransport: null,
        audioBitrateBand: null,
        videoResolutionBand: null,
      }),
    ).toEqual({
      direction: "recv",
      source: "camera",
      codec: "unknown",
      ice_candidate_type: "unknown",
      ice_transport: "unknown",
    });
  });

  test("a camera path carries its resolution band — the 720p-vs-1080p egress signal", () => {
    expect(
      callMediaPathEventAttributes({
        direction: "send",
        source: "camera",
        codec: "VP9",
        iceCandidateType: "host",
        iceTransport: "udp",
        audioBitrateBand: null,
        videoResolutionBand: "720p",
      }),
    ).toMatchObject({ source: "camera", codec: "VP9", video_resolution_band: "720p" });
  });

  test("an audio path carries the bitrate band (active-speaker signal), codec and source", () => {
    expect(
      callMediaPathEventAttributes({
        direction: "send",
        source: "microphone",
        codec: "opus",
        iceCandidateType: "host",
        iceTransport: "udp",
        audioBitrateBand: "high",
        videoResolutionBand: null,
      }),
    ).toEqual({
      direction: "send",
      source: "microphone",
      codec: "opus",
      ice_candidate_type: "host",
      ice_transport: "udp",
      audio_bitrate_band: "high",
    });
  });

  test("a video path omits the audio_bitrate_band attribute entirely", () => {
    expect(callMediaPathEventAttributes(RELAY_TCP)).not.toHaveProperty("audio_bitrate_band");
  });

  test("an audio path omits the video_resolution_band attribute entirely", () => {
    expect(
      callMediaPathEventAttributes({
        direction: "send",
        source: "microphone",
        codec: "opus",
        iceCandidateType: "host",
        iceTransport: "udp",
        audioBitrateBand: "high",
        videoResolutionBand: null,
      }),
    ).not.toHaveProperty("video_resolution_band");
  });

  test("never emits participant identifiers or JIDs", () => {
    const attrs = callMediaPathEventAttributes(RELAY_TCP);
    const allowed = new Set([
      "direction",
      "source",
      "codec",
      "ice_candidate_type",
      "ice_transport",
      "audio_bitrate_band",
      "video_resolution_band",
    ]);
    for (const key of Object.keys(attrs)) {
      expect(allowed.has(key)).toBe(true);
    }
    const serialized = JSON.stringify(attrs).toLowerCase();
    expect(serialized).not.toContain("@");
    expect(serialized).not.toContain("identity");
    expect(serialized).not.toContain("jid");
  });
});

describe("createCallMediaPathBeacon", () => {
  const snap = (over: Partial<CallMediaPathSnapshot> = {}): CallMediaPathSnapshot => ({
    ...RELAY_TCP,
    ...over,
  });

  test("reports a snapshot once, not again for an equal recompute", () => {
    const reported: CallMediaPathSnapshot[] = [];
    const beacon = createCallMediaPathBeacon((s) => reported.push(s));

    beacon.observe(snap());
    beacon.observe(snap());

    expect(reported).toHaveLength(1);
  });

  test("re-beacons when the negotiated codec changes", () => {
    const reported: CallMediaPathSnapshot[] = [];
    const beacon = createCallMediaPathBeacon((s) => reported.push(s));

    beacon.observe(snap({ codec: "VP9" }));
    beacon.observe(snap({ codec: "VP8" }));

    expect(reported).toHaveLength(2);
    expect(reported[1]?.codec).toBe("VP8");
  });

  test("re-beacons when the audio bitrate band changes (silent → high speaker)", () => {
    const reported: CallMediaPathSnapshot[] = [];
    const beacon = createCallMediaPathBeacon((s) => reported.push(s));
    const audio = (band: "silent" | "standard" | "high"): CallMediaPathSnapshot => ({
      direction: "send",
      source: "microphone",
      codec: "opus",
      iceCandidateType: "host",
      iceTransport: "udp",
      audioBitrateBand: band,
      videoResolutionBand: null,
    });

    beacon.observe(audio("silent"));
    beacon.observe(audio("high"));
    beacon.observe(audio("silent")); // back to a band already seen → no re-beacon

    expect(reported.map((s) => s.audioBitrateBand)).toEqual(["silent", "high"]);
  });

  test("re-beacons when the camera resolution band changes (1080p → 720p cap)", () => {
    const reported: CallMediaPathSnapshot[] = [];
    const beacon = createCallMediaPathBeacon((s) => reported.push(s));

    beacon.observe(snap({ source: "camera", videoResolutionBand: "1080p" }));
    beacon.observe(snap({ source: "camera", videoResolutionBand: "720p" }));

    expect(reported).toHaveLength(2);
    expect(reported.map((s) => s.videoResolutionBand)).toEqual(["1080p", "720p"]);
  });

  test("re-beacons when the ICE path changes (relay → host)", () => {
    const reported: CallMediaPathSnapshot[] = [];
    const beacon = createCallMediaPathBeacon((s) => reported.push(s));

    beacon.observe(snap({ iceCandidateType: "relay", iceTransport: "tcp" }));
    beacon.observe(snap({ iceCandidateType: "host", iceTransport: "udp" }));

    expect(reported).toHaveLength(2);
  });

  test("treats send and recv of an otherwise-equal path as distinct", () => {
    const reported: CallMediaPathSnapshot[] = [];
    const beacon = createCallMediaPathBeacon((s) => reported.push(s));

    beacon.observe(snap({ direction: "send" }));
    beacon.observe(snap({ direction: "recv" }));

    expect(reported).toHaveLength(2);
  });

  test("does not re-beacon a snapshot already seen earlier in the same call", () => {
    const reported: CallMediaPathSnapshot[] = [];
    const beacon = createCallMediaPathBeacon((s) => reported.push(s));

    beacon.observe(snap({ codec: "VP9" }));
    beacon.observe(snap({ codec: "VP8" }));
    beacon.observe(snap({ codec: "VP9" }));

    expect(reported).toHaveLength(2);
  });

  test("reset re-arms the beacon for the next call", () => {
    const reported: CallMediaPathSnapshot[] = [];
    const beacon = createCallMediaPathBeacon((s) => reported.push(s));

    beacon.observe(snap());
    beacon.reset();
    beacon.observe(snap());

    expect(reported).toHaveLength(2);
  });
});
