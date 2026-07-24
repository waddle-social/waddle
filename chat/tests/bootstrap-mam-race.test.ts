// Regression tests for #675: the initial MAM bootstrap in `loadMessages`
// wholesale-replaced `messages.value` after the `queryMamPage` await, wiping
// any live message that `mergeLiveMessage` sorted into the timeline during
// the await window (XEP-0313 live-vs-archive delivery is intentionally
// asynchronous — the latest MAM page must not be assumed to contain
// in-flight live deliveries).

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelMamPaging } from "../src/channels/mam-paging";
import { useDmMamPaging } from "../src/dms/mam-paging";
import { dmMessageFromArchived } from "@/lib/xmpp/wasm-message-codecs";
import { fromLiveDmMessage } from "../src/dms/message-timeline-state";
import type { LiveDmMessage } from "../src/lib/xmpp-client";
import { roomMessageFromArchived } from "@/lib/xmpp/wasm-message-codecs";
import { mapLiveRoomMessageToTimeline } from "../src/channels/timeline";
import { insertLiveMessage } from "../src/lib/messaging/timeline-insert";
import type { BrowserXmppClient, LiveRoomMessage } from "../src/lib/xmpp-client";
import type { ChannelSummary } from "../src/lib/chat-types";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { WasmArchivedMessage } from "../src/lib/xmpp/wasm-types";

const session: WaddleSession = {
  session_id: "tok",
  user_id: "alice-id",
  username: "alice",
  avatar_url: null,
  xmpp_localpart: "alice",
  jid: "alice@example.com/desktop",
  xmpp_websocket_url: "wss://example.com/ws",
  is_expired: false,
  expires_at: null,
};

const channel: ChannelSummary = { id: "space_room", name: "Room" };

