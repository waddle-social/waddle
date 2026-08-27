import { afterEach, describe, expect, test } from "bun:test";
import {
  $callState,
  $lastCallError,
  applyCallEvent,
  beginOutgoingCall,
  callLifecycleTerminalForSignaledEnd,
  callLifecycleTerminalForTeardown,
  clearCallState,
  clearLastCallError,
  reduceCallState,
  reportCallError,
  teardownSetupOutcome,
} from "../src/lib/calls/call-store";
import {
  clearDmCallActivities,
  readDmCallActivity,
} from "../src/lib/calls/dm-call-activity";
import type { CallEvent, CallMedia, CallState, LiveKitJoin } from "../src/lib/calls/types";
import {
  __resetCallLifecycleTelemetryForTesting,
  finishCallAttempt,
  markCallAttemptAccepted,
} from "../src/lib/calls/call-lifecycle-telemetry";
import { __setFaroForTesting } from "../src/lib/telemetry";
import { useCallEngine } from "../src/lib/calls/use-call-engine";
import { DisconnectReason, type Room } from "livekit-client";
import {
  $mucCallLeavingRooms,
  $mucCallLiveParticipants,
  clearAllLiveCallParticipants,
  setLiveCallParticipants,
} from "../src/lib/calls/muc-call-live-participants";

const audioVideo: CallMedia = { audio: true, video: true };
const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: "alice@waddle.test::c1",
  identity: "bob@waddle.test/desktop",
  token: "eyJhbGc.payload.sig",
};

afterEach(() => {
  clearCallState();
  clearDmCallActivities();
  clearAllLiveCallParticipants();
  __resetCallLifecycleTelemetryForTesting();
  __setFaroForTesting(null);
});

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

  test("sibling proceed finalizes the pending browser attempt exactly once", async () => {
    clearCallState();
    __resetCallLifecycleTelemetryForTesting();
    const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushEvent: (name: string, attributes?: Record<string, string>) => {
          events.push({ name, attributes });
        },
      },
    } as never);

    applyCallEvent({
      kind: "propose",
      from: "alice@waddle.test/desktop",
      sid: "sibling-accepted",
      media: audioVideo,
    });
    applyCallEvent({
      kind: "proceed",
      from: "alice@waddle.test/phone",
      sid: "sibling-accepted",
    });
    clearCallState();

    expect($callState.get()).toEqual({ phase: "idle" });
    expect(events).toEqual([{
      name: "chat.call.lifecycle",
      attributes: expect.objectContaining({
        setup_outcome: "proposed",
        end_reason: "hangup",
        call_kind: "dm",
      }),
    }]);

  });

  test("local accept proceed does not finalize the attempt (session-initiate follows)", async () => {
    clearCallState();
    __resetCallLifecycleTelemetryForTesting();
    const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushEvent: (name: string, attributes?: Record<string, string>) => {
          events.push({ name, attributes });
        },
      },
    } as never);

    applyCallEvent({
      kind: "propose",
      from: "alice@waddle.test/desktop",
      sid: "local-accept",
      media: audioVideo,
    });
    // The echo of THIS resource's own <proceed/> (local accept): the
    // attempt must survive so session-initiate can mark it accepted.
    applyCallEvent(
      {
        kind: "proceed",
        from: "bob@waddle.test/web",
        sid: "local-accept",
      },
      { selfOriginated: true, selfFullJid: "bob@waddle.test/web" },
    );
    expect(events).toEqual([]);

    // The attempt is still alive: a later terminal event emits its
    // lifecycle beacon (accepted via the media path in the real flow).
    markCallAttemptAccepted("local-accept");
    finishCallAttempt("local-accept", { endReason: "hangup" });
    expect(events).toEqual([{
      name: "chat.call.lifecycle",
      attributes: expect.objectContaining({
        setup_outcome: "accepted",
        end_reason: "hangup",
        call_kind: "dm",
      }),
    }]);
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
    ["failed-transport", "failed", "error"],
    ["incompatible-parameters", "failed", "error"],
    ["timeout", "timeout", "error"],
    ["decline", "declined", "peer-left"],
    ["success", "proposed", "peer-left"],
  ])("maps pre-active %s to setup outcome %s and end reason %s", (
    reason,
    setupOutcome,
    endReason,
  ) => {
    expect(callLifecycleTerminalForSignaledEnd(incoming, terminate(reason), reason))
      .toEqual({ setupOutcome, endReason });
  });

  test.each([
    ["active", "accepted"],
    ["incoming", "declined"],
    // A torn-down pending group-call setup is a failed attempt, not a
    // proposed one — greptile P1 on PR #1415.
    ["muc-pending", "failed"],
    ["outgoing", "proposed"],
  ] as const)("teardown from %s reports setup outcome %s", (phase, setupOutcome) => {
    expect(teardownSetupOutcome(phase)).toBe(setupOutcome);
  });

  test.each([
    ["failed-transport", "error"],
    ["timeout", "error"],
    ["success", "peer-left"],
  ])("maps active %s to end reason %s", (reason, endReason) => {
    expect(callLifecycleTerminalForSignaledEnd(active, terminate(reason), reason))
      .toEqual({ endReason });
  });

  test("maps a proposed-then-retracted attempt to a peer departure", () => {
    expect(callLifecycleTerminalForSignaledEnd(incoming, {
      kind: "retract",
      from: "alice@waddle.test/desktop",
      sid: "c1",
    }, "retract")).toEqual({ setupOutcome: "proposed", endReason: "peer-left" });
  });

  test.each([
    ["incoming", "clear", "declined", "hangup"],
    ["outgoing", "success", "proposed", "hangup"],
    ["muc-pending", "clear", "failed", "error"],
    ["active", "gone", "accepted", "error"],
  ] as const)(
    "maps non-active/local %s %s teardown to %s and %s",
    (phase, disposition, setupOutcome, endReason) => {
      expect(callLifecycleTerminalForTeardown(phase, disposition))
        .toEqual({ setupOutcome, endReason });
    },
  );
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
      context: { kind: "call.operation", reason: "call-operation" },
    });
    __setFaroForTesting(null);
  });
});

