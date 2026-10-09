// Direct unit tests for useChannelLiveMerge covering the per-XEP apply*
// helpers and the typed-dispatch path through handleRoomMessage.

import { afterEach, describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelLiveMerge } from "../src/channels/live-merge";
import type { LiveRoomMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { occupantJidDirectory } from "../src/lib/avatars/author-jid";
import { queuedRoomMessageToTimeline } from "../src/channels/message-timeline-state";
import { roomMessageFromArchived } from "../src/lib/xmpp/wasm-message-codecs";
import { __setFaroForTesting } from "../src/lib/telemetry";

afterEach(() => {
  __setFaroForTesting(null);
  occupantJidDirectory.clear();
});

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

  test("ignores unloaded targets and reports only real receive-processing failures", () => {
    const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({ api: { pushEvent: (name: string, attributes?: Record<string, string>) => {
      events.push({ name, attributes });
    } } } as never);
    const h = harness();
    h.liveMerge.applyDisplayed("outside-loaded-window", "bob");
    expect(events).toEqual([]);

    h.messages.value = [{
      id: "m1",
      createdAt: "2020-01-01T00:00:00Z",
      body: "",
      nick: "alice",
      get readBy(): string[] { throw new Error("broken row"); },
    } as TimelineMessage];
    h.liveMerge.applyDisplayed("m1", "bob");

    expect(events[0]).toEqual({
      name: "chat.xmpp.displayed_marker.failed",
      attributes: {
        direction: "receive",
        kind: "room",
        reason: "receive-processing-failed",
        round_trip_latency_band: "over-5s",
      },
    });
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

  test("correction replaces stale link preview state", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "old https://old.example",
        nick: "bob",
        authorJid: "room@muc.example.com/bob",
        timestamp: 0,
        linkPreviews: [{ originalUrl: "https://old.example", title: "Old" }],
      } as TimelineMessage,
    ];

    h.liveMerge.handleRoomMessage(makeLive({
      replacesId: "m1",
      body: "edited https://new.example",
      linkPreviews: [{ originalUrl: "https://new.example", title: "New" }],
    }));

    expect(h.messages.value[0]?.linkPreviews).toEqual([{ originalUrl: "https://new.example", title: "New" }]);

    h.liveMerge.handleRoomMessage(makeLive({
      replacesId: "m1",
      body: "edited without preview",
    }));

    expect(h.messages.value[0]?.linkPreviews).toBeUndefined();
  });

  test("plain message routes to mergeLiveMessage (append)", () => {
    const h = harness();
    const before = h.messages.value.length;
    const out = h.liveMerge.handleRoomMessage(makeLive({ body: "hi" }));
    expect(out.kind).toBe("live");
    expect(h.messages.value.length).toBe(before + 1);
  });

  test("call-thread-ended fastening updates the existing anchor instead of appending", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "anchor-local",
        replyableId: "anchor-stanza-id",
        body: "alice started a call",
        author: "alice",
        authorJid: "room@muc.example.com/alice",
        createdAt: "2026-06-07T14:30:00Z",
        createdAtSource: "archive",
        isSelf: false,
        threadId: "call-thread-uuid",
        callThread: {
          kind: "muc",
          sid: "session-uuid",
          media: ["audio"],
          initiator: "alice@example.com",
          started: "2026-06-07T14:30:00Z",
        },
      } as TimelineMessage,
    ];

    const out = h.liveMerge.handleRoomMessage(makeLive({
      id: "ended-event",
      body: "",
      callThreadEnded: {
        anchorId: "anchor-stanza-id",
        ended: "2026-06-07T14:35:00Z",
        duration: "PT5M",
      },
    }));

    expect(out.kind).toBe("ignore");
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.callThread).toEqual({
      kind: "muc",
      sid: "session-uuid",
      media: ["audio"],
      initiator: "alice@example.com",
      started: "2026-06-07T14:30:00Z",
      ended: "2026-06-07T14:35:00Z",
      duration: "PT5M",
    });
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
  test("preserves two occupants that reuse the same sender-selected id", () => {
    const h = harness();
    h.liveMerge.mergeLiveMessage({
      id: "room-stanza-alice",
      wireIds: ["shared-client-id"],
      author: "alice",
      authorJid: "room@muc.example.com/alice",
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: "alice@example.com/phone",
      body: "from alice",
      isSelf: false,
      createdAt: "2026-05-14T10:36:55.000Z",
      createdAtSource: "delay",
    });
    h.liveMerge.mergeLiveMessage({
      id: "room-stanza-bob",
      wireIds: ["shared-client-id"],
      author: "bob",
      authorJid: "room@muc.example.com/bob",
      authorOccupantJid: "room@muc.example.com/bob",
      authorRealJid: "bob@example.com/laptop",
      body: "from bob",
      isSelf: false,
      createdAt: "2026-05-14T10:36:56.000Z",
      createdAtSource: "delay",
    });

    expect(h.messages.value.map(({ id, body }) => ({ id, body }))).toEqual([
      { id: "room-stanza-alice", body: "from alice" },
      { id: "room-stanza-bob", body: "from bob" },
    ]);
  });

  test("preserves same-occupant messages with distinct room-authored stanza ids", () => {
    const h = harness();
    const sender = {
      wireIds: ["reused-client-id"],
      stanzaIdBy: "room@muc.example.com",
      author: "alice",
      authorJid: "alice@example.com/phone",
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: "alice@example.com/phone",
      isSelf: false,
      createdAtSource: "delay" as const,
    };
    h.liveMerge.mergeLiveMessage({
      ...sender,
      id: "room-stanza-1",
      stanzaId: "room-stanza-1",
      replyableId: "room-stanza-1",
      body: "first",
      createdAt: "2026-05-14T10:36:55Z",
    });
    h.liveMerge.mergeLiveMessage({
      ...sender,
      id: "room-stanza-2",
      stanzaId: "room-stanza-2",
      replyableId: "room-stanza-2",
      body: "second",
      createdAt: "2026-05-14T10:36:56Z",
    });

    expect(h.messages.value.map(({ id, body }) => ({ id, body }))).toEqual([
      { id: "room-stanza-1", body: "first" },
      { id: "room-stanza-2", body: "second" },
    ]);
  });

  test("reconciles the same real JID across wire casing differences", () => {
    const h = harness();
    const base = {
      id: "room-stanza-1",
      stanzaId: "room-stanza-1",
      stanzaIdBy: "room@muc.example.com",
      replyableId: "room-stanza-1",
      author: "alice",
      authorOccupantJid: "room@muc.example.com/alice",
      body: "same message",
      isSelf: false,
      createdAtSource: "delay" as const,
    };
    h.liveMerge.mergeLiveMessage({
      ...base,
      authorJid: "Alice@Example.COM/phone",
      authorRealJid: "Alice@Example.COM/phone",
      createdAt: "2026-05-14T10:36:55Z",
    });
    h.liveMerge.mergeLiveMessage({
      ...base,
      authorJid: "alice@example.com/laptop",
      authorRealJid: "alice@example.com/laptop",
      createdAt: "2026-05-14T10:36:56Z",
    });

    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.id).toBe("room-stanza-1");
  });

  test("preserves distinct room-stamped messages across nicks for the same bare real JID", () => {
    const h = harness();
    for (const [index, nick] of ["alice-phone", "alice-laptop"].entries()) {
      const stanzaId = `room-stanza-${index + 1}`;
      h.liveMerge.mergeLiveMessage({
        id: stanzaId,
        wireIds: ["reused-client-id"],
        stanzaId,
        stanzaIdBy: "room@muc.example.com",
        replyableId: stanzaId,
        author: nick,
        authorJid: `alice@example.com/${nick}`,
        authorOccupantJid: `room@muc.example.com/${nick}`,
        authorRealJid: `alice@example.com/${nick}`,
        body: `message ${index + 1}`,
        isSelf: false,
        createdAt: `2026-05-14T10:36:5${index + 5}Z`,
        createdAtSource: "delay",
      });
    }

    expect(h.messages.value.map((message) => message.id)).toEqual([
      "room-stanza-1",
      "room-stanza-2",
    ]);
  });

  test("preserves distinct room stamps when an anonymous nick is reused after departure", () => {
    const h = harness();
    for (const [index, body] of ["departed occupant", "replacement occupant"].entries()) {
      const stanzaId = `room-stanza-${index + 1}`;
      h.liveMerge.mergeLiveMessage({
        id: stanzaId,
        wireIds: ["reused-client-id"],
        stanzaId,
        stanzaIdBy: "room@muc.example.com",
        replyableId: stanzaId,
        author: "guest",
        authorJid: "room@muc.example.com/guest",
        authorOccupantJid: "room@muc.example.com/guest",
        body,
        isSelf: false,
        createdAt: `2026-05-14T10:36:5${index + 5}Z`,
        createdAtSource: "delay",
      });
    }

    expect(h.messages.value.map((message) => message.body)).toEqual([
      "departed occupant",
      "replacement occupant",
    ]);
  });

  test("reconciles sender IDs for the same real account across occupant nicks", () => {
    const h = harness();
    for (const [index, nick] of ["alice-phone", "alice-laptop"].entries()) {
      h.liveMerge.mergeLiveMessage({
        id: `envelope-${index + 1}`,
        wireIds: ["reused-client-id"],
        author: nick,
        authorJid: `alice@example.com/${nick}`,
        authorOccupantJid: `room@muc.example.com/${nick}`,
        authorRealJid: `alice@example.com/${nick}`,
        body: `message ${index + 1}`,
        isSelf: false,
        createdAt: `2026-05-14T10:36:5${index + 5}Z`,
        createdAtSource: "delay",
      });
    }

    expect(h.messages.value.map((message) => message.id)).toEqual(["envelope-2"]);
  });

  test("does not trust a reused nick when the known real JID changed", () => {
    const h = harness();
    h.liveMerge.mergeLiveMessage({
      id: "room-stanza-old-alice",
      wireIds: ["shared-client-id"],
      author: "alice",
      authorJid: "old-alice@example.com/phone",
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: "old-alice@example.com/phone",
      body: "before nick reuse",
      isSelf: false,
      createdAt: "2026-05-14T10:36:55.000Z",
      createdAtSource: "delay",
    });
    h.liveMerge.mergeLiveMessage({
      id: "room-stanza-new-alice",
      wireIds: ["shared-client-id"],
      author: "alice",
      authorJid: "new-alice@example.com/laptop",
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: "new-alice@example.com/laptop",
      body: "after nick reuse",
      isSelf: false,
      createdAt: "2026-05-14T10:36:56.000Z",
      createdAtSource: "delay",
    });

    expect(h.messages.value).toHaveLength(2);
  });

  test("promotes an optimistic sender id to the room-authored stanza id", () => {
    const h = harness();
    h.pendingEchoClientIds.add("client-id");
    h.messages.value = [{
      id: "client-id",
      author: "alice",
      authorJid: "room@muc.example.com/alice",
      authorOccupantJid: "room@muc.example.com/alice",
      body: "my message",
      isSelf: true,
      deliveryStatus: "sending",
      createdAt: "2026-05-14T10:36:55.000Z",
      createdAtSource: "queued",
    }];

    h.liveMerge.mergeLiveMessage({
      id: "room-stanza-self",
      wireIds: ["client-id"],
      author: "alice",
      authorJid: "alice@example.com/phone",
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: "alice@example.com/phone",
      body: "my message",
      isSelf: true,
      createdAt: "2026-05-14T10:36:56.000Z",
      createdAtSource: "delay",
    });

    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.id).toBe("room-stanza-self");
    expect(h.messages.value[0]?.wireIds).toEqual(["client-id"]);
    expect(h.messages.value[0]?.deliveryStatus).toBe("delivered");
  });

  test("a pending room send does not trust an echo identified only by a self nick", () => {
    const h = harness();
    h.pendingEchoClientIds.add("client-id");
    h.messages.value = [{
      id: "client-id", author: "alice", authorJid: "room@muc.example.com/alice",
      authorOccupantJid: "room@muc.example.com/alice", authorRealJid: session.jid,
      body: "my message", isSelf: true, deliveryStatus: "sending",
      createdAt: "2026-05-14T10:36:55Z", createdAtSource: "queued",
    }];
    h.liveMerge.mergeLiveMessage({
      id: "echo-id", wireIds: ["client-id"], author: "alice", authorJid: "room@muc.example.com/alice",
      authorOccupantJid: "room@muc.example.com/alice", body: "my message", isSelf: true,
      createdAt: "2026-05-14T10:36:56Z", createdAtSource: "delay",
    });
    expect(h.messages.value).toHaveLength(2);
    expect(h.pendingEchoClientIds.has("client-id")).toBe(true);
  });

  test("body echo fallback preserves distinct canonical room messages", () => {
    for (const tracked of [false, true]) {
      const h = harness();
      if (tracked) h.pendingEchoClientIds.add("first");
      const base: TimelineMessage = {
        id: "first", author: "alice", authorJid: session.jid, authorRealJid: session.jid,
        authorOccupantJid: "room@muc.example.com/alice", body: "same", isSelf: true,
        stanzaId: "first", stanzaIdBy: "room@muc.example.com", deliveryStatus: "sending",
        createdAt: "2026-05-14T10:36:55Z", createdAtSource: "queued",
      };
      h.messages.value = [base];
      h.liveMerge.mergeLiveMessage({ ...base, id: "second", stanzaId: "second", deliveryStatus: undefined, createdAtSource: "delay" });
      expect(h.messages.value).toHaveLength(2);
    }
  });

  test("preserved echo fallback refuses unknown sender rows without local provenance", () => {
    const h = harness();
    h.messages.value = [{
      id: "old", author: "alice", authorJid: "room@muc.example.com/alice",
      authorOccupantJid: "room@muc.example.com/alice", body: "same", isSelf: true,
      deliveryStatus: "failed", createdAt: "2026-05-14T10:36:55Z", createdAtSource: "delay",
    }];
    h.liveMerge.mergeLiveMessage({
      id: "new", author: "alice", authorJid: session.jid, authorRealJid: session.jid,
      authorOccupantJid: "room@muc.example.com/alice", body: "same", isSelf: true,
      createdAt: "2026-05-14T10:36:56Z", createdAtSource: "delay",
    });
    expect(h.messages.value).toHaveLength(2);
  });

  test("fails closed when sender-scoped aliases select multiple rows", () => {
    const h = harness();
    const sender = {
      author: "alice",
      authorJid: "alice@example.com/phone",
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: "alice@example.com/phone",
      isSelf: false,
      createdAtSource: "delay" as const,
    };
    h.messages.value = [
      { ...sender, id: "room-stanza-1", wireIds: ["alias-a"], body: "one", createdAt: "2026-05-14T10:36:55Z" },
      { ...sender, id: "room-stanza-2", wireIds: ["alias-b"], body: "two", createdAt: "2026-05-14T10:36:56Z" },
    ];

    h.liveMerge.mergeLiveMessage({
      ...sender,
      id: "room-stanza-3",
      wireIds: ["alias-a", "alias-b"],
      body: "three",
      createdAt: "2026-05-14T10:36:57Z",
    });

    expect(h.messages.value.map((message) => message.id)).toEqual([
      "room-stanza-1",
      "room-stanza-2",
      "room-stanza-3",
    ]);
  });

  test("sender-scopes a correction alias after a cross-occupant collision", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "room-stanza-alice",
        wireIds: ["shared-client-id"],
        author: "alice",
        authorJid: "room@muc.example.com/alice",
        authorOccupantJid: "room@muc.example.com/alice",
        body: "alice original",
        isSelf: false,
        createdAt: "2026-05-14T10:36:55Z",
        createdAtSource: "delay",
      },
      {
        id: "room-stanza-bob",
        wireIds: ["shared-client-id"],
        author: "bob",
        authorJid: "room@muc.example.com/bob",
        authorOccupantJid: "room@muc.example.com/bob",
        body: "bob original",
        isSelf: false,
        createdAt: "2026-05-14T10:36:56Z",
        createdAtSource: "delay",
      },
    ];

    h.liveMerge.applyCorrection("shared-client-id", "alice edited", {
      authorJid: "room@muc.example.com/alice",
    });
    h.liveMerge.applyRetraction(makeLive({
      retractsId: "room-stanza-bob",
      fromJid: "room@muc.example.com/bob",
      nick: "bob",
    }));

    expect(h.messages.value[0]).toMatchObject({ body: "alice edited", isEdited: true });
    expect(h.messages.value[1]).toMatchObject({ body: "", isRetracted: true });
  });

  test("fails closed when a correction alias selects multiple rows inside one sender scope", () => {
    const h = harness();
    const alice = {
      wireIds: ["shared-client-id"],
      author: "alice",
      authorJid: "room@muc.example.com/alice",
      authorOccupantJid: "room@muc.example.com/alice",
      isSelf: false,
      createdAtSource: "delay" as const,
    };
    h.messages.value = [
      { ...alice, id: "room-stanza-1", body: "first", createdAt: "2026-05-14T10:36:55Z" },
      { ...alice, id: "room-stanza-2", body: "second", createdAt: "2026-05-14T10:36:56Z" },
    ];

    h.liveMerge.applyCorrection("shared-client-id", "ambiguous edit", {
      authorJid: "room@muc.example.com/alice",
    });

    expect(h.messages.value.map((message) => message.body)).toEqual(["first", "second"]);
    expect(h.messages.value.every((message) => !message.isEdited)).toBe(true);
  });

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
        authorOccupantJid: "room@muc.example.com/alice",
        authorRealJid: session.jid,
        deliveryStatus: "sending",
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.mergeLiveMessage({
      id: "server-id",
      body: "my msg",
      nick: "alice",
      isSelf: true,
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: session.jid,
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
        authorOccupantJid: "room@muc.example.com/alice",
        authorRealJid: session.jid,
        deliveryStatus: "sending",
        createdAt: "2026-05-14T10:36:55.000Z",
      } as TimelineMessage,
    ];
    h.liveMerge.mergeLiveMessage({
      id: "d09c804f-f862-44df-8c7b-32e058cbf4ea",
      body: "duplicate-body",
      nick: "alice",
      isSelf: true,
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: session.jid,
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
        authorOccupantJid: "room@muc.example.com/alice",
        authorRealJid: session.jid,
        createdAt: "2026-05-14T10:36:55.000Z",
      } as TimelineMessage,
    ];
    h.liveMerge.mergeLiveMessage({
      id: "server-id",
      body: "canonical body",
      nick: "alice",
      isSelf: true,
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: session.jid,
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
        authorOccupantJid: "room@muc.example.com/alice",
        authorRealJid: session.jid,
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
      authorOccupantJid: "room@muc.example.com/alice",
      authorRealJid: session.jid,
      createdAt: "2026-05-14T10:36:56.000Z",
    } as TimelineMessage);

    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.deliveryStatus).toBe("delivered");
    expect(h.messages.value[0]?.linkPreviews).toBeUndefined();
  });
});

