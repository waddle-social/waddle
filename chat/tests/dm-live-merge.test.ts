// Direct unit tests for useDmLiveMerge: per-XEP apply* helpers, the
// handleIncomingMessage dispatcher, and self-echo reconciliation for 1:1.

import { afterEach, describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useDmLiveMerge } from "../src/dms/live-merge";
import { buildDmCallStartedAnchor, dmCallAnchorId } from "../src/lib/calls/dm-call-anchor";
import type { LiveDmMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { __setFaroForTesting } from "../src/lib/telemetry";

afterEach(() => __setFaroForTesting(null));

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

  test("reports a receive merge failure but not an unloaded target", () => {
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

    expect(events[0]?.attributes).toEqual({
      direction: "receive",
      kind: "dm",
      reason: "receive-processing-failed",
      round_trip_latency_band: "over-5s",
    });
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

  test("correction never resurrects a retracted message (#1267 item 5)", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "",
        nick: "bob",
        authorJid: "bob@example.com",
        isRetracted: true,
        timestamp: 0,
      } as TimelineMessage,
    ];
    h.liveMerge.handleIncomingMessage(makeLive({
      replacesId: "m1",
      body: "resurrected content",
      fromJid: "bob@example.com",
    }));
    expect(h.messages.value[0]?.body).toBe("");
    expect(h.messages.value[0]?.isEdited).toBeUndefined();
  });

  test("MUC-PM correction requires the full occupant JID to match (#1256)", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "pm1",
        body: "original",
        nick: "juliet",
        authorJid: "room@muc.example.com/juliet",
        authorOccupantJid: "room@muc.example.com/juliet",
        timestamp: 0,
      } as TimelineMessage,
    ];
    // A different occupant of the SAME room shares the bare JID — the
    // full occupant JID must gate the edit (XEP-0308 business rules).
    h.liveMerge.handleIncomingMessage(makeLive({
      replacesId: "pm1",
      body: "hijacked",
      fromJid: "room@muc.example.com/iago",
    }));
    expect(h.messages.value[0]?.body).toBe("original");

    h.liveMerge.handleIncomingMessage(makeLive({
      replacesId: "pm1",
      body: "legit edit",
      fromJid: "room@muc.example.com/juliet",
    }));
    expect(h.messages.value[0]?.body).toBe("legit edit");
  });

  test("correction replaces stale link preview state", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "m1",
        body: "old https://old.example",
        nick: "bob",
        authorJid: "bob@example.com",
        timestamp: 0,
        linkPreviews: [{ originalUrl: "https://old.example", title: "Old" }],
      } as TimelineMessage,
    ];

    h.liveMerge.handleIncomingMessage(makeLive({
      replacesId: "m1",
      body: "edited https://new.example",
      fromJid: "bob@example.com",
      linkPreviews: [{ originalUrl: "https://new.example", title: "New" }],
    }));

    expect(h.messages.value[0]?.linkPreviews).toEqual([{ originalUrl: "https://new.example", title: "New" }]);

    h.liveMerge.handleIncomingMessage(makeLive({
      replacesId: "m1",
      body: "edited without preview",
      fromJid: "bob@example.com",
    }));

    expect(h.messages.value[0]?.linkPreviews).toBeUndefined();
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

  test("sender-scopes a reused alias before retracting", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "alice-canonical",
        wireIds: ["shared-id"],
        body: "alice message",
        nick: "alice",
        authorJid: "alice@example.com/phone",
        timestamp: 0,
      } as TimelineMessage,
      {
        id: "bob-canonical",
        wireIds: ["shared-id"],
        body: "bob message",
        nick: "bob",
        authorJid: "bob@example.com/laptop",
        timestamp: 1,
      } as TimelineMessage,
    ];

    h.liveMerge.applyRetraction(makeLive({
      retractsId: "shared-id",
      retractionId: "bob-retraction",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
    }));

    expect(h.messages.value[0]).toMatchObject({ body: "alice message" });
    expect(h.messages.value[1]).toMatchObject({ body: "", isRetracted: true });
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

  test("sender-scopes a reused alias before correcting", () => {
    const h = harness();
    h.messages.value = [
      {
        id: "alice-canonical",
        wireIds: ["shared-id"],
        body: "alice message",
        nick: "alice",
        authorJid: "alice@example.com/phone",
        timestamp: 0,
      } as TimelineMessage,
      {
        id: "bob-canonical",
        wireIds: ["shared-id"],
        body: "bob message",
        nick: "bob",
        authorJid: "bob@example.com/laptop",
        timestamp: 1,
      } as TimelineMessage,
    ];

    h.liveMerge.applyCorrection("shared-id", "bob edited", "bob@example.com/mobile");

    expect(h.messages.value[0]).toMatchObject({ body: "alice message" });
    expect(h.messages.value[1]).toMatchObject({ body: "bob edited", isEdited: true });
  });

  test("fails closed when one sender claims a correction or retraction alias twice", () => {
    const h = harness();
    const bob = {
      wireIds: ["shared-id"],
      nick: "bob",
      authorJid: "bob@example.com/laptop",
    };
    h.messages.value = [
      { ...bob, id: "bob-1", body: "first", timestamp: 0 } as TimelineMessage,
      { ...bob, id: "bob-2", body: "second", timestamp: 1 } as TimelineMessage,
    ];

    h.liveMerge.applyCorrection("shared-id", "ambiguous edit", "bob@example.com/mobile");
    h.liveMerge.applyRetraction(makeLive({
      retractsId: "shared-id",
      fromJid: "bob@example.com/mobile",
      nick: "bob",
    }));

    expect(h.messages.value.map((message) => message.body)).toEqual(["first", "second"]);
    expect(h.messages.value.every((message) => !message.isEdited && !message.isRetracted)).toBe(true);
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
        createdAt: "2026-05-14T10:36:55.000Z",
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
      id: "d09c804f-f862-44df-8c7b-32e058cbf4ea",
      body: "same-body",
      nick: "alice",
      isSelf: true,
      createdAt: "2026-05-14T10:36:56.000Z",
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
      createdAt: "2026-05-14T10:36:56.000Z",
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

describe("dm call-anchor dedup via the live-merge path", () => {
  test("a MAM call anchor collapses onto the synthesized live card by sid alias", () => {
    const h = harness();
    // The live synthesized "started a call" card is already in the timeline.
    const synth = buildDmCallStartedAnchor(
      {
        peerBareJid: "bob@example.com",
        sid: "sid-x",
        media: { audio: true, video: false },
        initiator: "alice@example.com/desktop",
        started: "2026-06-09T12:00:00Z",
      },
      session.jid,
    );
    h.messages.value = [synth];

    // The archived `<proceed/>` row for the same call arrives via the live
    // merge with a distinct primary id but the `dmcall:<sid>` wire alias.
    const mamCard: TimelineMessage = {
      id: "proceed-stanza-id",
      wireIds: [dmCallAnchorId("sid-x")],
      author: "bob",
      authorJid: "bob@example.com/phone",
      body: "",
      createdAt: "2026-06-09T12:00:05Z",
      createdAtSource: "archive",
      isSelf: false,
      threadId: "sid-x",
      callThread: {
        kind: "dm",
        sid: "sid-x",
        media: ["audio"],
        initiator: "alice@example.com/desktop",
        started: "2026-06-09T12:00:00Z",
      },
    };

    h.liveMerge.mergeLiveMessage(mamCard);

    expect(h.messages.value.filter((m) => m.callThread)).toHaveLength(1);
  });
});
