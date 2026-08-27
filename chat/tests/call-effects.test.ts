import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  $callState,
  $lastCallError,
  clearCallState,
} from "../src/lib/calls/call-store";
import { handleCallEventSideEffect } from "../src/lib/calls/call-effects";
import type { CallWireSender } from "../src/lib/calls/outbound";
import type { CallEvent, CallMedia, CallState, LiveKitJoin } from "../src/lib/calls/types";
import { __resetCallLifecycleTelemetryForTesting } from "../src/lib/calls/call-lifecycle-telemetry";
import { __setFaroForTesting } from "../src/lib/telemetry";

const audioVideo: CallMedia = { audio: true, video: true };
const audioOnly: CallMedia = { audio: true, video: false };
const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: "alice@waddle.test::c9",
  identity: "bob@waddle.test/desktop",
  token: "eyJhbGc.payload.sig",
};

function mockSender(): CallWireSender {
  return {
    send_call_propose: mock(async () => undefined),
    send_call_proceed: mock(async () => undefined),
    send_call_reject: mock(async () => undefined),
    send_call_reject_tie_break: mock(async () => undefined),
    send_call_retract: mock(async () => undefined),
    send_call_retract_tie_break: mock(async () => undefined),
    send_call_finish: mock(async () => undefined),
    send_call_finish_migrated: mock(async () => undefined),
    send_call_session_initiate: mock(async () => undefined),
    send_call_session_accept: mock(async () => undefined),
    send_call_session_terminate: mock(async () => undefined),
  };
}

afterEach(() => {
  clearCallState();
  __resetCallLifecycleTelemetryForTesting();
  __setFaroForTesting(null);
});