describe("live row author stamping (nick reuse)", () => {
  test("a live row keeps the occupant behind its nick at ingest after the nick is reused", async () => {
    const { occupantJidDirectory, authorAvatarJid } = await import("../src/lib/avatars/author-jid");
    occupantJidDirectory.clear();
    try {
      const h = harness();
      occupantJidDirectory.record("room@muc.example.com", "sam", "alice@example.com");
      h.liveMerge.handleRoomMessage(makeLive({
        id: "from-alice", nick: "sam", fromJid: "room@muc.example.com/sam", body: "hi from alice",
        createdAt: new Date().toISOString(),
      }));
      occupantJidDirectory.record("room@muc.example.com", "sam", "bob@example.com");
      h.liveMerge.handleRoomMessage(makeLive({
        id: "from-bob", nick: "sam", fromJid: "room@muc.example.com/sam", body: "hi from bob",
        createdAt: new Date().toISOString(),
      }));

      const byId = new Map(h.messages.value.map((row) => [row.id, row]));
      expect(authorAvatarJid(byId.get("from-alice")!)).toBe("alice@example.com");
      expect(authorAvatarJid(byId.get("from-bob")!)).toBe("bob@example.com");
    } finally {
      occupantJidDirectory.clear();
    }
  });

  test("a catch-up re-emission of a stamped row after a handover keeps the original author", async () => {
    const { occupantJidDirectory, authorAvatarJid } = await import("../src/lib/avatars/author-jid");
    occupantJidDirectory.clear();
    try {
      const h = harness();
      occupantJidDirectory.record("room@muc.example.com", "sam", "alice@example.com");
      const sentAt = new Date().toISOString();
      h.liveMerge.handleRoomMessage(makeLive({
        stanzaId: "from-alice", stanzaIdBy: "room@muc.example.com",
        id: "from-alice", nick: "sam", fromJid: "room@muc.example.com/sam", body: "hi from alice",
        createdAt: sentAt, createdAtSource: "fallback",
      }));
      await Bun.sleep(5);
      occupantJidDirectory.record("room@muc.example.com", "sam", "bob@example.com");
      // Reconnect catch-up re-emits the same stanza on the live path.
      h.liveMerge.handleRoomMessage(makeLive({
        stanzaId: "from-alice", stanzaIdBy: "room@muc.example.com",
        id: "from-alice", nick: "sam", fromJid: "room@muc.example.com/sam", body: "hi from alice",
        createdAt: sentAt, createdAtSource: "archive",
      }));

      expect(h.messages.value).toHaveLength(1);
      expect(authorAvatarJid(h.messages.value[0]!)).toBe("alice@example.com");
    } finally {
      occupantJidDirectory.clear();
    }
  });

  test("an unseen catch-up or replayed row is never stamped with the nick's current holder", async () => {
    const { occupantJidDirectory, authorAvatarJid } = await import("../src/lib/avatars/author-jid");
    occupantJidDirectory.clear();
    try {
      const h = harness();
      // Alice held "sam" when these were sent; Bob holds it by the time they arrive.
      occupantJidDirectory.record("room@muc.example.com", "sam", "bob@example.com");
      h.liveMerge.handleRoomMessage(makeLive({
        id: "missed-alice", nick: "sam", fromJid: "room@muc.example.com/sam", body: "sent while we were away",
        createdAt: new Date().toISOString(), createdAtSource: "archive",
      }));
      h.liveMerge.handleRoomMessage(makeLive({
        id: "delayed-alice", nick: "sam", fromJid: "room@muc.example.com/sam", body: "SM replay",
        createdAt: new Date().toISOString(), createdAtSource: "delay",
      }));

      const byId = new Map(h.messages.value.map((row) => [row.id, row]));
      expect(byId.get("missed-alice")?.authorAvatarJid).toBeUndefined();
      expect(authorAvatarJid(byId.get("missed-alice")!)).toBeNull();
      expect(authorAvatarJid(byId.get("delayed-alice")!)).toBeNull();
    } finally {
      occupantJidDirectory.clear();
    }
  });
});



