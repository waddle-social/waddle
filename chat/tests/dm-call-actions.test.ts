import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  $callState,
  clearCallState,
} from "../src/lib/calls/call-store";
import { startDmCallAction } from "../src/lib/calls/dm-call-actions";
import {
  clearDmCallActivities,
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
});
