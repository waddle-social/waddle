// Direct unit tests for useChannelLiveMerge covering the per-XEP apply*
// helpers and the typed-dispatch path through handleRoomMessage.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelLiveMerge } from "../src/channels/live-merge";
import type { LiveRoomMessage } from "../src/lib/xmpp-client";
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

  const liveMerge = useChannelLiveMerge({
    session: ref(session),
    messages,
    activeChannelId: ref("general"),
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin,
    persistLastSeen,
  });

  return { messages, pendingEchoClientIds, liveMerge, persistLastSeen };
}

function makeLive(overrides: Partial<LiveRoomMessage> = {}): LiveRoomMessage {
  return {
    type: "message",
    roomJid: "room@muc.example.com",
    fromJid: "room@muc.example.com/bob",
    nick: "bob",
    body: "hi",
    timestamp: Date.now(),
    isSelf: false,
    ...overrides,
  } as unknown as LiveRoomMessage;
}

describe("applyDisplayed (XEP-0333)", () => {
  test("appends the reader's nick to readBy", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", body: "hi", nick: "alice", timestamp: 0, isSelf: true } as TimelineMessage,
    ];
    h.liveMerge.applyDisplayed("m1", "bob");
    expect(h.messages.value[0]?.readBy).toEqual(["bob"]);
  });

  test("dedups when the same reader is already in readBy", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", body: "", nick: "", timestamp: 0, readBy: ["bob"] } as TimelineMessage,
    ];
    h.liveMerge.applyDisplayed("m1", "bob");
    expect(h.messages.value[0]?.readBy).toEqual(["bob"]);
  });
});

describe("applyReaction (XEP-0444 replace semantics)", () => {
  test("adds a new emoji from a new sender", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", reactionTargetId: "stanza-1", body: "", nick: "", timestamp: 0 } as TimelineMessage,
    ];
    h.liveMerge.applyReaction("stanza-1", "bob", ["👍"], "room@muc.example.com/bob");
    expect(h.messages.value[0]?.reactions).toEqual({ "👍": ["bob"] });
  });

  test("replace semantics: empty emojis set drops all of that sender's reactions", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        reactionTargetId: "stanza-1",
        reactions: { "👍": ["bob"], "❤️": ["bob"] },
        reactionSenders: { "👍": { "room@muc.example.com/bob": "bob" }, "❤️": { "room@muc.example.com/bob": "bob" } },
        body: "", nick: "", timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.applyReaction("stanza-1", "bob", [], "room@muc.example.com/bob");
    expect(h.messages.value[0]?.reactions).toBeUndefined();
  });

  test("non-matching reactionTargetId is ignored", () => {
    const h = harness();
    const original: TimelineMessage = {
      id: "m1",
      reactionTargetId: "stanza-1",
      body: "", nick: "", timestamp: 0,
    } as TimelineMessage;
    h.messages.value = [original];
    h.liveMerge.applyReaction("different-stanza", "bob", ["👍"], "x");
    expect(h.messages.value[0]?.reactions).toBeUndefined();
  });
});

describe("handleRoomMessage typed dispatch", () => {
  test("retraction routes to applyRetraction (with sender match) and reports kind", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "hi",
        nick: "bob",
        authorJid: "room@muc.example.com/bob",
        replyableId: "m1",
        timestamp: 0,
      } as TimelineMessage,
    ];
    const out = h.liveMerge.handleRoomMessage(makeLive({
      retractsId: "m1",
      retractionId: "r1",
    }));
    expect(out.kind).toBe("retraction");
    expect(h.messages.value[0]?.isRetracted).toBe(true);
  });

  test("correction routes to applyCorrection (with sender match) and reports kind", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "old",
        nick: "bob",
        authorJid: "room@muc.example.com/bob",
        timestamp: 0,
      } as TimelineMessage,
    ];
    const out = h.liveMerge.handleRoomMessage(makeLive({
      replacesId: "m1",
      body: "edited",
    }));
    expect(out.kind).toBe("correction");
    expect(h.messages.value[0]?.body).toBe("edited");
    expect(h.messages.value[0]?.isEdited).toBe(true);
  });

  test("plain message routes to mergeLiveMessage (append)", () => {
    const h = harness();
    const before = h.messages.value.length;
    const out = h.liveMerge.handleRoomMessage(makeLive({ body: "hi" }));
    expect(out.kind).toBe("live");
    expect(h.messages.value.length).toBe(before + 1);
  });

  test("ignore: non-message stanza doesn't touch the timeline", () => {
    const h = harness();
    const before = h.messages.value.length;
    const out = h.liveMerge.handleRoomMessage({ type: "presence" } as unknown as LiveRoomMessage);
    expect(out.kind).toBe("ignore");
    expect(h.messages.value.length).toBe(before);
  });
});

describe("applyRetraction sender-match gate (XEP-0424)", () => {
  test("retracts when sender matches the target's authorJid", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "alice's message",
        nick: "alice",
        authorJid: "room@muc.example.com/alice",
        replyableId: "m1",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.applyRetraction(makeLive({
      retractsId: "m1",
      retractionId: "r1",
      fromJid: "room@muc.example.com/alice",
      nick: "alice",
    }));
    expect(h.messages.value[0]?.isRetracted).toBe(true);
  });

  test("refuses to retract when sender doesn't match target's authorJid (spoof attempt)", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "alice's message",
        nick: "alice",
        authorJid: "room@muc.example.com/alice",
        replyableId: "m1",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.applyRetraction(makeLive({
      retractsId: "m1",
      retractionId: "r1",
      fromJid: "room@muc.example.com/mallory",
      nick: "mallory",
    }));
    expect(h.messages.value[0]?.isRetracted).toBeFalsy();
  });
});

