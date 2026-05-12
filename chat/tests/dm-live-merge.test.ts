// Direct unit tests for useDmLiveMerge: per-XEP apply* helpers, the
// handleIncomingMessage dispatcher, and self-echo reconciliation for 1:1.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useDmLiveMerge } from "../src/dms/live-merge";
import type { LiveDmMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";

const session: WaddleSession = {
  username: "alice",
  jid: "alice@example.com/desktop",
  session_id: "tok",
  xmpp_websocket_url: "wss://example.com/ws",
};

function harness() {
  const messages = ref<TimelineMessage[]>([]);
  const pendingEchoClientIds = new Set<string>();
  const scrollToPinnedEdgeAndPin = mock(async () => true);
  const persistLastSeen = mock(() => {});
  const isFeedVisible = (m: TimelineMessage) => !m.threadId || m.id === m.threadId;

  const liveMerge = useDmLiveMerge({
    session: ref(session),
    messages,
    activePeerJid: ref("bob@example.com"),
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin,
    persistLastSeen,
    isFeedVisible,
  });

  return { messages, pendingEchoClientIds, liveMerge };
}

function makeLive(overrides: Partial<LiveDmMessage> = {}): LiveDmMessage {
  return {
    type: "message",
    peerJid: "bob@example.com",
    fromJid: "bob@example.com",
    nick: "bob",
    body: "hi",
    timestamp: Date.now(),
    isSelf: false,
    ...overrides,
  } as unknown as LiveDmMessage;
}

describe("applyDisplayed (XEP-0333)", () => {
  test("appends reader nick once and dedups", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    h.liveMerge.applyDisplayed("m1", "bob");
    h.liveMerge.applyDisplayed("m1", "bob");
    expect(h.messages.value[0]?.readBy).toEqual(["bob"]);
  });
});

describe("applyReaction (XEP-0444 replace semantics)", () => {
  test("adds an emoji", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    h.liveMerge.applyReaction("m1", "bob", ["🎉"]);
    expect(h.messages.value[0]?.reactions).toEqual({ "🎉": ["bob"] });
  });

  test("empty emoji list removes the sender's reactions", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", reactions: { "🎉": ["bob"] }, body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    h.liveMerge.applyReaction("m1", "bob", []);
    expect(h.messages.value[0]?.reactions).toBeUndefined();
  });
});

describe("handleIncomingMessage typed dispatch", () => {
  test("retraction routes to applyRetraction (with DM sender match)", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "to retract",
        nick: "bob",
        authorJid: "bob@example.com",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.handleIncomingMessage(makeLive({
      retractsId: "m1",
      retractionId: "r1",
      fromJid: "bob@example.com",
    }));
    expect(h.messages.value[0]?.isRetracted).toBe(true);
  });

  test("correction routes to applyCorrection", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "old",
        nick: "bob",
        authorJid: "bob@example.com",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.handleIncomingMessage(makeLive({
      replacesId: "m1",
      body: "edited",
      fromJid: "bob@example.com",
    }));
    expect(h.messages.value[0]?.body).toBe("edited");
    expect(h.messages.value[0]?.isEdited).toBe(true);
  });

  test("plain message routes to mergeLiveMessage and appends", () => {
    const h = harness();
    const before = h.messages.value.length;
    h.liveMerge.handleIncomingMessage(makeLive({ body: "hi" }));
    expect(h.messages.value.length).toBe(before + 1);
  });
});

describe("applyRetraction sender-match gate (XEP-0424)", () => {
  test("refuses to retract when sender doesn't match target's authorJid", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "from bob",
        nick: "bob",
        authorJid: "bob@example.com",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.applyRetraction(makeLive({
      retractsId: "m1",
      retractionId: "r1",
      fromJid: "mallory@example.com",
      nick: "mallory",
    }));
    expect(h.messages.value[0]?.isRetracted).toBeFalsy();
  });
});

describe("applyCorrection sender-match gate (XEP-0308)", () => {
  test("refuses to correct when fromJid doesn't match target", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "from bob",
        nick: "bob",
        authorJid: "bob@example.com",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.applyCorrection("m1", "hijacked!", "mallory@example.com");
    expect(h.messages.value[0]?.body).toBe("from bob");
    expect(h.messages.value[0]?.isEdited).toBeFalsy();
  });
});

describe("mergeLiveMessage self-echo reconciliation", () => {
  test("body-match fallback only targets messages tracked in pendingEchoClientIds", () => {
    const h = harness();
    h.pendingEchoClientIds.add("client-1");
    h.messages.value = [
      // Tracked pending — should match
      {
        id: "client-1",
        body: "same-body",
        isSelf: true,
        deliveryStatus: "sending",
        nick: "alice",
        timestamp: 0,
      } as TimelineMessage,
      // Not pending — must NOT be retargeted by a same-body echo
      {
        id: "older-self-already-delivered",
        body: "same-body",
        isSelf: true,
        deliveryStatus: "delivered",
        nick: "alice",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.mergeLiveMessage({
      id: "server-id",
      body: "same-body",
      nick: "alice",
      isSelf: true,
      timestamp: 0,
    } as TimelineMessage);
    // Pending one became delivered; older delivered one is untouched.
    expect(h.messages.value.find((m) => m.body === "same-body" && m.deliveryStatus === "delivered")).toBeTruthy();
    expect(h.pendingEchoClientIds.has("client-1")).toBe(false);
  });

  test("non-self incoming appends without reconciling", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", body: "earlier", nick: "alice", isSelf: true, timestamp: 0 } as TimelineMessage,
    ];
    h.liveMerge.mergeLiveMessage({
      id: "m2",
      body: "from bob",
      nick: "bob",
      isSelf: false,
      timestamp: 0,
    } as TimelineMessage);
    expect(h.messages.value.length).toBe(2);
  });
});
