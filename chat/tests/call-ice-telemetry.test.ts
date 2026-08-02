import { describe, expect, test } from "bun:test";
import {
  callIceEventAttributes,
  createCallIceBeacon,
  iceRestartBucket,
  iceStatsEntries,
  icePathClass,
  summarizeIceStats,
  type CallIceSnapshot,
} from "../src/lib/calls/call-ice-telemetry";

/** A report where a direct (srflx/udp) pair won and TURN was reachable. */
const DIRECT_WITH_TURN_AVAILABLE = [
  { id: "T1", type: "transport", selectedCandidatePairId: "P1" },
  { id: "P1", type: "candidate-pair", state: "succeeded", localCandidateId: "L1" },
  { id: "L1", type: "local-candidate", candidateType: "srflx", protocol: "udp" },
  { id: "L2", type: "local-candidate", candidateType: "relay", relayProtocol: "udp" },
];

/** A report where media is hairpinned through a TCP TURN relay. */
const RELAY_OVER_TCP = [
  { id: "T1", type: "transport", selectedCandidatePairId: "P1" },
  { id: "P1", type: "candidate-pair", state: "succeeded", localCandidateId: "L1" },
  { id: "L1", type: "local-candidate", candidateType: "relay", protocol: "udp", relayProtocol: "tcp" },
];

/** Gathering produced only host candidates — no TURN fallback exists. */
const NO_RELAY_GATHERED = [
  { id: "P1", type: "candidate-pair", state: "succeeded", nominated: true, localCandidateId: "L1" },
  { id: "L1", type: "local-candidate", candidateType: "host", protocol: "udp" },
];

describe("icePathClass", () => {
  test("only a relay candidate counts as a relayed path", () => {
    expect(icePathClass("relay")).toBe("relay");
    expect(icePathClass("host")).toBe("direct");
    expect(icePathClass("srflx")).toBe("direct");
    expect(icePathClass("prflx")).toBe("direct");
    expect(icePathClass(null)).toBe("unknown");
  });
});

describe("iceRestartBucket", () => {
  test("buckets a restart count into bounded labels", () => {
    expect(iceRestartBucket(0)).toBe("none");
    expect(iceRestartBucket(-1)).toBe("none");
    expect(iceRestartBucket(1)).toBe("once");
    expect(iceRestartBucket(3)).toBe("few");
    expect(iceRestartBucket(4)).toBe("many");
    expect(iceRestartBucket(9999)).toBe("many");
  });
});

