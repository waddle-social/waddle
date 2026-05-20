import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  $callState,
  applyCallEvent,
  beginOutgoingCall,
  clearCallState,
  scheduleOutgoingTimeout,
  tearDownActiveCall,
} from "../src/lib/calls/call-store";
import { applyMucCallPresence } from "../src/lib/calls/muc-call-presence";
import type { CallWireSender } from "../src/lib/calls/outbound";
import type { CallMedia, LiveKitJoin } from "../src/lib/calls/types";

/**
 * Mock the wasm `update_muji_presence` such that the preparing
 * branch simulates the MUC echoing the preparing presence back
 * — the XEP-0272 §Joining echo that `awaitPreparingEcho` blocks
 * on inside `beginMucCall`. Without this the tests would all
 * pay the 2s echo-timeout penalty on every call to `beginMucCall`
 * that passes a `selfNick`.
 */
function mockUpdateMujiPresenceWithEcho() {
  return mock(
    async (
      roomJid: string,
      nick: string,
      active: boolean,
      preparing: boolean,
      _video: boolean,
    ) => {
      if (preparing) {
        applyMucCallPresence({
          from: `${roomJid}/${nick}`,
          presence_type: "available",
          muji: { preparing: true, active: false },
        });
      }
      if (active) {
        applyMucCallPresence({
          from: `${roomJid}/${nick}`,
          presence_type: "available",
          muji: { preparing: false, active: true },
        });
      }
    },
  );
}

/**
 * Mock the wasm `send_muji_session_initiate` for the XEP-0166
 * §6.3 separate-IQ accept flow. After resolving the empty IQ-result
 * ack, fires `applyCallEvent` with the typed `session-accept` event
 * the chat-side's `tryFulfillMujiAccept` is waiting on. The real
 * wasm bridge would parse this from an inbound `<iq type='set'>`
 * stanza routed through `on_call`; tests skip the wire dance.
 */
function mockSendMujiSessionInitiateWithAccept(join: LiveKitJoin) {
  return mock(async (roomJid: string, _video: boolean) => {
    // Empty IQ-result ack is the function's resolution; fire the
    // inbound session-accept on the next microtask so the await
    // chain in beginMucCall sees the resolver populated before the
    // event lands.
    queueMicrotask(() => {
      applyCallEvent({
        kind: "session-accept",
        from: "calls.waddle.test",
        sid: roomJid,
        media: { audio: true, video: true },
        join,
      });
    });
  });
}

const audioVideo: CallMedia = { audio: true, video: true };
const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: "alice@waddle.test::c1",
  identity: "bob@waddle.test/desktop",
  token: "eyJhbGc.payload.sig",
};

function mockSender(): CallWireSender {
  return {
    send_call_propose: mock(async () => undefined),
    send_call_proceed: mock(async () => undefined),
    send_call_reject: mock(async () => undefined),
    send_call_retract: mock(async () => undefined),
    send_call_finish: mock(async () => undefined),
    send_call_session_initiate: mock(async () => undefined),
    send_call_session_accept: mock(async () => undefined),
    send_call_session_terminate: mock(async () => undefined),
  };
}

afterEach(() => {
  clearCallState();
});