describe("production live self echo without disclosed real JID", () => {
  const room = "room@muc.example.com";

  function echo(nick: string, source: "live" | "archive" = "live", timestamp?: string, archiveEnvelope = "archive-envelope") {
    return roomMessageFromArchived({
      mam_id: source === "live" ? "client-id" : archiveEnvelope, id: "client-id", origin_id: "client-id",
      stanza_id: "room-echo", stanza_id_by: room, from: `${room}/${nick}`,
      to: session.jid, body: "my message", message_type: "groupchat", is_muc: true,
      timestamp, reaction_emojis: [], markup_spans: [], mention_uris: [], references: [],
      is_sticker: false, shared_files: [], link_previews: [],
    }, source)!;
  }

  function queued(h: ReturnType<typeof harness>) {
    const row = queuedRoomMessageToTimeline(session, room, {
      kind: "room", roomJid: room, id: "client-id", body: "my message",
      createdAt: "2026-07-01T10:00:00Z",
    });
    h.messages.value = [row];
    h.pendingEchoClientIds.add("client-id");
    return row;
  }

  test("actual self presence reconciles an undelayed live echo lacking author_real_jid", () => {
    const h = harness();
    const local = queued(h);
    occupantJidDirectory.recordOwnNick(room, "alice");
    const incoming = echo("alice");
    expect(incoming.authorRealJid).toBeUndefined();
    expect(incoming.createdAtSource).toBe("fallback");
    expect(incoming.archiveId).toBeUndefined();
    h.liveMerge.handleRoomMessage(incoming);
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]).toMatchObject({ authorRealJid: session.jid, isSelf: true, deliveryStatus: "delivered", rowKey: local.rowKey });
    expect(h.pendingEchoClientIds.has("client-id")).toBe(false);
  });

  test("room-assigned own nick reconciles independently of the configured nick", () => {
    const h = harness();
    queued(h);
    occupantJidDirectory.recordOwnNick(room, "alice_2");
    h.liveMerge.handleRoomMessage(echo("alice_2"));
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]).toMatchObject({ authorRealJid: session.jid, authorOccupantJid: `${room}/alice_2`, isSelf: true, deliveryStatus: "delivered" });
  });

  test("absent, old-nick, delayed, and archive echoes cannot borrow own identity", () => {
    for (const scenario of ["no-presence", "avatar-only", "old-nick", "delay", "archive", "malformed-archive", "empty-archive"] as const) {
      occupantJidDirectory.clear();
      const h = harness();
      queued(h);
      if (scenario === "avatar-only") occupantJidDirectory.record(room, "alice", session.jid);
      if (scenario !== "no-presence" && scenario !== "avatar-only") occupantJidDirectory.recordOwnNick(room, scenario === "old-nick" ? "alice_2" : "alice");
      const incoming = echo("alice", scenario === "archive" || scenario === "malformed-archive" || scenario === "empty-archive" ? "archive" : "live", scenario === "delay" || scenario === "archive" ? "2026-07-01T10:00:00Z" : undefined, scenario === "empty-archive" ? "" : undefined);
      h.liveMerge.handleRoomMessage(incoming);
      expect(h.messages.value).toHaveLength(2);
      expect(h.messages.value.find((row) => row.id === "room-echo")?.authorRealJid).toBeUndefined();
      if (scenario === "malformed-archive" || scenario === "empty-archive") expect(h.messages.value.find((row) => row.id === "room-echo")?.authorAvatarJid).toBeUndefined();
      expect(h.pendingEchoClientIds.has("client-id")).toBe(true);
    }
  });

  test("codec preserves distinct raw client and origin IDs without claiming its envelope ID", () => {
    const incoming = roomMessageFromArchived({
      mam_id: "client-id", id: "client-id", origin_id: "different-origin", from: `${room}/alice`, body: "my message", message_type: "groupchat", is_muc: true,
      reaction_emojis: [], markup_spans: [], mention_uris: [], references: [], is_sticker: false, shared_files: [], link_previews: [],
    }, "live")!;
    expect(incoming.senderChosenIds).toEqual(["client-id", "different-origin"]);
    const h = harness();
    queued(h);
    occupantJidDirectory.recordOwnNick(room, "alice");
    h.liveMerge.handleRoomMessage(incoming);
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.senderChosenIds).toEqual(["client-id", "different-origin"]);
  });

  test("origin-only live wrappers never lend their fabricated envelope ID to authored fallback", () => {
    for (const stamped of [false, true]) {
      occupantJidDirectory.clear();
      occupantJidDirectory.recordOwnNick(room, "alice");
      const h = harness();
      const originOnly = (envelope: string, origin: string, canonical = "room-one") => roomMessageFromArchived({
        mam_id: envelope, origin_id: origin, from: `${room}/alice`, body: "origin only", message_type: "groupchat", is_muc: true,
        ...(stamped ? { stanza_id: canonical, stanza_id_by: room } : {}),
        reaction_emojis: [], markup_spans: [], mention_uris: [], references: [], is_sticker: false, shared_files: [], link_previews: [],
      }, "live")!;
      h.liveMerge.handleRoomMessage(originOnly("fabricated-one", "actual-origin"));
      h.liveMerge.handleRoomMessage(originOnly("fabricated-two", "actual-origin"));
      expect(h.messages.value).toHaveLength(1);
      expect(h.messages.value[0]?.senderChosenIds).toEqual(["actual-origin"]);
      h.liveMerge.handleRoomMessage(originOnly("fabricated-three", "fabricated-one", "room-two"));
      expect(h.messages.value).toHaveLength(2);
    }
  });

});
