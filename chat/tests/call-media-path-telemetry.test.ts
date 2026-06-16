import { describe, expect, test } from "bun:test";
import {
  callMediaPathEventAttributes,
  createCallMediaPathBeacon,
  type CallMediaPathSnapshot,
} from "../src/lib/calls/call-media-path-telemetry";

const RELAY_TCP: CallMediaPathSnapshot = {
  direction: "send",
  source: "screen",
  codec: "VP9",
  iceCandidateType: "relay",
  iceTransport: "tcp",
};

describe("callMediaPathEventAttributes", () => {
  test("maps a snapshot to flat, low-cardinality attributes", () => {
    expect(callMediaPathEventAttributes(RELAY_TCP)).toEqual({
      direction: "send",
      source: "screen",
      codec: "VP9",
      ice_candidate_type: "relay",
      ice_transport: "tcp",
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
      }),
    ).toEqual({
      direction: "recv",
      source: "camera",
      codec: "unknown",
      ice_candidate_type: "unknown",
      ice_transport: "unknown",
    });
  });

  test("never emits participant identifiers or JIDs", () => {
    const attrs = callMediaPathEventAttributes(RELAY_TCP);
    const allowed = new Set([
      "direction",
      "source",
      "codec",
      "ice_candidate_type",
      "ice_transport",
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
