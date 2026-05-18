import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  $callState,
  beginOutgoingCall,
  clearCallState,
  scheduleOutgoingTimeout,
  tearDownActiveCall,
} from "../src/lib/calls/call-store";
import type { CallWireSender } from "../src/lib/calls/outbound";
import type { CallMedia, LiveKitJoin } from "../src/lib/calls/types";

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
  test("beginMucCall sets active(kind: 'muc') after parsing the issued transport", async () => {
    const sender = {
      send_raw_iq: mock(async () => {
        return [
          '<iq type="result" id="x">',
          '<joined xmlns="urn:waddle:muc-call:0">',
          '<transport xmlns="urn:waddle:transports:livekit:0">',
          "<url>wss://livekit.test</url>",
          "<room>chan@muc.test</room>",
          "<identity>alice@waddle.test/web</identity>",
          "<token>jwt.payload.sig</token>",
          "</transport>",
          "</joined>",
          "</iq>",
        ].join("");
      }),
    };
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    await beginMucCall(sender, "chan@muc.test", audioVideo);
    const state = $callState.get();
    expect(state).toEqual({
      phase: "active",
      peer: "chan@muc.test",
      sid: "chan@muc.test",
      media: audioVideo,
      join: {
        url: "wss://livekit.test",
        room: "chan@muc.test",
        identity: "alice@waddle.test/web",
        token: "jwt.payload.sig",
      },
      kind: "muc",
    });
  });

  test("tearDownActiveCall on a MUC call dispatches request-leave", async () => {
    const send_raw_iq = mock(async () => "<iq type='result' id='x'/>");
    $callState.set({
      phase: "active",
      peer: "chan@muc.test",
      sid: "chan@muc.test",
      media: audioVideo,
      join,
      kind: "muc",
    });
    await tearDownActiveCall({ send_raw_iq } as unknown as Parameters<typeof tearDownActiveCall>[0], "gone");
    expect(send_raw_iq).toHaveBeenCalledTimes(1);
    const xml = (send_raw_iq.mock.calls[0] as string[])[0];
    expect(xml).toContain('request-leave');
    expect(xml).toContain('chan@muc.test');
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