describe("handleCallEventSideEffect", () => {
  test("proceed side-effect failure ends outgoing UI and preserves a visible error", async () => {
    clearCallState();
    const sender = mockSender();
    sender.send_call_session_initiate = mock(async () => {
      throw new Error("session-initiate failed");
    });
    const prev: CallState = {
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c9",
      media: audioVideo,
    };
    $callState.set(prev);
    await handleCallEventSideEffect({
      kind: "proceed",
      from: "bob@waddle.test/desktop",
      sid: "c9",
    }, prev, sender, "alice@waddle.test/web-1");
    expect($callState.get()).toEqual({
      phase: "ended",
      sid: "c9",
      reason: "error",
    });
    expect($lastCallError.get()).toBe("session-initiate failed");
    clearCallState();
  });

  test("session-initiate side-effect failure ends accepted incoming UI and preserves a visible error", async () => {
    clearCallState();
    const sender = mockSender();
    sender.send_call_session_accept = mock(async () => {
      throw new Error("session-accept failed");
    });
    const prev: CallState = {
      phase: "incoming",
      from: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
    };
    $callState.set({
      phase: "active",
      peer: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
      join,
      kind: "dm",
    });
    await handleCallEventSideEffect({
      kind: "session-initiate",
      from: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
      join,
    }, prev, sender, "bob@waddle.test/web-1");
    expect($callState.get()).toEqual({
      phase: "ended",
      sid: "c9",
      reason: "error",
    });
    expect($lastCallError.get()).toBe("session-accept failed");
    clearCallState();
  });

  test("fires session-initiate when proceed lands on a matching outgoing call", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c9",
      media: audioVideo,
    };
    const event: CallEvent = {
      kind: "proceed",
      from: "bob@waddle.test/desktop",
      sid: "c9",
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_session_initiate).toHaveBeenCalledTimes(1);
    expect(sender.send_call_session_initiate).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "alice@waddle.test/web-1",
      "c9",
      true,
      true,
    );
  });

  test("ignores proceed when there is no outgoing call", async () => {
    const sender = mockSender();
    const prev: CallState = { phase: "idle" };
    const event: CallEvent = {
      kind: "proceed",
      from: "bob@waddle.test/desktop",
      sid: "c9",
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_session_initiate).not.toHaveBeenCalled();
  });

  test("ignores proceed when the sid does not match the outgoing call", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c9",
      media: audioVideo,
    };
    const event: CallEvent = {
      kind: "proceed",
      from: "bob@waddle.test/desktop",
      sid: "different-sid",
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_session_initiate).not.toHaveBeenCalled();
  });

  test("does not fire on non-proceed events when outgoing", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c9",
      media: audioVideo,
    };
    const event: CallEvent = {
      kind: "reject",
      from: "bob@waddle.test/desktop",
      sid: "c9",
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_session_initiate).not.toHaveBeenCalled();
  });

  test("fires session-accept when session-initiate lands on a matching incoming call", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "incoming",
      from: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
    };
    const event: CallEvent = {
      kind: "session-initiate",
      from: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
      join,
    };
    await handleCallEventSideEffect(event, prev, sender, "bob@waddle.test/web-1");
    expect(sender.send_call_session_accept).toHaveBeenCalledTimes(1);
    expect(sender.send_call_session_accept).toHaveBeenCalledWith(
      "alice@waddle.test/desktop", // peer_full_jid
      "bob@waddle.test/web-1",     // responder_full_jid
      "c9",
      true,
      true,
    );
  });

  test("session-accept side-effect carries the event's media flags", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "incoming",
      from: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioOnly,
    };
    const event: CallEvent = {
      kind: "session-initiate",
      from: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioOnly,
      join,
    };
    await handleCallEventSideEffect(event, prev, sender, "bob@waddle.test/web-1");
    expect(sender.send_call_session_accept).toHaveBeenCalledWith(
      "alice@waddle.test/desktop",
      "bob@waddle.test/web-1",
      "c9",
      true,
      false,
    );
  });

  test("ignores session-initiate when not in incoming phase", async () => {
    const sender = mockSender();
    const prev: CallState = { phase: "idle" };
    const event: CallEvent = {
      kind: "session-initiate",
      from: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
      join,
    };
    await handleCallEventSideEffect(event, prev, sender, "bob@waddle.test/web-1");
    expect(sender.send_call_session_accept).not.toHaveBeenCalled();
  });

  test("ignores session-initiate when sid does not match incoming call", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "incoming",
      from: "alice@waddle.test/desktop",
      sid: "c9",
      media: audioVideo,
    };
    const event: CallEvent = {
      kind: "session-initiate",
      from: "alice@waddle.test/desktop",
      sid: "stale-sid",
      media: audioVideo,
      join,
    };
    await handleCallEventSideEffect(event, prev, sender, "bob@waddle.test/web-1");
    expect(sender.send_call_session_accept).not.toHaveBeenCalled();
  });

  test("busy-reject: propose while active fires reject to event.from", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "active",
      peer: "carol@waddle.test/desktop",
      sid: "c-existing",
      media: audioVideo,
      join,
      kind: "dm",
    };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test/phone",
      sid: "c-new",
      media: audioVideo,
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_reject).toHaveBeenCalledTimes(1);
    expect(sender.send_call_reject).toHaveBeenCalledWith("dave@waddle.test/phone", "c-new");
  });

  test("existing-session tie-break: same-peer active propose finishes old sid and proceeds new sid", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "active",
      peer: "dave@waddle.test/desktop",
      sid: "old-session",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "dave@waddle.test/desktop",
    };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test/tablet",
      sid: "new-session",
      media: audioVideo,
    };
    $callState.set(prev);
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_finish_migrated).toHaveBeenCalledWith(
      "dave@waddle.test/tablet",
      "old-session",
      "new-session",
    );
    expect(sender.send_call_proceed).toHaveBeenCalledWith(
      "dave@waddle.test/tablet",
      "new-session",
    );
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "dave@waddle.test/tablet",
      sid: "new-session",
      media: audioVideo,
    });
    clearCallState();
  });

  test("tie-break: lower inbound propose retracts our outgoing session and becomes incoming", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "outgoing",
      to: "dave@waddle.test",
      sid: "z-outgoing",
      media: audioVideo,
    };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test/phone",
      sid: "a-incoming",
      media: audioVideo,
    };
    $callState.set(prev);
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_retract_tie_break).toHaveBeenCalledTimes(1);
    expect(sender.send_call_retract_tie_break).toHaveBeenCalledWith(
      "dave@waddle.test/phone",
      "z-outgoing",
    );
    expect(sender.send_call_reject_tie_break).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "dave@waddle.test/phone",
      sid: "a-incoming",
      media: audioVideo,
    });
    clearCallState();
  });

  test("tie-break retract failure reports the inbound proposal as one failed attempt", async () => {
    const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushEvent: (name: string, attributes?: Record<string, string>) => {
          events.push({ name, attributes });
        },
        pushError: () => undefined,
      },
    } as never);
    const sender = mockSender();
    sender.send_call_retract_tie_break = mock(async () => {
      throw new Error("tie-break retract failed");
    });
    const prev: CallState = {
      phase: "outgoing",
      to: "dave@waddle.test",
      sid: "z-outgoing",
      media: audioVideo,
    };
    $callState.set(prev);

    await handleCallEventSideEffect({
      kind: "propose",
      from: "dave@waddle.test/phone",
      sid: "a-incoming",
      media: audioVideo,
    }, prev, sender, "alice@waddle.test/web-1");

    expect(events).toEqual([{
      name: "chat.call.lifecycle",
      attributes: expect.objectContaining({
        setup_outcome: "failed",
        end_reason: "error",
        call_kind: "dm",
      }),
    }]);
  });

  test("active-call migration failure reports the inbound proposal as one failed attempt", async () => {
    const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushEvent: (name: string, attributes?: Record<string, string>) => {
          events.push({ name, attributes });
        },
        pushError: () => undefined,
      },
    } as never);
    const sender = mockSender();
    sender.send_call_finish_migrated = mock(async () => {
      throw new Error("migration finish failed");
    });
    const prev: CallState = {
      phase: "active",
      peer: "dave@waddle.test/desktop",
      sid: "old-session",
      media: audioVideo,
      join,
      kind: "dm",
    };
    $callState.set(prev);

    await handleCallEventSideEffect({
      kind: "propose",
      from: "dave@waddle.test/tablet",
      sid: "new-session",
      media: audioVideo,
    }, prev, sender, "alice@waddle.test/web-1");

    expect(events).toEqual([{
      name: "chat.call.lifecycle",
      attributes: expect.objectContaining({
        setup_outcome: "failed",
        end_reason: "error",
        call_kind: "dm",
      }),
    }]);
  });

  test("tie-break: higher inbound propose is rejected and outgoing state stays intact", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "outgoing",
      to: "dave@waddle.test",
      sid: "a-outgoing",
      media: audioVideo,
    };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test/phone",
      sid: "z-incoming",
      media: audioVideo,
    };
    $callState.set(prev);
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_reject_tie_break).toHaveBeenCalledTimes(1);
    expect(sender.send_call_reject_tie_break).toHaveBeenCalledWith(
      "dave@waddle.test/phone",
      "z-incoming",
    );
    expect(sender.send_call_retract_tie_break).not.toHaveBeenCalled();
    expect($callState.get()).toBe(prev);
    clearCallState();
  });

  test("busy-reject: propose while outgoing against a different bare JID fires plain reject", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "outgoing",
      to: "carol@waddle.test",
      sid: "c-existing",
      media: audioVideo,
    };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test/phone",
      sid: "c-new",
      media: audioVideo,
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_reject).toHaveBeenCalledTimes(1);
    expect(sender.send_call_reject_tie_break).not.toHaveBeenCalled();
  });

  test("busy-reject: propose while idle does NOT fire reject", async () => {
    const sender = mockSender();
    const prev: CallState = { phase: "idle" };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test/phone",
      sid: "c-new",
      media: audioVideo,
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_reject).not.toHaveBeenCalled();
  });

  test("busy-reject: propose while ended does NOT fire reject", async () => {
    const sender = mockSender();
    const prev: CallState = { phase: "ended", sid: "c-old", reason: "success" };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test/phone",
      sid: "c-new",
      media: audioVideo,
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_reject).not.toHaveBeenCalled();
  });
});
