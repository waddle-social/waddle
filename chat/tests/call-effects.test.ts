import { describe, expect, mock, test } from "bun:test";
import { handleCallEventSideEffect } from "../src/lib/calls/call-effects";
import type { CallWireSender } from "../src/lib/calls/outbound";
import type { CallEvent, CallMedia, CallState } from "../src/lib/calls/types";

const audioVideo: CallMedia = { audio: true, video: true };

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

  test("does not fire on non-proceed events", async () => {
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
});
