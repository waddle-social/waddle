import { describe, expect, mock, test } from "bun:test";
import { handleCallEventSideEffect } from "../src/lib/calls/call-effects";
import type { CallWireSender } from "../src/lib/calls/outbound";
import type { CallEvent, CallMedia, CallState, LiveKitJoin } from "../src/lib/calls/types";

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
    send_call_retract: mock(async () => undefined),
    send_call_finish: mock(async () => undefined),
    send_call_session_initiate: mock(async () => undefined),
    send_call_session_accept: mock(async () => undefined),
    send_call_session_terminate: mock(async () => undefined),
  };
}

describe("handleCallEventSideEffect", () => {
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
      from: "bob@waddle.test",
      sid: "c9",
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_session_initiate).not.toHaveBeenCalled();
  });

  test("fires session-accept when session-initiate lands on a matching incoming call", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "incoming",
      from: "alice@waddle.test",
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
      "alice@waddle.test/desktop", // initiator_full_jid
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
      from: "alice@waddle.test",
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
      from: "alice@waddle.test",
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
    };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test",
      sid: "c-new",
      media: audioVideo,
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_reject).toHaveBeenCalledTimes(1);
    expect(sender.send_call_reject).toHaveBeenCalledWith("dave@waddle.test", "c-new");
  });

  test("busy-reject: propose while outgoing fires reject", async () => {
    const sender = mockSender();
    const prev: CallState = {
      phase: "outgoing",
      to: "carol@waddle.test",
      sid: "c-existing",
      media: audioVideo,
    };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test",
      sid: "c-new",
      media: audioVideo,
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_reject).toHaveBeenCalledTimes(1);
  });

  test("busy-reject: propose while idle does NOT fire reject", async () => {
    const sender = mockSender();
    const prev: CallState = { phase: "idle" };
    const event: CallEvent = {
      kind: "propose",
      from: "dave@waddle.test",
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
      from: "dave@waddle.test",
      sid: "c-new",
      media: audioVideo,
    };
    await handleCallEventSideEffect(event, prev, sender, "alice@waddle.test/web-1");
    expect(sender.send_call_reject).not.toHaveBeenCalled();
  });
});