describe("tearDownActiveCall", () => {
  test("active call: dispatches sessionTerminate with the given reason", async () => {
    const sender = mockSender();
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
    });
    await tearDownActiveCall(sender, "gone");
    expect(sender.send_call_session_terminate).toHaveBeenCalledTimes(1);
    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
      "gone",
    );
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("active DM call: also dispatches XEP-0353 finish after sessionTerminate", async () => {
    // XEP-0353 §3.5: after either side terminates a call, both SHOULD
    // emit `<finish/>` so the MAM bookend pairing stays consistent.
    // Without this the responder's archive only carries the `propose`
    // / `proceed` half of the JMI handshake and the initiator's UI
    // can't render a clean "call ended" row.
    const sender = mockSender();
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
    });
    await tearDownActiveCall(sender, "success");
    expect(sender.send_call_session_terminate).toHaveBeenCalledTimes(1);
    expect(sender.send_call_finish).toHaveBeenCalledTimes(1);
    expect(sender.send_call_finish).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
    );
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("active DM call: finish failure does not block clearing the slot", async () => {
    // A stale wasm bundle without `send_call_finish` (or a transient
    // I/O error inside the finish stanza) MUST NOT keep us stuck on
    // `phase: active` after the terminate already went out — the
    // local UI would still render the call overlay while the peer
    // has already ended.
    const sender: CallWireSender = {
      send_call_session_terminate: mock(async () => undefined),
      // No send_call_finish → outboundCalls.finish throws.
    };
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
    });
    await tearDownActiveCall(sender, "success");
    expect(sender.send_call_session_terminate).toHaveBeenCalledTimes(1);
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("active call: success reason for graceful logout", async () => {
    const sender = mockSender();
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
    });
    await tearDownActiveCall(sender, "success");
    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
      "success",
    );
  });

  test("outgoing call: dispatches retract", async () => {
    const sender = mockSender();
    beginOutgoingCall("bob@waddle.test", "c2", audioVideo);
    await tearDownActiveCall(sender, "gone");
    expect(sender.send_call_retract).toHaveBeenCalledTimes(1);
    expect(sender.send_call_retract).toHaveBeenCalledWith("bob@waddle.test", "c2");
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("incoming call: dispatches reject", async () => {
    const sender = mockSender();
    $callState.set({
      phase: "incoming",
      from: "alice@waddle.test",
      sid: "c3",
      media: audioVideo,
    });
    await tearDownActiveCall(sender, "gone");
    expect(sender.send_call_reject).toHaveBeenCalledTimes(1);
    expect(sender.send_call_reject).toHaveBeenCalledWith("alice@waddle.test", "c3");
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("idle: no-op", async () => {
    const sender = mockSender();
    await tearDownActiveCall(sender, "gone");
    expect(sender.send_call_session_terminate).not.toHaveBeenCalled();
    expect(sender.send_call_retract).not.toHaveBeenCalled();
    expect(sender.send_call_reject).not.toHaveBeenCalled();
  });

  test("null sender: still clears state without throwing", async () => {
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
    });
    await tearDownActiveCall(null, "gone");
    expect($callState.get()).toEqual({ phase: "idle" });
  });
});

