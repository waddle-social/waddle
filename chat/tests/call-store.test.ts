import { describe, expect, test } from "bun:test";
import {
  $callState,
  $lastCallError,
  applyCallEvent,
  beginOutgoingCall,
  callLifecycleTerminalForRemoteEnd,
  clearCallState,
  clearLastCallError,
  reduceCallState,
  reportCallError,
} from "../src/lib/calls/call-store";
import {
  clearDmCallActivities,
  readDmCallActivity,
} from "../src/lib/calls/dm-call-activity";
import type { CallEvent, CallMedia, CallState, LiveKitJoin } from "../src/lib/calls/types";
import { __setFaroForTesting } from "../src/lib/telemetry";

const audioVideo: CallMedia = { audio: true, video: true };
const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: "alice@waddle.test::c1",
  identity: "bob@waddle.test/desktop",
  token: "eyJhbGc.payload.sig",
};

describe("call-store reducer", () => {
  test("propose transitions idle → incoming", () => {
    const next = reduceCallState({ phase: "idle" }, {
      kind: "propose",
      from: "alice@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
    });
    expect(next).toEqual({
      phase: "incoming",
      from: "alice@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
    });
  });

  test("propose is ignored when already in an active call", () => {
    const current = {
      phase: "active" as const,
      peer: "carol@waddle.test/desktop",
      sid: "c0",
      media: audioVideo,
      join,
      kind: "dm",
    };
    const next = reduceCallState(current, {
      kind: "propose",
      from: "dave@waddle.test/phone",
      sid: "c2",
      media: audioVideo,
    });
    expect(next).toBe(current);
  });

  test("session-initiate transitions incoming → active with credentials", () => {
    // Per XEP-0166 §6.2, session-initiate is only valid after the
    // responder has accepted via JMI proceed — so it MUST arrive
    // while the slot is `incoming`. Stale or out-of-order
    // session-initiate arriving while idle is dropped (see the sid
    // guards section below).
    const next = reduceCallState(
      {
        phase: "incoming",
        from: "alice@waddle.test/desktop",
        sid: "c1",
        media: audioVideo,
      },
      {
        kind: "session-initiate",
        from: "alice@waddle.test/desktop",
        sid: "c1",
        media: audioVideo,
        join,
      },
    );
    expect(next).toEqual({
      phase: "active",
      peer: "alice@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/desktop",
    });
  });

  test("session-terminate carries reason through", () => {
    const next = reduceCallState(
      {
        phase: "active",
        peer: "alice@waddle.test/desktop",
        sid: "c1",
        media: audioVideo,
        join,
      kind: "dm",
      },
      { kind: "session-terminate", from: "alice@waddle.test/desktop", sid: "c1", reason: "success" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c1", reason: "success" });
  });

  test("session-terminate without serialized reason ends with null reason", () => {
    const next = reduceCallState(
      {
        phase: "active",
        peer: "alice@waddle.test/desktop",
        sid: "c1",
        media: audioVideo,
        join,
        kind: "dm",
      },
      { kind: "session-terminate", from: "alice@waddle.test/desktop", sid: "c1" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c1", reason: null });
  });

  test("finish ends the call without a reason", () => {
    const next = reduceCallState(
      {
        phase: "incoming",
        from: "alice@waddle.test/desktop",
        sid: "c1",
        media: audioVideo,
      },
      { kind: "finish", from: "alice@waddle.test/desktop", sid: "c1" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c1", reason: null });
  });

  test("reject from idle is a no-op", () => {
    const next = reduceCallState({ phase: "idle" }, {
      kind: "reject",
      from: "alice@waddle.test/desktop",
      sid: "c1",
    });
    expect(next).toEqual({ phase: "idle" });
  });

  test("retract from incoming ends the call", () => {
    const next = reduceCallState(
      {
        phase: "incoming",
        from: "alice@waddle.test/desktop",
        sid: "c1",
        media: audioVideo,
      },
      { kind: "retract", from: "alice@waddle.test/desktop", sid: "c1" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c1", reason: "retract" });
  });

  test("applyCallEvent writes to the global store", () => {
    clearCallState();
    expect($callState.get()).toEqual({ phase: "idle" });
    applyCallEvent({
      kind: "propose",
      from: "alice@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
    });
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "alice@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
    });
    clearCallState();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("beginOutgoingCall transitions to the outgoing phase", () => {
    clearCallState();
    clearDmCallActivities();
    beginOutgoingCall("bob@waddle.test", "c9", audioVideo);
    expect($callState.get()).toEqual({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c9",
      media: audioVideo,
    });
    expect(readDmCallActivity("bob@waddle.test")).toMatchObject({
      peerJid: "bob@waddle.test",
      sid: "c9",
      state: "ringing",
      direction: "outgoing",
    });
    clearCallState();
    clearDmCallActivities();
  });

  test("proceed leaves outgoing intact (side-effect emits session-initiate)", () => {
    const current = {
      phase: "outgoing" as const,
      to: "bob@waddle.test",
      sid: "c9",
      media: audioVideo,
    };
    const next = reduceCallState(current, {
      kind: "proceed",
      from: "bob@waddle.test/desktop",
      sid: "c9",
    });
    expect(next).toBe(current);
  });

  test("proceed clears sibling responder ringing for the same sid", () => {
    const next = reduceCallState(
      {
        phase: "incoming",
        from: "alice@waddle.test/laptop",
        sid: "c9",
        media: audioVideo,
      },
      {
        kind: "proceed",
        from: "bob@waddle.test/phone",
        sid: "c9",
      },
    );

    expect(next).toEqual({ phase: "idle" });
  });

  test("session-accept transitions outgoing to active", () => {
    const next = reduceCallState(
      {
        phase: "outgoing",
        to: "bob@waddle.test",
        sid: "c9",
        media: audioVideo,
      },
      {
        kind: "session-accept",
        from: "bob@waddle.test/desktop",
        sid: "c9",
        media: audioVideo,
        join,
      },
    );
    expect(next).toEqual({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
      join,
      kind: "dm",
    });
  });

  test("reject from outgoing ends the call with reject reason", () => {
    const next = reduceCallState(
      {
        phase: "outgoing",
        to: "bob@waddle.test",
        sid: "c9",
        media: audioVideo,
      },
      { kind: "reject", from: "bob@waddle.test/desktop", sid: "c9" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c9", reason: "reject" });
  });

  test("retract from outgoing ends the call with retract reason", () => {
    const next = reduceCallState(
      {
        phase: "outgoing",
        to: "bob@waddle.test",
        sid: "c9",
        media: audioVideo,
      },
      { kind: "retract", from: "alice@waddle.test/desktop", sid: "c9" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c9", reason: "retract" });
  });

  // -- sid + phase guards --------------------------------------------

  test("session-initiate is dropped when phase is not incoming", () => {
    const current = { phase: "idle" as const };
    const next = reduceCallState(current, {
      kind: "session-initiate",
      from: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
      join,
    });
    expect(next).toBe(current);
  });

  test("session-initiate is dropped when sid does not match incoming", () => {
    const current = {
      phase: "incoming" as const,
      from: "alice@waddle.test/desktop",
      sid: "c-live",
      media: audioVideo,
    };
    const next = reduceCallState(current, {
      kind: "session-initiate",
      from: "alice@waddle.test/desktop",
      sid: "c-stale",
      media: audioVideo,
      join,
    });
    expect(next).toBe(current);
  });

  test("session-accept is dropped when phase is not outgoing", () => {
    const current = { phase: "idle" as const };
    const next = reduceCallState(current, {
      kind: "session-accept",
      from: "bob@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
      join,
    });
    expect(next).toBe(current);
  });

  test("session-accept is dropped when sid does not match outgoing", () => {
    const current = {
      phase: "outgoing" as const,
      to: "bob@waddle.test",
      sid: "c-live",
      media: audioVideo,
    };
    const next = reduceCallState(current, {
      kind: "session-accept",
      from: "bob@waddle.test/desktop",
      sid: "c-stale",
      media: audioVideo,
      join,
    });
    expect(next).toBe(current);
  });

  test("session-terminate with mismatched sid is a no-op", () => {
    const current = {
      phase: "active" as const,
      peer: "bob@waddle.test/desktop",
      sid: "c-live",
      media: audioVideo,
      join,
      kind: "dm",
    };
    const next = reduceCallState(current, {
      kind: "session-terminate",
      from: "bob@waddle.test/desktop",
      sid: "c-stale",
      reason: "success",
    });
    expect(next).toBe(current);
  });

  test("finish with mismatched sid is a no-op", () => {
    const current = {
      phase: "active" as const,
      peer: "bob@waddle.test/desktop",
      sid: "c-live",
      media: audioVideo,
      join,
      kind: "dm",
    };
    const next = reduceCallState(current, {
      kind: "finish",
      from: "bob@waddle.test/desktop",
      sid: "c-stale",
    });
    expect(next).toBe(current);
  });

  test("reject with mismatched sid is a no-op", () => {
    const current = {
      phase: "outgoing" as const,
      to: "bob@waddle.test",
      sid: "c-live",
      media: audioVideo,
    };
    const next = reduceCallState(current, {
      kind: "reject",
      from: "bob@waddle.test/desktop",
      sid: "c-stale",
    });
    expect(next).toBe(current);
  });

  test("propose while active is retained as current state", () => {
    const current = {
      phase: "active" as const,
      peer: "carol@waddle.test/desktop",
      sid: "c-live",
      media: audioVideo,
      join,
      kind: "dm",
    };
    const next = reduceCallState(current, {
      kind: "propose",
      from: "dave@waddle.test/phone",
      sid: "c-new",
      media: audioVideo,
    });
    expect(next).toBe(current);
  });
});

describe("call lifecycle terminal mapping", () => {
  const incoming: CallState = {
    phase: "incoming",
    from: "alice@waddle.test/desktop",
    sid: "c1",
    media: audioVideo,
  };
  const active: CallState = {
    phase: "active",
    peer: "alice@waddle.test/desktop",
    sid: "c1",
    media: audioVideo,
    join,
    kind: "dm",
  };
  const terminate = (reason: string): CallEvent => ({
    kind: "session-terminate",
    from: "alice@waddle.test/desktop",
    sid: "c1",
    reason,
  });

  test.each([
    ["failed-transport", "failed"],
    ["incompatible-parameters", "failed"],
    ["timeout", "timeout"],
    ["decline", "declined"],
    ["success", "proposed"],
  ])("maps pre-active %s to setup outcome %s", (reason, setupOutcome) => {
    expect(callLifecycleTerminalForRemoteEnd(incoming, terminate(reason), reason))
      .toEqual({ setupOutcome });
  });

  test.each([
    ["failed-transport", "error"],
    ["timeout", "error"],
    ["success", "peer-left"],
  ])("maps active %s to end reason %s", (reason, endReason) => {
    expect(callLifecycleTerminalForRemoteEnd(active, terminate(reason), reason))
      .toEqual({ endReason });
  });
});

describe("clearLastCallError", () => {
  test("clears the error without touching $callState", () => {
    // Pre-transition rejection scenario: a `beginMucCall` failure
    // sets the error while phase stays idle. `clearCallState` is
    // overkill here (it would emit a redundant state set);
    // `clearLastCallError` is the targeted helper.
    clearCallState();
    const errors: Array<{ type?: string; context?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushError: (_error: Error, options?: { type?: string; context?: Record<string, string> }) => {
          errors.push(options ?? {});
        },
      },
    } as never);
    reportCallError(new Error("muc-call: transport missing @url"));
    expect($lastCallError.get()).toBe("muc-call: transport missing @url");
    const phaseBefore = $callState.get().phase;
    clearLastCallError();
    expect($lastCallError.get()).toBeNull();
    expect($callState.get().phase).toBe(phaseBefore);
    expect(errors[0]).toEqual({
      type: "call.operation",
      context: expect.objectContaining({ recoverable: "true", detail: "call-operation" }),
    });
    __setFaroForTesting(null);
  });
});