describe("summarizeIceStats", () => {
  test("reports a direct path while still recording that TURN was reachable", () => {
    expect(summarizeIceStats(DIRECT_WITH_TURN_AVAILABLE, 0)).toEqual({
      pathClass: "direct",
      candidateType: "srflx",
      transport: "udp",
      turnReachability: "gathered",
      restartBucket: "none",
    });
  });

  test("reads a relay's transport from relayProtocol so a TCP fallback is visible", () => {
    // The candidate's own `protocol` says udp (the TURN↔SFU leg); only
    // `relayProtocol` reveals that the client↔TURN leg is TCP.
    expect(summarizeIceStats(RELAY_OVER_TCP, 0)).toMatchObject({
      pathClass: "relay",
      candidateType: "relay",
      transport: "tcp",
      turnReachability: "gathered",
    });
  });

  test("a direct win without a visible relay candidate is unknown, not a false alarm", () => {
    // Track-scoped getRTCStatsReport() can omit candidates unreferenced by
    // the selected pair, so a healthy direct call must not report
    // "no TURN fallback" (codex review, PR #1487).
    expect(summarizeIceStats(NO_RELAY_GATHERED, 0)).toMatchObject({
      pathClass: "direct",
      candidateType: "host",
      turnReachability: "unknown",
    });
  });

  test("a succeeded pair with unmeasurable candidate fields is unknown, not not-gathered", () => {
    // pathClass degrades to "unknown" when the selected local-candidate's
    // fields are missing, but the call IS working — that must not read as
    // the alert-worthy "no TURN fallback" (Qodo review, PR #1487).
    const succeededButUnmeasurable = [
      { id: "P1", type: "candidate-pair", state: "succeeded", localCandidateId: "L-gone" },
      { id: "L1", type: "local-candidate" },
    ];
    expect(summarizeIceStats(succeededButUnmeasurable, 0)).toMatchObject({
      pathClass: "unknown",
      turnReachability: "unknown",
    });
  });

  test("an explicitly failed selected pair with no relay reads not-gathered", () => {
    // transport.selectedCandidatePairId pointing at a FAILED pair is a
    // genuine no-path failure, not an established call (Qodo review,
    // PR #1487).
    const selectedButFailed = [
      { id: "T1", type: "transport", selectedCandidatePairId: "P1" },
      { id: "P1", type: "candidate-pair", state: "failed", localCandidateId: "L1" },
      { id: "L1", type: "local-candidate", candidateType: "host", protocol: "udp" },
    ];
    expect(summarizeIceStats(selectedButFailed, 0)).toMatchObject({
      turnReachability: "not-gathered",
    });
  });

  test("flags a client whose gathering produced candidates but no relay and no path", () => {
    const failedNoRelay = [
      { id: "P1", type: "candidate-pair", state: "failed", localCandidateId: "L1" },
      { id: "L1", type: "local-candidate", candidateType: "host", protocol: "udp" },
    ];
    expect(summarizeIceStats(failedNoRelay, 0)).toMatchObject({
      pathClass: "unknown",
      turnReachability: "not-gathered",
    });
  });

  test("an empty report is unknown rather than a fabricated direct path", () => {
    expect(summarizeIceStats([], 0)).toEqual({
      pathClass: "unknown",
      candidateType: null,
      transport: null,
      // No local-candidate entries were present at all, so TURN
      // reachability is unknowable — not the alertable "not-gathered".
      turnReachability: "unknown",
      restartBucket: "none",
    });
  });

  test("a nominated-but-failed pair never shadows the succeeded one", () => {
    const report = [
      { id: "PBad", type: "candidate-pair", state: "failed", nominated: true, localCandidateId: "LR" },
      { id: "PGood", type: "candidate-pair", state: "succeeded", localCandidateId: "LH" },
      { id: "LR", type: "local-candidate", candidateType: "relay", relayProtocol: "tcp" },
      { id: "LH", type: "local-candidate", candidateType: "host", protocol: "udp" },
    ];
    expect(summarizeIceStats(report, 0)).toMatchObject({
      pathClass: "direct",
      candidateType: "host",
    });
  });

  test("carries the caller's restart count through as a bucket", () => {
    expect(summarizeIceStats(NO_RELAY_GATHERED, 2).restartBucket).toBe("few");
  });
});

describe("callIceEventAttributes", () => {
  test("maps a snapshot to flat, low-cardinality, PII-free attributes", () => {
    const snapshot: CallIceSnapshot = {
      pathClass: "relay",
      candidateType: "relay",
      transport: "tcp",
      turnReachability: "gathered",
      restartBucket: "once",
    };
    expect(callIceEventAttributes(snapshot)).toEqual({
      ice_path: "relay",
      ice_candidate_type: "relay",
      ice_transport: "tcp",
      turn_reachability: "gathered",
      ice_restarts: "once",
    });
  });

  test("renders unmeasured fields as 'unknown' rather than dropping them", () => {
    const snapshot: CallIceSnapshot = {
      pathClass: "unknown",
      candidateType: null,
      transport: null,
      turnReachability: "not-gathered",
      restartBucket: "none",
    };
    expect(callIceEventAttributes(snapshot)).toEqual({
      ice_path: "unknown",
      ice_candidate_type: "unknown",
      ice_transport: "unknown",
      turn_reachability: "not-gathered",
      ice_restarts: "none",
    });
  });

  test("no attribute value can carry an address, port, or hostname", () => {
    const report = [
      { id: "T1", type: "transport", selectedCandidatePairId: "P1" },
      { id: "P1", type: "candidate-pair", state: "succeeded", localCandidateId: "L1" },
      {
        id: "L1",
        type: "local-candidate",
        candidateType: "relay",
        relayProtocol: "udp",
        address: "203.0.113.7",
        port: 49152,
        url: "turn:turn.waddle.social:3478",
      },
    ];
    const values = Object.values(callIceEventAttributes(summarizeIceStats(report, 0)));
    expect(values.join("|")).not.toContain("203.0.113.7");
    expect(values.join("|")).not.toContain("49152");
    expect(values.join("|")).not.toContain("turn.waddle.social");
  });
});