describe("MUC group call", () => {
  test("beginMucCall sets active(kind: 'muc') after awaiting the separate session-accept (XEP-0166 §6.3)", async () => {
    const expectedJoin: LiveKitJoin = {
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "jwt.payload.sig",
    };
    const sender = {
      send_muji_session_initiate:
        mockSendMujiSessionInitiateWithAccept(expectedJoin),
    };
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    await beginMucCall(sender, "chan@muc.test", audioVideo);
    const state = $callState.get();
    expect(state).toEqual({
      phase: "active",
      peer: "chan@muc.test",
      sid: "chan@muc.test",
      media: audioVideo,
      join: expectedJoin,
      kind: "muc",
      selfNick: undefined,
    });
    expect(sender.send_muji_session_initiate).toHaveBeenCalledWith(
      "chan@muc.test",
      true, // audioVideo.video
    );
  });

  test("beginMucCall with a nick implements the XEP-0272 §Joining two-phase preparing→content flow", async () => {
    const send_muji_session_initiate = mockSendMujiSessionInitiateWithAccept({
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "jwt.payload.sig",
    });
    const update_muji_presence = mockUpdateMujiPresenceWithEcho();
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    await beginMucCall(
      {
        send_muji_session_initiate,
        update_muji_presence,
      } as unknown as Parameters<typeof beginMucCall>[0],
      "chan@muc.test",
      audioVideo,
      "alice",
    );
    // Two-phase flow:
    //   1. preparing  → `<muji><preparing/></muji>`
    //   2. (session-initiate IQ round-trip — implicit wait for MUC echo)
    //   3. active content → `<muji><content .../></muji>`
    expect(update_muji_presence).toHaveBeenCalledTimes(2);
    expect(update_muji_presence).toHaveBeenNthCalledWith(
      1,
      "chan@muc.test",
      "alice",
      false, // active
      true, // preparing
      false, // video — irrelevant in preparing phase
    );
    // The session-initiate must run BETWEEN the preparing and the
    // content presences. mock() records call order globally
    // across the mock pair, so we assert relative ordering.
    expect(send_muji_session_initiate).toHaveBeenCalledTimes(1);
    expect(update_muji_presence).toHaveBeenNthCalledWith(
      2,
      "chan@muc.test",
      "alice",
      true, // active
      false, // preparing
      true, // video (audioVideo fixture has video=true)
    );
  });

  test("tearDownActiveCall still clears the Muji presence when sendMujiSessionTerminate throws", async () => {
    // Regression guard: a stale wasm bundle without
    // `send_muji_session_terminate` makes the call throw. The
    // presence cleanup MUST still run — otherwise the user's
    // `<muji/>` advertisement lingers until the XMPP session
    // disconnects, exactly the bug this teardown path exists to fix.
    const update_muji_presence = mockUpdateMujiPresenceWithEcho();
    $callState.set({
      phase: "active",
      peer: "chan@muc.test",
      sid: "chan@muc.test",
      media: audioVideo,
      join,
      kind: "muc",
      selfNick: "alice",
    });
    await tearDownActiveCall(
      { update_muji_presence } as unknown as Parameters<
        typeof tearDownActiveCall
      >[0],
      "success",
    );
    expect(update_muji_presence).toHaveBeenCalledTimes(1);
    expect(update_muji_presence).toHaveBeenCalledWith(
      "chan@muc.test",
      "alice",
      false, // active
      false, // preparing
      false, // video
    );
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("tearDownActiveCall on a MUC call dispatches Muji session-terminate AND clears the presence", async () => {
    const send_muji_session_terminate = mock(async () => undefined);
    const update_muji_presence = mockUpdateMujiPresenceWithEcho();
    $callState.set({
      phase: "active",
      peer: "chan@muc.test",
      sid: "chan@muc.test",
      media: audioVideo,
      join,
      kind: "muc",
      selfNick: "alice",
    });
    await tearDownActiveCall(
      {
        send_muji_session_terminate,
        update_muji_presence,
      } as unknown as Parameters<typeof tearDownActiveCall>[0],
      "gone",
    );
    expect(send_muji_session_terminate).toHaveBeenCalledTimes(1);
    expect(send_muji_session_terminate).toHaveBeenCalledWith("chan@muc.test");
    expect(update_muji_presence).toHaveBeenCalledTimes(1);
    expect(update_muji_presence).toHaveBeenCalledWith(
      "chan@muc.test",
      "alice",
      false, // active
      false, // preparing
      false, // video
    );
    expect($callState.get()).toEqual({ phase: "idle" });
  });
});

describe("scheduleOutgoingTimeout", () => {
  test("fires retract and ends the call when the timer elapses on outgoing", async () => {
    const sender = mockSender();
    beginOutgoingCall("bob@waddle.test", "c-timeout", audioVideo);
    scheduleOutgoingTimeout(sender, "c-timeout", 10);
    // Wait past the timer.
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(sender.send_call_retract).toHaveBeenCalledTimes(1);
    expect(sender.send_call_retract).toHaveBeenCalledWith("bob@waddle.test", "c-timeout");
    expect($callState.get()).toEqual({
      phase: "ended",
      sid: "c-timeout",
      reason: "timeout",
    });
  });

  test("is a no-op if the call has already transitioned out of outgoing", async () => {
    const sender = mockSender();
    beginOutgoingCall("bob@waddle.test", "c-fast", audioVideo);
    scheduleOutgoingTimeout(sender, "c-fast", 10);
    // Simulate a fast peer answering — call moves to active before timer fires.
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c-fast",
      media: audioVideo,
      join,
    });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(sender.send_call_retract).not.toHaveBeenCalled();
  });

  test("is a no-op if the sid no longer matches", async () => {
    const sender = mockSender();
    beginOutgoingCall("bob@waddle.test", "c-old", audioVideo);
    scheduleOutgoingTimeout(sender, "c-old", 10);
    // Replace with a new outgoing call (different sid) — old timer
    // should not fire retract on the new call.
    beginOutgoingCall("carol@waddle.test", "c-new", audioVideo);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(sender.send_call_retract).not.toHaveBeenCalled();
  });
});
