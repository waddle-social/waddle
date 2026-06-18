import { afterEach, describe, expect, test } from "bun:test";
import {
  $callChatUnread,
  inboundCallChatThreadIds,
  isCallChatTabFocused,
  nextCallChatUnread,
  resetCallChatUnread,
  syncCallChatUnread,
} from "../src/lib/calls/call-chat-unread";
import { $callState, clearCallState } from "../src/lib/calls/call-store";

afterEach(() => {
  resetCallChatUnread();
  clearCallState();
});

describe("nextCallChatUnread", () => {
  test("counts inbound messages arriving while not focused as unread", () => {
    const result = nextCallChatUnread({
      inboundIds: ["m1", "m2", "m3"],
      lastSeenId: "m1",
      focused: false,
    });

    // m2 and m3 arrived after the last-seen watermark.
    expect(result.unread).toBe(2);
    // The watermark does not advance while the Chat tab is unfocused.
    expect(result.lastSeenId).toBe("m1");
  });

  test("clears the count and advances the watermark while focused", () => {
    const result = nextCallChatUnread({
      inboundIds: ["m1", "m2", "m3"],
      lastSeenId: "m1",
      focused: true,
    });

    expect(result.unread).toBe(0);
    // Reading the tab marks everything currently loaded as seen.
    expect(result.lastSeenId).toBe("m3");
  });

  test("treats every inbound message as unread when there is no watermark yet", () => {
    const result = nextCallChatUnread({
      inboundIds: ["m1", "m2"],
      lastSeenId: null,
      focused: false,
    });

    expect(result.unread).toBe(2);
    expect(result.lastSeenId).toBe(null);
  });
});

describe("inboundCallChatThreadIds", () => {
  test("keeps inbound messages on the call thread in order, dropping self and other threads", () => {
    const messages = [
      { id: "a", threadId: "call-1", isSelf: false },
      { id: "b", threadId: "other", isSelf: false },
      { id: "c", threadId: "call-1", isSelf: true },
      { id: "d", threadId: "call-1", isSelf: false },
    ];

    expect(inboundCallChatThreadIds(messages, "call-1")).toEqual(["a", "d"]);
  });

  test("returns nothing when there is no active call thread", () => {
    const messages = [{ id: "a", threadId: "call-1", isSelf: false }];
    expect(inboundCallChatThreadIds(messages, null)).toEqual([]);
  });

  test("excludes the call-anchor system row that shares the thread id", () => {
    // The call anchor (DM: threadId === sid; MUC: the thread root) carries the
    // same threadId but is a bodyless system card, not a chat message.
    const messages = [
      { id: "anchor", threadId: "call-1", isSelf: false, callThread: { kind: "muc" } },
      { id: "a", threadId: "call-1", isSelf: false },
    ];
    expect(inboundCallChatThreadIds(messages, "call-1")).toEqual(["a"]);
  });
});

describe("isCallChatTabFocused", () => {
  test("is focused only when the dock is open on the Chat tab", () => {
    expect(isCallChatTabFocused(true, "chat")).toBe(true);
    expect(isCallChatTabFocused(true, "participants")).toBe(false);
    expect(isCallChatTabFocused(false, "chat")).toBe(false);
  });
});

describe("$callChatUnread store", () => {
  test("accumulates while unfocused and clears once the Chat tab is focused", () => {
    expect($callChatUnread.get()).toBe(0);

    syncCallChatUnread(["m1", "m2"], false);
    expect($callChatUnread.get()).toBe(2);

    syncCallChatUnread(["m1", "m2", "m3"], false);
    expect($callChatUnread.get()).toBe(3);

    // Focusing the tab clears the badge and marks the loaded messages seen.
    syncCallChatUnread(["m1", "m2", "m3"], true);
    expect($callChatUnread.get()).toBe(0);

    // A later message arriving while unfocused only counts the new one.
    syncCallChatUnread(["m1", "m2", "m3", "m4"], false);
    expect($callChatUnread.get()).toBe(1);
  });

  test("resets to zero when the call leaves the active phase", () => {
    $callState.set({
      phase: "active",
      peer: "room@muc.waddle.test",
      sid: "c1",
      media: { audio: true, video: false },
      join: { url: "wss://livekit.test", room: "room@muc.waddle.test", identity: "me", token: "jwt" },
      kind: "muc",
      selfNick: "me",
      selfFullJid: "me@waddle.test/browser",
    });
    syncCallChatUnread(["m1", "m2"], false);
    expect($callChatUnread.get()).toBe(2);

    clearCallState();
    expect($callChatUnread.get()).toBe(0);
  });
});
