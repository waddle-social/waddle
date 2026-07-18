import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  __resetCallLifecycleTelemetryForTesting,
  beginCallAttempt,
  durationBucket,
  finishCallAttempt,
  finishCallAttemptForTransportDisconnect,
  markCallAttemptAccepted,
  observeCallConnectionPhase,
  observeCallConnectionQuality,
  observeCallStats,
  packetLossBand,
  reconnectCountBucket,
  reportDeclinedCallAttempt,
  reportFailedCallAttempt,
  rttBand,
} from "../src/lib/calls/call-lifecycle-telemetry";
import { __setFaroForTesting } from "../src/lib/telemetry";

function createFaroStub() {
  const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
  return {
    events,
    api: {
      pushEvent(name: string, attributes?: Record<string, string>) {
        events.push({ name, attributes });
      },
    },
  };
}

beforeEach(() => {
  __resetCallLifecycleTelemetryForTesting();
  __setFaroForTesting(null);
});

afterEach(() => {
  __setFaroForTesting(null);
});

describe("call lifecycle mappings", () => {
  test("maps duration, RTT, and packet loss into coarse bands", () => {
    expect(durationBucket(0)).toBe("none");
    expect(durationBucket(59_999)).toBe("under-1m");
    expect(durationBucket(60_000)).toBe("1m-10m");
    expect(rttBand(null)).toBe("unknown");
    expect(rttBand(100)).toBe("100ms-300ms");
    expect(rttBand(301)).toBe("over-300ms");
    expect(packetLossBand(0.5)).toBe("under-1pct");
    expect(packetLossBand(5)).toBe("1pct-5pct");
    expect(packetLossBand(5.1)).toBe("over-5pct");
    expect(reconnectCountBucket(0)).toBe("none");
    expect(reconnectCountBucket(1)).toBe("once");
    expect(reconnectCountBucket(2)).toBe("multiple");
  });

  test("emits one declined DM setup outcome for an unaccepted attempt", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    beginCallAttempt("dm-1", "dm");

    const first = finishCallAttempt(
      "dm-1",
      { setupOutcome: "declined", endReason: "peer-left" },
      1_000,
    );
    const duplicate = finishCallAttempt(
      "dm-1",
      { setupOutcome: "failed", endReason: "error" },
      2_000,
    );

    expect(first).toMatchObject({
      setupOutcome: "declined",
      endReason: "peer-left",
      callKind: "dm",
      durationBucket: "none",
    });
    expect(duplicate).toBeNull();
    expect(stub.events).toEqual([{
      name: "chat.call.lifecycle",
      attributes: {
        setup_outcome: "declined",
        end_reason: "peer-left",
        duration_bucket: "none",
        call_kind: "dm",
        rtt_band: "unknown",
        packet_loss_band: "unknown",
        connection_quality: "unknown",
        reconnect_count: "none",
      },
    }]);
  });

  test("records a busy declined proposal without replacing the active attempt", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    beginCallAttempt("active", "dm");
    markCallAttemptAccepted("active");

    reportDeclinedCallAttempt("busy-proposal");
    finishCallAttempt("active", { endReason: "hangup" });

    expect(stub.events.map((event) => event.attributes)).toEqual([
      expect.objectContaining({
        setup_outcome: "declined",
        end_reason: "hangup",
        call_kind: "dm",
      }),
      expect.objectContaining({ setup_outcome: "accepted", end_reason: "hangup" }),
    ]);
  });

  test("classifies transport loss from observed reconnect state and permits explicit SID reuse", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    beginCallAttempt("restored-sid", "dm");
    markCallAttemptAccepted("restored-sid");
    expect(finishCallAttemptForTransportDisconnect("restored-sid")?.endReason).toBe("error");

    beginCallAttempt("restored-sid", "dm");
    markCallAttemptAccepted("restored-sid");
    observeCallConnectionPhase("reconnecting");
    expect(finishCallAttemptForTransportDisconnect("restored-sid")?.endReason)
      .toBe("reconnect-exhausted");

    expect(stub.events).toHaveLength(2);
  });

  test("does not call a later transport loss exhausted after a successful reconnect", () => {
    beginCallAttempt("dm-recovered", "dm");
    markCallAttemptAccepted("dm-recovered");
    observeCallConnectionPhase("reconnecting");
    observeCallConnectionPhase("connected");

    expect(finishCallAttemptForTransportDisconnect("dm-recovered")?.endReason).toBe("error");
  });

  test("keeps duration and quality owned by each sequential attempt", () => {
    beginCallAttempt("first", "dm");
    markCallAttemptAccepted("first", 1_000);
    observeCallConnectionQuality("poor");
    finishCallAttempt("first", { endReason: "hangup" }, 91_000);

    beginCallAttempt("second", "dm");
    const second = finishCallAttempt(
      "second",
      { setupOutcome: "failed", endReason: "error" },
      100_000,
    );

    expect(second).toMatchObject({
      durationBucket: "none",
      connectionQuality: "unknown",
    });
  });

  test("emits a standalone failed MUC preflight exactly once", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    reportFailedCallAttempt("muc-preflight", "muc");
    reportFailedCallAttempt("muc-preflight", "muc");

    expect(stub.events).toEqual([{
      name: "chat.call.lifecycle",
      attributes: expect.objectContaining({
        setup_outcome: "failed",
        end_reason: "error",
        call_kind: "muc",
      }),
    }]);
  });

  test("accepted MUC calls retain call_kind, end reason, and end-of-call quality", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    beginCallAttempt("muc-1", "muc");
    markCallAttemptAccepted("muc-1", 1_000);
    observeCallStats({ rttMs: 340, packetLossPct: 6.2 });
    observeCallConnectionQuality("poor");
    observeCallConnectionPhase("connected");
    observeCallConnectionPhase("reconnecting");
    observeCallConnectionPhase("reconnecting");
    observeCallConnectionPhase("connected");

    const payload = finishCallAttempt(
      "muc-1",
      { setupOutcome: "failed", endReason: "hangup" },
      91_000,
    );

    expect(payload).toEqual({
      setupOutcome: "accepted",
      endReason: "hangup",
      durationBucket: "1m-10m",
      callKind: "muc",
      rttBand: "over-300ms",
      packetLossBand: "over-5pct",
      connectionQuality: "poor",
      reconnectCount: "once",
    });
    expect(stub.events[0]?.attributes).toMatchObject({
      setup_outcome: "accepted",
      end_reason: "hangup",
      call_kind: "muc",
      reconnect_count: "once",
    });
  });
});
