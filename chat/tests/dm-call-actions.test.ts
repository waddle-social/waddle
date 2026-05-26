import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  $callState,
  clearCallState,
} from "../src/lib/calls/call-store";
import {
  answerIncomingDmCallActivity,
  startDmCallAction,
} from "../src/lib/calls/dm-call-actions";
import {
  applyDmCallEvent,
  clearDmCallActivities,
  clearDmCallActivity,
  readDmCallActivity,
} from "../src/lib/calls/dm-call-activity";
import type { CallWireSender } from "../src/lib/calls/outbound";

afterEach(() => {
  clearCallState();
  clearDmCallActivities();
});

describe("startDmCallAction", () => {
  test("sends a fresh XEP-0353 propose and marks the peer as ringing", async () => {
    const sender: CallWireSender = {
      send_call_propose: mock(async () => undefined),
    };

    await startDmCallAction({
      peerBareJid: "bob@waddle.test",
      media: { audio: true, video: true },
      getSender: () => sender,
      getInitiator: () => "alice@waddle.test/web",
    });

    expect(sender.send_call_propose).toHaveBeenCalledTimes(1);
    const [peerJid, sid, audio, video] = (sender.send_call_propose as {
      mock: { calls: unknown[][] };
    }).mock.calls[0] ?? [];
    expect(peerJid).toBe("bob@waddle.test");
    expect(typeof sid).toBe("string");
    expect(String(sid)).toMatch(/^c/);
    expect(audio).toBe(true);
    expect(video).toBe(true);
    expect($callState.get()).toMatchObject({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid,
      initiator: "alice@waddle.test/web",
    });
    expect(readDmCallActivity("bob@waddle.test")).toMatchObject({
      sid,
      state: "ringing",
      direction: "outgoing",
    });
  });

  test("rolls back optimistic state when the propose fails", async () => {
    const sender: CallWireSender = {
      send_call_propose: mock(async () => {
        throw new Error("wire down");
      }),
    };

    await startDmCallAction({
      peerBareJid: "bob@waddle.test",
      media: { audio: true, video: false },
      getSender: () => sender,
    });

    expect($callState.get()).toEqual({ phase: "idle" });
    expect(readDmCallActivity("bob@waddle.test")).toBeNull();
  });

  test("answers a hydrated incoming call with the stored proposer full JID", async () => {
    const sender: CallWireSender = {
      send_call_proceed: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: "bob@waddle.test/phone",
        sid: "hydrated-incoming",
        media: { audio: true, video: true },
      },
      selfBareJid: "alice@waddle.test",
      to: "alice@waddle.test/web",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    await answerIncomingDmCallActivity({
      peerBareJid: "bob@waddle.test",
      proposerFullJid: "bob@waddle.test/phone",
      sid: "hydrated-incoming",
      media: { audio: true, video: true },
      getSender: () => sender,
    });

    expect(sender.send_call_proceed).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "hydrated-incoming",
    );
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "bob@waddle.test/phone",
      sid: "hydrated-incoming",
      media: { audio: true, video: true },
      accepting: true,
    });
  });

  test("does not answer a hydrated incoming call without a matching peer full JID", async () => {
    const sender: CallWireSender = {
      send_call_proceed: mock(async () => undefined),
    };

    await answerIncomingDmCallActivity({
      peerBareJid: "bob@waddle.test",
      proposerFullJid: "mallory@waddle.test/phone",
      sid: "forged",
      media: { audio: true, video: false },
      getSender: () => sender,
    });

    expect(sender.send_call_proceed).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("does not answer when the hydrated incoming activity was already cleared", async () => {
    const sender: CallWireSender = {
      send_call_proceed: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: "bob@waddle.test/phone",
        sid: "stale-call",
        media: { audio: true, video: false },
      },
      selfBareJid: "alice@waddle.test",
      to: "alice@waddle.test/web",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });
    clearDmCallActivity("bob@waddle.test", "stale-call");

    await answerIncomingDmCallActivity({
      peerBareJid: "bob@waddle.test",
      proposerFullJid: "bob@waddle.test/phone",
      sid: "stale-call",
      media: { audio: true, video: false },
      getSender: () => sender,
    });

    expect(sender.send_call_proceed).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("answers an existing live incoming call through the current full JID", async () => {
    const sender: CallWireSender = {
      send_call_proceed: mock(async () => undefined),
    };
    $callState.set({
      phase: "incoming",
      from: "bob@waddle.test/laptop",
      sid: "same-call",
      media: { audio: true, video: false },
    });

    await answerIncomingDmCallActivity({
      peerBareJid: "bob@waddle.test",
      proposerFullJid: "bob@waddle.test/stale-phone",
      sid: "same-call",
      media: { audio: true, video: true },
      getSender: () => sender,
    });

    expect(sender.send_call_proceed).toHaveBeenCalledWith(
      "bob@waddle.test/laptop",
      "same-call",
    );
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "bob@waddle.test/laptop",
      sid: "same-call",
      media: { audio: true, video: false },
      accepting: true,
    });
  });
});