describe("createCallIceBeacon", () => {
  test("forwards credential refresh and expiry lifecycle beacons", () => {
    const credentials: string[] = [];
    const beacon = createCallIceBeacon(() => undefined, (event) => credentials.push(event));

    beacon.noteCredentials("refreshed");
    beacon.noteCredentials("expired");

    expect(credentials).toEqual(["refreshed", "expired"]);
  });

  test("a report that measured nothing is not beaconed", () => {
    const seen: CallIceSnapshot[] = [];
    const beacon = createCallIceBeacon((snapshot) => seen.push(snapshot));

    // Pre-gathering / mid-reconnect: no pair, no local candidates. A
    // beacon here would ship a false "no TURN path" signal.
    beacon.observe([]);
    expect(seen).toHaveLength(0);

    beacon.observe(DIRECT_WITH_TURN_AVAILABLE);
    expect(seen).toHaveLength(1);
  });

  test("beacons each distinct ICE state at most once per call", () => {
    const seen: CallIceSnapshot[] = [];
    const beacon = createCallIceBeacon((snapshot) => seen.push(snapshot));

    beacon.observe(DIRECT_WITH_TURN_AVAILABLE);
    beacon.observe(DIRECT_WITH_TURN_AVAILABLE);
    expect(seen).toHaveLength(1);

    // A re-route to a TURN relay is a genuinely new state.
    beacon.observe(RELAY_OVER_TCP);
    expect(seen).toHaveLength(2);
    expect(seen[1]?.pathClass).toBe("relay");
  });

  test("crossing a restart-bucket boundary re-beacons the same path", () => {
    const seen: CallIceSnapshot[] = [];
    const beacon = createCallIceBeacon((snapshot) => seen.push(snapshot));

    beacon.observe(RELAY_OVER_TCP);
    beacon.noteIceRestart();
    beacon.observe(RELAY_OVER_TCP);

    expect(seen.map((snapshot) => snapshot.restartBucket)).toEqual(["none", "once"]);
    expect(beacon.restartCount()).toBe(1);
  });

  test("reset re-arms the seen-set and zeroes the restart count", () => {
    const seen: CallIceSnapshot[] = [];
    const beacon = createCallIceBeacon((snapshot) => seen.push(snapshot));

    beacon.observe(RELAY_OVER_TCP);
    beacon.noteIceRestart();
    beacon.reset();
    expect(beacon.restartCount()).toBe(0);

    beacon.observe(RELAY_OVER_TCP);
    expect(seen).toHaveLength(2);
    expect(seen[1]?.restartBucket).toBe("none");
  });
});

describe("iceStatsEntries", () => {
  test("collects a Map-like RTCStatsReport into a plain iterable", () => {
    const report = new Map(
      DIRECT_WITH_TURN_AVAILABLE.map((stat) => [stat.id, stat] as const),
    );
    expect(iceStatsEntries(report)).toHaveLength(DIRECT_WITH_TURN_AVAILABLE.length);
    expect(summarizeIceStats(iceStatsEntries(report), 0).pathClass).toBe("direct");
  });
});