describe("applyCorrection sender-match gate (XEP-0308)", () => {
  test("corrects when sender matches the target's authorJid", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "old text",
        nick: "alice",
        authorJid: "room@muc.example.com/alice",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.applyCorrection(
      "m1",
      "new text",
      { authorJid: "room@muc.example.com/alice" },
    );
    expect(h.messages.value[0]?.body).toBe("new text");
    expect(h.messages.value[0]?.isEdited).toBe(true);
  });

  test("refuses to correct when sender doesn't match (spoof attempt)", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "old text",
        nick: "alice",
        authorJid: "room@muc.example.com/alice",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.applyCorrection(
      "m1",
      "hijacked!",
      { authorJid: "room@muc.example.com/mallory" },
    );
    expect(h.messages.value[0]?.body).toBe("old text");
    expect(h.messages.value[0]?.isEdited).toBeFalsy();
  });
});

describe("mergeLiveMessage self-echo reconciliation", () => {
  test("keeps timeline ordered when an older catch-up message lands after a newer live message", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "newer",
        body: "newer",
        nick: "bob",
        isSelf: false,
        createdAt: "2026-05-14T10:36:56.000Z",
      } as TimelineMessage,
    ];
    h.liveMerge.mergeLiveMessage({
      id: "older",
      body: "older",
      nick: "bob",
      isSelf: false,
      createdAt: "2026-05-14T10:36:55Z",
    } as TimelineMessage);
    expect(h.messages.value.map((message) => message.id)).toEqual(["older", "newer"]);
  });

  test("reconciles by id when the server-assigned id matches a wireId of an optimistic insert", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "client-id",
        wireIds: ["server-id"],
        body: "my msg",
        nick: "alice",
        isSelf: true,
        deliveryStatus: "sending",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.mergeLiveMessage({
      id: "server-id",
      body: "my msg",
      nick: "alice",
      isSelf: true,
      timestamp: 0,
    } as TimelineMessage);
    expect(h.messages.value.length).toBe(1);
    expect(h.messages.value[0]?.deliveryStatus).toBe("delivered");
  });

  test("reconciles by body fallback when id misses but pendingEchoClientIds tracks the optimistic id", () => {
    const h = harness();
    h.pendingEchoClientIds.add("client-id");
    h.messages.value = [
      {
        id: "client-id",
        body: "duplicate-body",
        nick: "alice",
        isSelf: true,
        deliveryStatus: "sending",
        createdAt: "2026-05-14T10:36:55.000Z",
      } as TimelineMessage,
    ];
    h.liveMerge.mergeLiveMessage({
      id: "d09c804f-f862-44df-8c7b-32e058cbf4ea",
      body: "duplicate-body",
      nick: "alice",
      isSelf: true,
      createdAt: "2026-05-14T10:36:56.000Z",
    } as TimelineMessage);
    expect(h.messages.value.length).toBe(1);
    expect(h.messages.value[0]?.deliveryStatus).toBe("delivered");
    // Body-match fallback consumed the pending entry so a second same-body
    // send can't accidentally retarget it.
    expect(h.pendingEchoClientIds.has("client-id")).toBe(false);
  });

  test("non-self echo appends without reconciling", () => {
    const h = harness();
    h.messages.value = [
      { id: "m1", body: "earlier", nick: "alice", timestamp: 0 } as TimelineMessage,
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

  test("keeps incoming canonical fields when reconciling an optimistic self-echo", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "client-id",
        wireIds: ["server-id"],
        body: "draft body",
        nick: "alice",
        isSelf: true,
        createdAt: "2026-05-14T10:36:55.000Z",
      } as TimelineMessage,
    ];
    h.liveMerge.mergeLiveMessage({
      id: "server-id",
      body: "canonical body",
      nick: "alice",
      isSelf: true,
      createdAt: "2026-05-14T10:36:55.000Z",
      extensionAnnotations: [
        {
          extensionId: "github",
          annotationId: "a1",
          surfaceKind: "message-card",
          title: "GitHub",
          fields: {},
          actions: [],
        },
      ],
    } as TimelineMessage);
    expect(h.messages.value[0]?.body).toBe("canonical body");
    expect(h.messages.value[0]?.extensionAnnotations?.[0]?.annotationId).toBe("a1");
  });

  test("clears optimistic link preview when authoritative self-echo has none", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "client-id",
        wireIds: ["server-id"],
        body: "read https://example.com",
        nick: "alice",
        isSelf: true,
        deliveryStatus: "sending",
        linkPreviews: [{ originalUrl: "https://example.com", title: "Example" }],
        createdAt: "2026-05-14T10:36:55.000Z",
      } as TimelineMessage,
    ];

    h.liveMerge.mergeLiveMessage({
      id: "server-id",
      body: "read https://example.com",
      nick: "alice",
      isSelf: true,
      createdAt: "2026-05-14T10:36:56.000Z",
    } as TimelineMessage);

    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.deliveryStatus).toBe("delivered");
    expect(h.messages.value[0]?.linkPreviews).toBeUndefined();
  });
});