const baseArchivedRoom: WasmArchivedMessage = {
  mam_id: "",
  message_type: "groupchat",
  from: "space_room@muc.example.com/bob",
  to: "alice@example.com",
  body: "hello",
  reaction_emojis: [],
  is_muc: true,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

function archivedRoomRow(id: string, body: string, timestamp: string): LiveRoomMessage {
  return roomMessageFromArchived({
    ...baseArchivedRoom,
    mam_id: id,
    stanza_id: id,
    stanza_id_by: "space_room@muc.example.com",
    body,
    timestamp,
  })! as LiveRoomMessage;
}

function liveRoomRow(id: string, body: string, timestamp: string): LiveRoomMessage {
  return roomMessageFromArchived(
    {
      ...baseArchivedRoom,
      mam_id: "",
      stanza_id: id,
      stanza_id_by: "space_room@muc.example.com",
      body,
      timestamp,
    },
    "live",
  )! as LiveRoomMessage;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("useChannelMamPaging.loadMessages — bootstrap race (#675)", () => {
  test("preserves a live message that arrives during the queryMamPage await", async () => {
    const mamRows = [
      archivedRoomRow("arch-1", "older", "2026-07-01T09:00:00.000Z"),
      archivedRoomRow("arch-2", "newer", "2026-07-01T09:30:00.000Z"),
    ];
    const mamPage = deferred<{ messages: LiveRoomMessage[]; firstArchiveId: string; complete: boolean }>();

    const queryMamPage = mock(async () => mamPage.promise);
    const xmppClient = {
      queryMamPage,
      fetchRoomPins: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const pendingEchoClientIds = new Set<string>();
    const paging = useChannelMamPaging({
      session: ref(session),
      xmppClient: ref(xmppClient),
      activeSpaceId: ref("space"),
      activeChannelId: ref("space_room"),
      currentChannel: ref(channel),
      messages,
      firstUnseenId: ref<string | null>(null),
      timelineEl: ref(null),
      scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      normalizeError: (e) => String(e),
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    const loadPromise = paging.loadMessages("space", "space_room");
    // Let loadMessages reach the queryMamPage await.
    await Promise.resolve();

    // A live message lands mid-bootstrap, exactly as mergeLiveMessage would
    // sort it into messages.value.
    const live = mapLiveRoomMessageToTimeline(
      session,
      liveRoomRow("live-1", "live during bootstrap", "2026-07-01T10:05:00.000Z"),
    );
    messages.value = insertLiveMessage(messages.value, live, pendingEchoClientIds).messages;
    expect(messages.value.some((m) => m.id === "live-1")).toBe(true);

    // The MAM page (which does not contain the live message) lands after.
    mamPage.resolve({ messages: mamRows, firstArchiveId: "arch-1", complete: true });
    await expect(loadPromise).resolves.toBe("loaded");

    const ids = messages.value.map((m) => m.id);
    // The live arrival must survive the bootstrap merge...
    expect(ids).toContain("live-1");
    // ...in chronological position (it is the newest message)...
    expect(ids[ids.length - 1]).toBe("live-1");
    // ...alongside the full MAM page.
    expect(ids).toContain("arch-1");
    expect(ids).toContain("arch-2");
  });

  test("does not duplicate a live message whose MAM copy is in the page (XEP-0359 id parity)", async () => {
    // The archived copy carries the same room-stamped stanza-id the live
    // delivery carried, plus its MAM archive id.
    const archivedCopy = roomMessageFromArchived({
      ...baseArchivedRoom,
      mam_id: "mam-uid-9",
      stanza_id: "live-1",
      stanza_id_by: "space_room@muc.example.com",
      body: "live during bootstrap",
      timestamp: "2026-07-01T10:05:00.000Z",
    })! as LiveRoomMessage;
    const mamRows = [archivedRoomRow("arch-1", "older", "2026-07-01T09:00:00.000Z"), archivedCopy];
    const mamPage = deferred<{ messages: LiveRoomMessage[]; firstArchiveId: string; complete: boolean }>();

    const xmppClient = {
      queryMamPage: mock(async () => mamPage.promise),
      fetchRoomPins: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const pendingEchoClientIds = new Set<string>();
    const paging = useChannelMamPaging({
      session: ref(session),
      xmppClient: ref(xmppClient),
      activeSpaceId: ref("space"),
      activeChannelId: ref("space_room"),
      currentChannel: ref(channel),
      messages,
      firstUnseenId: ref<string | null>(null),
      timelineEl: ref(null),
      scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      normalizeError: (e) => String(e),
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    const loadPromise = paging.loadMessages("space", "space_room");
    await Promise.resolve();

    const live = mapLiveRoomMessageToTimeline(
      session,
      liveRoomRow("live-1", "live during bootstrap", "2026-07-01T10:05:00.000Z"),
    );
    messages.value = insertLiveMessage(messages.value, live, pendingEchoClientIds).messages;

    mamPage.resolve({ messages: mamRows, firstArchiveId: "arch-1", complete: true });
    await expect(loadPromise).resolves.toBe("loaded");

    // Exactly one row for that message, reachable via the live stanza-id,
    // and the full timeline is exactly two rows.
    const matches = messages.value.filter(
      (m) => m.id === "live-1" || (m.wireIds ?? []).includes("live-1"),
    );
    expect(matches).toHaveLength(1);
    expect(messages.value).toHaveLength(2);
    expect(matches[0]!.body).toBe("live during bootstrap");
  });
});

describe("bootstrap failure keeps live arrivals (#675 review)", () => {
  test("a failed MAM query does not wipe a live message that arrived during the await", async () => {
    const mamPage = deferred<never>();
    const xmppClient = {
      queryMamPage: mock(async () => mamPage.promise),
      fetchRoomPins: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const actionError = ref("");
    const pendingEchoClientIds = new Set<string>();
    const paging = useChannelMamPaging({
      session: ref(session),
      xmppClient: ref(xmppClient),
      activeSpaceId: ref("space"),
      activeChannelId: ref("space_room"),
      currentChannel: ref(channel),
      messages,
      firstUnseenId: ref<string | null>(null),
      timelineEl: ref(null),
      scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError,
      clearActionError: () => {},
      normalizeError: (e) => String(e),
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    const loadPromise = paging.loadMessages("space", "space_room");
    await Promise.resolve();

    const live = mapLiveRoomMessageToTimeline(
      session,
      liveRoomRow("live-1", "live during bootstrap", "2026-07-01T10:05:00.000Z"),
    );
    messages.value = insertLiveMessage(messages.value, live, pendingEchoClientIds).messages;

    mamPage.reject(new Error("mam unavailable"));
    await expect(loadPromise).resolves.toBe("failed");

    expect(messages.value.some((m) => m.id === "live-1")).toBe(true);
    // A kept live arrival must not hide that history failed to load —
    // only queued self-sends suppress the error (pre-existing behavior).
    expect(actionError.value).not.toBe("");
  });
});

describe("bootstrap merge keeps the unread divider anchored (#675 review)", () => {
  test("live arrivals during the load do not shift the divider past unread history", async () => {
    // unreadAtLoad was counted before the load; a foreign live message
    // arriving during the await extends the timeline tail and must be
    // counted as unread too — otherwise the divider lands K rows too
    // new and genuinely-unread history renders as read.
    const mamRows = [
      archivedRoomRow("arch-1", "read earlier", "2026-07-01T09:00:00.000Z"),
      archivedRoomRow("arch-2", "unread at load", "2026-07-01T09:30:00.000Z"),
    ];
    const mamPage = deferred<{ messages: LiveRoomMessage[]; firstArchiveId: string; complete: boolean }>();

    const xmppClient = {
      queryMamPage: mock(async () => mamPage.promise),
      fetchRoomPins: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const firstUnseenId = ref<string | null>(null);
    const pendingEchoClientIds = new Set<string>();
    const paging = useChannelMamPaging({
      session: ref(session),
      xmppClient: ref(xmppClient),
      activeSpaceId: ref("space"),
      activeChannelId: ref("space_room"),
      currentChannel: ref(channel),
      messages,
      firstUnseenId,
      timelineEl: ref(null),
      scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      normalizeError: (e) => String(e),
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    const loadPromise = paging.loadMessages("space", "space_room", 1);
    await Promise.resolve();

    const live = mapLiveRoomMessageToTimeline(
      session,
      liveRoomRow("live-1", "live during bootstrap", "2026-07-01T10:05:00.000Z"),
    );
    messages.value = insertLiveMessage(messages.value, live, pendingEchoClientIds).messages;

    mamPage.resolve({ messages: mamRows, firstArchiveId: "arch-1", complete: true });
    await expect(loadPromise).resolves.toBe("loaded");

    // arch-2 was the one unread message at load time; the live arrival
    // is also unread — the divider anchors at arch-2, not at live-1.
    expect(firstUnseenId.value).toBe("arch-2");
  });
});

describe("bootstrap merge keeps archive-applied retractions (#675 review)", () => {
  test("re-inserting the live original does not resurrect a tombstoned message (XEP-0424)", async () => {
    // The MAM page contains the original AND its retraction — the built
    // timeline row is a tombstone. The live copy of the (pre-retraction)
    // original merged during the await must not reintroduce its content
    // or clear the tombstone.
    const original = archivedRoomRow("live-1", "soon retracted", "2026-07-01T10:05:00.000Z");
    const retraction = roomMessageFromArchived({
      ...baseArchivedRoom,
      mam_id: "mam-retract-1",
      stanza_id: "retract-1",
      stanza_id_by: "space_room@muc.example.com",
      body: "",
      retracts_id: "live-1",
      timestamp: "2026-07-01T10:05:30.000Z",
    })! as LiveRoomMessage;
    const mamPage = deferred<{ messages: LiveRoomMessage[]; firstArchiveId: string; complete: boolean }>();

    const xmppClient = {
      queryMamPage: mock(async () => mamPage.promise),
      fetchRoomPins: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const pendingEchoClientIds = new Set<string>();
    const paging = useChannelMamPaging({
      session: ref(session),
      xmppClient: ref(xmppClient),
      activeSpaceId: ref("space"),
      activeChannelId: ref("space_room"),
      currentChannel: ref(channel),
      messages,
      firstUnseenId: ref<string | null>(null),
      timelineEl: ref(null),
      scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      normalizeError: (e) => String(e),
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    const loadPromise = paging.loadMessages("space", "space_room");
    await Promise.resolve();

    const live = mapLiveRoomMessageToTimeline(
      session,
      liveRoomRow("live-1", "soon retracted", "2026-07-01T10:05:00.000Z"),
    );
    messages.value = insertLiveMessage(messages.value, live, pendingEchoClientIds).messages;

    mamPage.resolve({ messages: [original, retraction], firstArchiveId: "mam-1", complete: true });
    await expect(loadPromise).resolves.toBe("loaded");

    const row = messages.value.find(
      (m) => m.id === "live-1" || (m.wireIds ?? []).includes("live-1"),
    );
    expect(row).toBeDefined();
    expect(row!.isRetracted).toBe(true);
    expect(row!.body).toBe("");
  });
});

describe("bootstrap merge keeps archive-applied corrections (#675 review)", () => {
  test("re-inserting the live original does not clobber a corrected body (XEP-0308)", async () => {
    // The MAM page contains the original AND its correction — the built
    // timeline row carries the corrected body. The live copy of the
    // (uncorrected) original merged during the await must not overwrite
    // it.
    const original = archivedRoomRow("live-1", "original body", "2026-07-01T10:05:00.000Z");
    const correction = roomMessageFromArchived({
      ...baseArchivedRoom,
      mam_id: "mam-corr-1",
      stanza_id: "corr-1",
      stanza_id_by: "space_room@muc.example.com",
      body: "corrected body",
      replaces_id: "live-1",
      timestamp: "2026-07-01T10:05:30.000Z",
    })! as LiveRoomMessage;
    const mamPage = deferred<{ messages: LiveRoomMessage[]; firstArchiveId: string; complete: boolean }>();

    const xmppClient = {
      queryMamPage: mock(async () => mamPage.promise),
      fetchRoomPins: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const pendingEchoClientIds = new Set<string>();
    const paging = useChannelMamPaging({
      session: ref(session),
      xmppClient: ref(xmppClient),
      activeSpaceId: ref("space"),
      activeChannelId: ref("space_room"),
      currentChannel: ref(channel),
      messages,
      firstUnseenId: ref<string | null>(null),
      timelineEl: ref(null),
      scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      normalizeError: (e) => String(e),
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    const loadPromise = paging.loadMessages("space", "space_room");
    await Promise.resolve();

    const live = mapLiveRoomMessageToTimeline(
      session,
      liveRoomRow("live-1", "original body", "2026-07-01T10:05:00.000Z"),
    );
    messages.value = insertLiveMessage(messages.value, live, pendingEchoClientIds).messages;

    mamPage.resolve({ messages: [original, correction], firstArchiveId: "mam-1", complete: true });
    await expect(loadPromise).resolves.toBe("loaded");

    const row = messages.value.find(
      (m) => m.id === "live-1" || (m.wireIds ?? []).includes("live-1"),
    );
    expect(row).toBeDefined();
    expect(row!.body).toBe("corrected body");
    expect(row!.isEdited).toBe(true);
  });
});

const peerJid = "bob@example.com";

const baseArchivedDm: WasmArchivedMessage = {
  mam_id: "",
  message_type: "chat",
  from: peerJid,
  to: "alice@example.com",
  body: "hello",
  reaction_emojis: [],
  is_muc: false,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

function archivedDmRow(id: string, body: string, timestamp: string): LiveDmMessage {
  return dmMessageFromArchived(
    { ...baseArchivedDm, mam_id: id, stanza_id: id, stanza_id_by: "alice@example.com", body, timestamp },
    "alice@example.com",
  )! as LiveDmMessage;
}

function liveDmRow(id: string, body: string, timestamp: string): LiveDmMessage {
  return dmMessageFromArchived(
    { ...baseArchivedDm, mam_id: "", origin_id: id, body, timestamp },
    "alice@example.com",
    "live",
  )! as LiveDmMessage;
}

describe("useDmMamPaging.loadMessages — bootstrap race (#675)", () => {
  test("preserves a live DM that arrives during the queryPersonalMamPage await", async () => {
    const mamRows = [
      archivedDmRow("dm-arch-1", "older", "2026-07-01T09:00:00.000Z"),
      archivedDmRow("dm-arch-2", "newer", "2026-07-01T09:30:00.000Z"),
    ];
    const mamPage = deferred<{ messages: LiveDmMessage[]; firstArchiveId: string; complete: boolean }>();

    const xmppClient = {
      queryPersonalMamPage: mock(async () => mamPage.promise),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const pendingEchoClientIds = new Set<string>();
    const paging = useDmMamPaging({
      session: ref(session),
      xmppClient: ref(xmppClient),
      activePeerJid: ref(peerJid),
      messages,
      firstUnseenId: ref<string | null>(null),
      loadErrorPeerJid: ref<string | null>(null),
      loadErrorMessage: ref(""),
      timelineEl: ref(null),
      scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      scrollToPinnedEdgeAndPin: async () => true,
      isFeedVisible: (m) => !m.threadId || m.id === m.threadId,
      persistLastSeen: () => {},
      dmLoadErrorMessage: () => "load error",
    });

    const loadPromise = paging.loadMessages(peerJid);
    await Promise.resolve();

    const live = fromLiveDmMessage(
      session,
      liveDmRow("dm-live-1", "live during bootstrap", "2026-07-01T10:05:00.000Z"),
    );
    messages.value = insertLiveMessage(messages.value, live, pendingEchoClientIds).messages;
    expect(messages.value.some((m) => m.id === "dm-live-1")).toBe(true);

    mamPage.resolve({ messages: mamRows, firstArchiveId: "dm-arch-1", complete: true });
    await expect(loadPromise).resolves.toBe("loaded");

    const ids = messages.value.map((m) => m.id);
    expect(ids).toContain("dm-live-1");
    expect(ids[ids.length - 1]).toBe("dm-live-1");
    expect(ids).toContain("dm-arch-1");
    expect(ids).toContain("dm-arch-2");
  });

  test("does not duplicate a live DM whose MAM copy is in the page (XEP-0359 id parity)", async () => {
    const archivedCopy = dmMessageFromArchived(
      {
        ...baseArchivedDm,
        mam_id: "dm-mam-uid-9",
        origin_id: "dm-live-1",
        body: "live during bootstrap",
        timestamp: "2026-07-01T10:05:00.000Z",
      },
      "alice@example.com",
    )! as LiveDmMessage;
    const mamRows = [archivedDmRow("dm-arch-1", "older", "2026-07-01T09:00:00.000Z"), archivedCopy];
    const mamPage = deferred<{ messages: LiveDmMessage[]; firstArchiveId: string; complete: boolean }>();

    const xmppClient = {
      queryPersonalMamPage: mock(async () => mamPage.promise),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const pendingEchoClientIds = new Set<string>();
    const paging = useDmMamPaging({
      session: ref(session),
      xmppClient: ref(xmppClient),
      activePeerJid: ref(peerJid),
      messages,
      firstUnseenId: ref<string | null>(null),
      loadErrorPeerJid: ref<string | null>(null),
      loadErrorMessage: ref(""),
      timelineEl: ref(null),
      scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      scrollToPinnedEdgeAndPin: async () => true,
      isFeedVisible: (m) => !m.threadId || m.id === m.threadId,
      persistLastSeen: () => {},
      dmLoadErrorMessage: () => "load error",
    });

    const loadPromise = paging.loadMessages(peerJid);
    await Promise.resolve();

    const live = fromLiveDmMessage(
      session,
      liveDmRow("dm-live-1", "live during bootstrap", "2026-07-01T10:05:00.000Z"),
    );
    messages.value = insertLiveMessage(messages.value, live, pendingEchoClientIds).messages;

    mamPage.resolve({ messages: mamRows, firstArchiveId: "dm-arch-1", complete: true });
    await expect(loadPromise).resolves.toBe("loaded");

    const matches = messages.value.filter(
      (m) => m.id === "dm-live-1" || (m.wireIds ?? []).includes("dm-live-1"),
    );
    expect(matches).toHaveLength(1);
    expect(messages.value).toHaveLength(2);
    expect(matches[0]!.body).toBe("live during bootstrap");
  });
});
