import { describe, expect, test } from "bun:test";
import {
  $callState,
  applyCallEvent,
  beginOutgoingCall,
  clearCallState,
  reduceCallState,
} from "../src/lib/calls/call-store";
import type { CallMedia, LiveKitJoin } from "../src/lib/calls/types";

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
      from: "alice@waddle.test",
      sid: "c1",
      media: audioVideo,
    });
    expect(next).toEqual({
      phase: "incoming",
      from: "alice@waddle.test",
      sid: "c1",
      media: audioVideo,
    });
  });

  test("propose is ignored when already in an active call", () => {
    const current = {
      phase: "active" as const,
      peer: "carol@waddle.test",
      sid: "c0",
      media: audioVideo,
      join,
    };
    const next = reduceCallState(current, {
      kind: "propose",
      from: "dave@waddle.test",
      sid: "c2",
      media: audioVideo,
    });
    expect(next).toBe(current);
  });

  test("session-initiate transitions to active with credentials", () => {
    const next = reduceCallState({ phase: "idle" }, {
      kind: "session-initiate",
      from: "alice@waddle.test",
      sid: "c1",
      media: audioVideo,
      join,
    });
    expect(next).toEqual({
      phase: "active",
      peer: "alice@waddle.test",
      sid: "c1",
      media: audioVideo,
      join,
    });
  });

  test("session-terminate carries reason through", () => {
    const next = reduceCallState(
      {
        phase: "active",
        peer: "alice@waddle.test",
        sid: "c1",
        media: audioVideo,
        join,
      },
      { kind: "session-terminate", from: "alice@waddle.test", sid: "c1", reason: "success" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c1", reason: "success" });
  });

  test("finish ends the call without a reason", () => {
    const next = reduceCallState(
      {
        phase: "incoming",
        from: "alice@waddle.test",
        sid: "c1",
        media: audioVideo,
      },
      { kind: "finish", from: "alice@waddle.test", sid: "c1" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c1", reason: null });
  });

  test("reject from idle is a no-op", () => {
    const next = reduceCallState({ phase: "idle" }, {
      kind: "reject",
      from: "alice@waddle.test",
      sid: "c1",
    });
    expect(next).toEqual({ phase: "idle" });
  });

  test("retract from incoming ends the call", () => {
    const next = reduceCallState(
      {
        phase: "incoming",
        from: "alice@waddle.test",
        sid: "c1",
        media: audioVideo,
      },
      { kind: "retract", from: "alice@waddle.test", sid: "c1" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c1", reason: "retract" });
  });

  test("applyCallEvent writes to the global store", () => {
    clearCallState();
    expect($callState.get()).toEqual({ phase: "idle" });
    applyCallEvent({
      kind: "propose",
      from: "alice@waddle.test",
      sid: "c1",
      media: audioVideo,
    });
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "alice@waddle.test",
      sid: "c1",
      media: audioVideo,
    });
    clearCallState();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("beginOutgoingCall transitions to the outgoing phase", () => {
    clearCallState();
    beginOutgoingCall("bob@waddle.test", "c9", audioVideo);
    expect($callState.get()).toEqual({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c9",
      media: audioVideo,
    });
    clearCallState();
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
      { kind: "reject", from: "bob@waddle.test", sid: "c9" },
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
      { kind: "retract", from: "alice@waddle.test", sid: "c9" },
    );
    expect(next).toEqual({ phase: "ended", sid: "c9", reason: "retract" });
  });
});