describe("terminal LiveKit disconnect", () => {
  test("clears an active call and reports the transport reason", async () => {
    const engine = useCallEngine().engine;
    const room = {
      off() {
        return room;
      },
      localParticipant: {
        getTrackPublication() {
          return undefined;
        },
      },
    };
    (engine as unknown as { room: Room | null }).room = room as unknown as Room;
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "transport-lost",
      media: audioVideo,
      join,
      kind: "dm",
    });

    await (
      engine as unknown as {
        handleDisconnected: (reason?: DisconnectReason) => Promise<void>;
      }
    ).handleDisconnected(DisconnectReason.DUPLICATE_IDENTITY);

    expect($callState.get()).toEqual({ phase: "idle" });
    expect($lastCallError.get()).toBe(
      "This call ended because the same account joined from another device.",
    );
  });

  test("preserves MUC leave cleanup after the call store becomes idle", async () => {
    const engine = useCallEngine().engine;
    const room = {
      off() {
        return room;
      },
      localParticipant: {
        getTrackPublication() {
          return undefined;
        },
      },
    };
    (engine as unknown as { room: Room | null }).room = room as unknown as Room;
    setLiveCallParticipants("room@conference.waddle.test", ["alice@waddle.test/web"]);
    $callState.set({
      phase: "active",
      peer: "room@conference.waddle.test",
      sid: "muc-transport-lost",
      media: audioVideo,
      join,
      kind: "muc",
      selfNick: "alice",
    });

    await (
      engine as unknown as {
        handleDisconnected: (reason?: DisconnectReason) => Promise<void>;
      }
    ).handleDisconnected(DisconnectReason.ROOM_DELETED);

    expect($mucCallLiveParticipants.get()).toEqual({});
    expect($mucCallLeavingRooms.get()).toEqual({
      "room@conference.waddle.test": "alice",
    });
  });
});
