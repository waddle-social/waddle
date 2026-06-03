import { describe, expect, mock, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { ChannelSummary } from "../src/lib/chat-types";
import type { WaddleSession } from "../src/lib/server-auth";
import type { LiveRoomMessage, MamHistoryPage } from "../src/lib/xmpp-client";
import type { WasmArchivedMessage } from "../src/lib/xmpp/wasm-types";
import type { InboxEntry } from "../src/lib/xmpp/inbox-types";
import { applyEntries, createInboxState } from "../src/services/inbox";
import {
  findChannelForRoomJid,
  lastFeedVisibleUnread,
  lastThreadUnread,
  selectUnreadRoomCandidates,
  useUnreadOverview,
} from "../src/lib/unread-overview-state";

type TestOverviewClient = NonNullable<Parameters<typeof useUnreadOverview>[0]["xmppClient"]["value"]>;

function makeMessage(
  partial: Partial<TimelineMessage> & { id: string; createdAt: string },
): TimelineMessage {
  return { author: "alice", body: "", isSelf: false, ...partial };
}

function muc(partial: Partial<InboxEntry> & { partner: string }): InboxEntry {
  return {
    kind: "muc",
    lastStanzaId: "s",
    lastUpdated: 0,
    unread: 0,
    ...partial,
  };
}

const ROOM = "general@muc.waddle.example";
const channels: ChannelSummary[] = [
  { id: "general", name: "General", jid: ROOM, spaceId: "space" },
];

const session: WaddleSession = {
  session_id: "session-1",
  user_id: "user-1",
  username: "alice",
  avatar_url: null,
  xmpp_localpart: "alice",
  jid: "alice@waddle.example/web",
  xmpp_websocket_url: "wss://xmpp.waddle.example/ws",
  is_expired: false,
  expires_at: null,
};

function liveRoomMessage(partial: Partial<LiveRoomMessage> & { id: string }): LiveRoomMessage {
  return {
    roomJid: ROOM,
    nick: "alice",
    body: "",
    createdAt: "2026-01-01T00:00:00.000Z",
    createdAtSource: "archive",
    type: "message",
    ...partial,
  };
}

function mamPage(messages: LiveRoomMessage[]): MamHistoryPage<LiveRoomMessage> {
  return { messages, complete: true };
}

function archivedRoomMessage(
  partial: Partial<WasmArchivedMessage> & { mam_id: string },
): WasmArchivedMessage {
  return {
    message_type: "groupchat",
    from: `${ROOM}/alice`,
    to: ROOM,
    body: "",
    timestamp: "2026-01-01T00:00:00.000Z",
    reaction_emojis: [],
    is_muc: true,
    markup_spans: [],
    mention_uris: [],
    references: [],
    is_sticker: false,
    shared_files: [],
    link_previews: [],
    ...partial,
  };
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

async function flushOverviewRefresh() {
  for (let i = 0; i < 4; i += 1) {
    await nextTick();
    await Promise.resolve();
  }
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
}

function createOverview(options: {
  state: ReturnType<typeof createInboxState>;
  channels?: ChannelSummary[];
  client: TestOverviewClient;
}) {
  const scope = effectScope();
  const xmppClient = ref<TestOverviewClient | null>(options.client);
  const sessionRef = ref<WaddleSession | null>(session);
  const channelsRef = ref<ChannelSummary[]>(options.channels ?? channels);
  const inboxState = ref(options.state);
  const overview = scope.run(() =>
    useUnreadOverview({
      xmppClient,
      session: sessionRef,
      channels: channelsRef,
      inboxState,
    }),
  );
  if (!overview) throw new Error("failed to create unread overview");
  return { channelsRef, inboxState, overview, scope };
}

describe("selectUnreadRoomCandidates", () => {
  test("includes channel-level and thread-level unread, excludes DMs and zeros", () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 3, lastUpdated: 100 }),
      muc({ partner: ROOM, thread: "t1", threadTitle: "Deploys", unread: 2, lastUpdated: 150 }),
      muc({ partner: ROOM, thread: "t2", unread: 0, lastUpdated: 90 }), // read thread, skipped
      { partner: "bob@waddle.example", kind: "direct", lastStanzaId: "s", lastUpdated: 999, unread: 5 },
      muc({ partner: "quiet@muc.waddle.example", unread: 0, lastUpdated: 10 }), // no unread, skipped
    ]);

    const candidates = selectUnreadRoomCandidates(state);
    expect(candidates).toHaveLength(1);
    const room = candidates[0]!;
    expect(room.roomJid).toBe(ROOM);
    expect(room.channelUnread).toBe(3);
    expect(room.lastUpdated).toBe(150);
    expect(room.threads).toHaveLength(1);
    expect(room.threads[0]).toMatchObject({ threadId: "t1", unread: 2, title: "Deploys" });
  });

  test("surfaces a room that has only unread threads (no channel-level unread)", () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, thread: "t1", preview: "hi there", unread: 1, lastUpdated: 50 }),
    ]);
    const candidates = selectUnreadRoomCandidates(state);
    expect(candidates).toHaveLength(1);
    expect(candidates[0]!.channelUnread).toBe(0);
    expect(candidates[0]!.threads[0]).toMatchObject({ threadId: "t1", title: "hi there" });
  });

  test("sorts rooms most-recently-active first", () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: "a@muc.x", unread: 1, lastUpdated: 10 }),
      muc({ partner: "b@muc.x", unread: 1, lastUpdated: 30 }),
      muc({ partner: "c@muc.x", unread: 1, lastUpdated: 20 }),
    ]);
    expect(selectUnreadRoomCandidates(state).map((c) => c.roomJid)).toEqual([
      "b@muc.x",
      "c@muc.x",
      "a@muc.x",
    ]);
  });
});

describe("lastFeedVisibleUnread", () => {
  const messages = [
    makeMessage({ id: "m1", createdAt: "2026-01-01T00:00:00Z" }),
    makeMessage({ id: "r1", createdAt: "2026-01-01T00:01:00Z", threadId: "m1" }), // threaded reply
    makeMessage({ id: "m2", createdAt: "2026-01-01T00:02:00Z" }),
    makeMessage({ id: "m3", createdAt: "2026-01-01T00:03:00Z" }),
  ];

  test("takes the last N feed-visible (non-threaded) messages", () => {
    expect(lastFeedVisibleUnread(messages, 2).map((m) => m.id)).toEqual(["m2", "m3"]);
  });

  test("treats a thread root (id === threadId) as feed-visible", () => {
    const withRoot = [makeMessage({ id: "m1", createdAt: "t", threadId: "m1" })];
    expect(lastFeedVisibleUnread(withRoot, 1).map((m) => m.id)).toEqual(["m1"]);
  });

  test("clamps when unread exceeds available feed-visible messages", () => {
    expect(lastFeedVisibleUnread(messages, 99).map((m) => m.id)).toEqual(["m1", "m2", "m3"]);
  });

  test("returns nothing for non-positive unread", () => {
    expect(lastFeedVisibleUnread(messages, 0)).toEqual([]);
  });
});

describe("lastThreadUnread", () => {
  const messages = [
    makeMessage({ id: "a", createdAt: "t1" }),
    makeMessage({ id: "b", createdAt: "t2" }),
    makeMessage({ id: "c", createdAt: "t3" }),
  ];

  test("takes the last N messages and clamps", () => {
    expect(lastThreadUnread(messages, 2).map((m) => m.id)).toEqual(["b", "c"]);
    expect(lastThreadUnread(messages, 99).map((m) => m.id)).toEqual(["a", "b", "c"]);
    expect(lastThreadUnread(messages, 0)).toEqual([]);
  });
});

describe("findChannelForRoomJid", () => {
  test("matches on the channel JID", () => {
    expect(findChannelForRoomJid(ROOM, channels)?.id).toBe("general");
  });

  test("falls back to the JID local-part slug for managed rooms", () => {
    const slugOnly: ChannelSummary[] = [{ id: "general", name: "General" }];
    expect(findChannelForRoomJid(ROOM, slugOnly)?.id).toBe("general");
  });

  test("returns null when the room is not in the topology", () => {
    expect(findChannelForRoomJid("missing@muc.waddle.example", channels)).toBeNull();
  });
});

describe("useUnreadOverview", () => {
  test("folds XEP-0308 corrections before selecting channel and thread unread rows", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 2, lastUpdated: 100 }),
      muc({
        partner: ROOM,
        thread: "thread-1",
        threadTitle: "Deploys",
        unread: 1,
        lastUpdated: 110,
      }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1",
          body: "uncorrected channel message",
          createdAt: "2026-01-01T00:00:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-1-edit",
          replacesId: "channel-1",
          body: "corrected channel message",
          createdAt: "2026-01-01T00:01:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-2",
          body: "next channel message",
          createdAt: "2026-01-01T00:02:00.000Z",
        }),
      ]));
    const queryMamThreadPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "thread-message-1",
          threadId: "thread-1",
          body: "uncorrected thread reply",
          createdAt: "2026-01-01T00:03:00.000Z",
        }),
        liveRoomMessage({
          id: "thread-message-1-edit",
          threadId: "thread-1",
          replacesId: "thread-message-1",
          body: "corrected thread reply",
          createdAt: "2026-01-01T00:04:00.000Z",
        }),
      ]));
    const { overview, scope } = createOverview({
      state,
      client: { queryMamPage, queryMamThreadPage } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    const group = overview.groups.value[0]!;
    expect(overview.error.value).toBeNull();
    expect(group.channelMessages.map((message) => message.id)).toEqual([
      "channel-1",
      "channel-2",
    ]);
    expect(group.channelMessages[0]).toMatchObject({
      body: "corrected channel message",
      isEdited: true,
    });
    expect(group.channelMessages.some((message) => message.id === "channel-1-edit")).toBe(false);
    expect(group.threads[0]?.messages.map((message) => message.id)).toEqual(["thread-message-1"]);
    expect(group.threads[0]?.messages[0]).toMatchObject({
      body: "corrected thread reply",
      isEdited: true,
    });
    scope.stop();
  });

  test("folds XEP-0424 retractions instead of rendering retraction rows", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1",
          wireIds: ["stanza-channel-1"],
          replyableId: "stanza-channel-1",
          body: "message to retract",
          createdAt: "2026-01-01T00:00:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-1-retract",
          retractsId: "stanza-channel-1",
          retractionId: "channel-1-retract",
          createdAt: "2026-01-01T00:01:00.000Z",
        }),
      ]));
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { overview, scope } = createOverview({
      state,
      client: { queryMamPage, queryMamThreadPage } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(overview.error.value).toBeNull();
    expect(overview.groups.value[0]?.channelMessages).toHaveLength(1);
    const retracted = overview.groups.value[0]?.channelMessages[0];
    expect(retracted?.id).toBe("channel-1");
    expect(retracted?.body).toBe("");
    expect(retracted?.isRetracted).toBe(true);
    expect(
      overview.groups.value[0]?.channelMessages.some((message) => message.id === "channel-1-retract"),
    ).toBe(false);
    scope.stop();
  });

  test("folds XEP-0444 reactions onto the target before selecting unread rows", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1",
          body: "message with reaction",
          reactionTargetId: "stanza-channel-1",
          createdAt: "2026-01-01T00:00:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-1-reaction",
          nick: "bob",
          _reactionTarget: "stanza-channel-1",
          _reactionEmojis: ["👍"],
          _reactionSenderId: "bob@waddle.example",
          createdAt: "2026-01-01T00:01:00.000Z",
        }),
      ]));
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { overview, scope } = createOverview({
      state,
      client: { queryMamPage, queryMamThreadPage } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(overview.error.value).toBeNull();
    expect(overview.groups.value[0]?.channelMessages).toHaveLength(1);
    const reacted = overview.groups.value[0]?.channelMessages[0];
    expect(reacted?.id).toBe("channel-1");
    expect(reacted?.body).toBe("message with reaction");
    expect(reacted?.reactions).toEqual({ "👍": ["bob"] });
    expect(reacted?.reactionSenders).toEqual({ "👍": { "bob@waddle.example": "bob" } });
    expect(
      overview.groups.value[0]?.channelMessages.some((message) => message.id === "channel-1-reaction"),
    ).toBe(false);
    scope.stop();
  });

  test("backfills stanza-id targets when the unread MAM page only has update rows", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 2, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1-reaction",
          nick: "bob",
          _reactionTarget: "stanza-channel-1",
          _reactionEmojis: ["👍"],
          _reactionSenderId: "bob@waddle.example",
          createdAt: "2026-01-01T00:10:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-2-retract",
          retractsId: "stanza-channel-2",
          retractionId: "channel-2-retract",
          createdAt: "2026-01-01T00:11:00.000Z",
        }),
      ]));
    const fetchRoomMessagesByStanzaIds = mock(async (
      _spaceId: string,
      _channelId: string,
      stanzaIds: string[],
    ) => {
      expect(stanzaIds).toEqual(["stanza-channel-1", "stanza-channel-2"]);
      return [
        archivedRoomMessage({
          mam_id: "mam-channel-1",
          id: "channel-1",
          body: "older reacted target",
          timestamp: "2026-01-01T00:00:00.000Z",
          stanza_ids: [{ id: "stanza-channel-1", by: ROOM }],
        }),
        archivedRoomMessage({
          mam_id: "mam-channel-2",
          id: "channel-2",
          body: "older retracted target",
          timestamp: "2026-01-01T00:01:00.000Z",
          stanza_ids: [{ id: "stanza-channel-2", by: ROOM }],
        }),
      ];
    });
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { overview, scope } = createOverview({
      state,
      client: {
        queryMamPage,
        queryMamThreadPage,
        fetchRoomMessagesByStanzaIds,
      } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(fetchRoomMessagesByStanzaIds).toHaveBeenCalledTimes(1);
    expect(overview.error.value).toBeNull();
    expect(overview.groups.value[0]?.channelMessages).toHaveLength(2);
    const reacted = overview.groups.value[0]?.channelMessages.find((message) =>
      message.id === "channel-1"
    );
    expect(reacted?.body).toBe("older reacted target");
    expect(reacted?.reactions).toEqual({ "👍": ["bob"] });
    const retracted = overview.groups.value[0]?.channelMessages.find((message) =>
      message.id === "channel-2"
    );
    expect(retracted?.body).toBe("");
    expect(retracted?.isRetracted).toBe(true);
    expect(
      overview.groups.value[0]?.channelMessages.some((message) =>
        message.id === "channel-1-reaction" || message.id === "channel-2-retract"
      ),
    ).toBe(false);
    scope.stop();
  });

  test("selects a folded update target by update recency on mixed headroom pages", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "read-channel-1",
          body: "already read headroom 1",
          createdAt: "2026-01-01T00:05:00.000Z",
        }),
        liveRoomMessage({
          id: "read-channel-2",
          body: "already read headroom 2",
          createdAt: "2026-01-01T00:06:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-1-reaction",
          nick: "bob",
          _reactionTarget: "stanza-channel-1",
          _reactionEmojis: ["👍"],
          _reactionSenderId: "bob@waddle.example",
          createdAt: "2026-01-01T00:10:00.000Z",
        }),
      ]));
    const fetchRoomMessagesByStanzaIds = mock(async () => [
      archivedRoomMessage({
        mam_id: "mam-channel-1",
        id: "channel-1",
        body: "old reacted target",
        timestamp: "2026-01-01T00:00:00.000Z",
        stanza_ids: [{ id: "stanza-channel-1", by: ROOM }],
      }),
    ]);
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { overview, scope } = createOverview({
      state,
      client: {
        queryMamPage,
        queryMamThreadPage,
        fetchRoomMessagesByStanzaIds,
      } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(overview.groups.value[0]?.channelMessages.map((message) => message.id)).toEqual([
      "channel-1",
    ]);
    expect(overview.groups.value[0]?.channelMessages[0]?.reactions).toEqual({ "👍": ["bob"] });
    scope.stop();
  });

  test("filters stanza-id backfill rows that do not carry a requested room stamp", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1-reaction",
          nick: "bob",
          _reactionTarget: "stanza-channel-1",
          _reactionEmojis: ["👍"],
          _reactionSenderId: "bob@waddle.example",
          createdAt: "2026-01-01T00:10:00.000Z",
        }),
      ]));
    const fetchRoomMessagesByStanzaIds = mock(async () => [
      archivedRoomMessage({
        mam_id: "mam-rogue",
        id: "stanza-channel-1",
        body: "wrong row",
        timestamp: "2026-01-01T00:00:00.000Z",
      }),
    ]);
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { overview, scope } = createOverview({
      state,
      client: {
        queryMamPage,
        queryMamThreadPage,
        fetchRoomMessagesByStanzaIds,
      } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(fetchRoomMessagesByStanzaIds).toHaveBeenCalledTimes(1);
    expect(overview.groups.value).toEqual([]);
    scope.stop();
  });

  test("does not select fetched retraction context when sender validation rejects the update", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1-retract",
          nick: "mallory",
          retractsId: "stanza-channel-1",
          retractionId: "channel-1-retract",
          createdAt: "2026-01-01T00:10:00.000Z",
        }),
      ]));
    const fetchRoomMessagesByStanzaIds = mock(async () => [
      archivedRoomMessage({
        mam_id: "mam-channel-1",
        id: "channel-1",
        body: "alice target",
        timestamp: "2026-01-01T00:00:00.000Z",
        stanza_ids: [{ id: "stanza-channel-1", by: ROOM }],
      }),
    ]);
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { overview, scope } = createOverview({
      state,
      client: {
        queryMamPage,
        queryMamThreadPage,
        fetchRoomMessagesByStanzaIds,
      } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(fetchRoomMessagesByStanzaIds).toHaveBeenCalledTimes(1);
    expect(overview.groups.value).toEqual([]);
    scope.stop();
  });

  test("ignores rejected duplicate retractions after a valid fold when choosing last unread", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1",
          body: "target",
          createdAt: "2026-01-01T00:00:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-1-retract",
          retractsId: "channel-1",
          retractionId: "channel-1-retract",
          createdAt: "2026-01-01T00:01:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-2",
          body: "newer regular unread",
          createdAt: "2026-01-01T00:02:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-1-retract-invalid",
          nick: "mallory",
          retractsId: "channel-1",
          retractionId: "channel-1-retract-invalid",
          createdAt: "2026-01-01T00:03:00.000Z",
        }),
      ]));
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { overview, scope } = createOverview({
      state,
      client: { queryMamPage, queryMamThreadPage } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(overview.error.value).toBeNull();
    expect(overview.groups.value[0]?.channelMessages.map((message) => message.id)).toEqual([
      "channel-2",
    ]);
    scope.stop();
  });

  test("ignores rejected duplicate corrections after a valid fold when choosing last unread", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1",
          body: "original body",
          createdAt: "2026-01-01T00:00:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-1-edit",
          replacesId: "channel-1",
          body: "corrected body",
          createdAt: "2026-01-01T00:01:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-2",
          body: "newer regular unread",
          createdAt: "2026-01-01T00:02:00.000Z",
        }),
        liveRoomMessage({
          id: "channel-1-edit-invalid",
          nick: "mallory",
          replacesId: "channel-1",
          body: "corrected body",
          createdAt: "2026-01-01T00:03:00.000Z",
        }),
      ]));
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { overview, scope } = createOverview({
      state,
      client: { queryMamPage, queryMamThreadPage } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(overview.error.value).toBeNull();
    expect(overview.groups.value[0]?.channelMessages.map((message) => message.id)).toEqual([
      "channel-2",
    ]);
    scope.stop();
  });

  test("does not send XEP-0308 origin ids to the stanza-id backfill fetcher", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1-edit",
          replacesId: "origin-channel-1",
          body: "corrected body",
          createdAt: "2026-01-01T00:10:00.000Z",
        }),
      ]));
    const fetchRoomMessagesByStanzaIds = mock(async () => [
      archivedRoomMessage({
        mam_id: "mam-channel-1",
        id: "channel-1",
        body: "old target",
        origin_id: "origin-channel-1",
        timestamp: "2026-01-01T00:00:00.000Z",
        stanza_ids: [{ id: "stanza-channel-1", by: ROOM }],
      }),
    ]);
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { overview, scope } = createOverview({
      state,
      client: {
        queryMamPage,
        queryMamThreadPage,
        fetchRoomMessagesByStanzaIds,
      } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(fetchRoomMessagesByStanzaIds).not.toHaveBeenCalled();
    expect(overview.groups.value).toEqual([]);
    scope.stop();
  });

  test("refreshes unread groups when channel topology arrives after inbox state", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
    ]);
    const queryMamPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "channel-1",
          body: "late topology message",
          createdAt: "2026-01-01T00:00:00.000Z",
        }),
      ]));
    const queryMamThreadPage = mock(async () => mamPage([]));
    const { channelsRef, overview, scope } = createOverview({
      state,
      channels: [],
      client: { queryMamPage, queryMamThreadPage } as TestOverviewClient,
    });

    await flushOverviewRefresh();

    expect(overview.groups.value).toEqual([]);
    expect(queryMamPage).not.toHaveBeenCalled();

    channelsRef.value = channels;
    await flushOverviewRefresh();

    expect(queryMamPage).toHaveBeenCalledTimes(1);
    expect(overview.groups.value[0]?.channelMessages.map((message) => message.id)).toEqual([
      "channel-1",
    ]);
    scope.stop();
  });

  test("does not launch stale thread MAM fetches after refresh invalidation", async () => {
    const initialState = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
      muc({
        partner: ROOM,
        thread: "thread-1",
        threadTitle: "Deploys",
        unread: 1,
        lastUpdated: 110,
      }),
    ]);
    const staleChannelPage = deferred<MamHistoryPage<LiveRoomMessage>>();
    let channelPageCalls = 0;
    const queryMamPage = mock(async () => {
      channelPageCalls += 1;
      if (channelPageCalls === 1) return staleChannelPage.promise;
      return mamPage([
        liveRoomMessage({
          id: "fresh-channel-1",
          body: "fresh unread message",
          createdAt: "2026-01-01T00:02:00.000Z",
        }),
      ]);
    });
    const queryMamThreadPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "stale-thread-message",
          threadId: "thread-1",
          body: "should not fetch",
          createdAt: "2026-01-01T00:03:00.000Z",
        }),
      ]));
    const { inboxState, overview, scope } = createOverview({
      state: initialState,
      client: { queryMamPage, queryMamThreadPage } as TestOverviewClient,
    });

    await nextTick();
    await Promise.resolve();
    expect(queryMamPage).toHaveBeenCalledTimes(1);

    inboxState.value = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 200 }),
    ]);
    await flushOverviewRefresh();
    expect(queryMamPage).toHaveBeenCalledTimes(2);

    staleChannelPage.resolve(mamPage([
      liveRoomMessage({
        id: "stale-channel-1",
        body: "stale unread message",
        createdAt: "2026-01-01T00:00:00.000Z",
      }),
    ]));
    await flushOverviewRefresh();

    expect(queryMamThreadPage).not.toHaveBeenCalled();
    expect(overview.groups.value[0]?.channelMessages.map((message) => message.id)).toEqual([
      "fresh-channel-1",
    ]);
    scope.stop();
  });

  test("invalidates an in-flight refresh when the overview scope is disposed", async () => {
    const state = applyEntries(createInboxState(), [
      muc({ partner: ROOM, unread: 1, lastUpdated: 100 }),
      muc({
        partner: ROOM,
        thread: "thread-1",
        threadTitle: "Deploys",
        unread: 1,
        lastUpdated: 110,
      }),
    ]);
    const staleChannelPage = deferred<MamHistoryPage<LiveRoomMessage>>();
    const queryMamPage = mock(async () => staleChannelPage.promise);
    const queryMamThreadPage = mock(async () =>
      mamPage([
        liveRoomMessage({
          id: "thread-message",
          threadId: "thread-1",
          body: "should not fetch after dispose",
          createdAt: "2026-01-01T00:03:00.000Z",
        }),
      ]));
    const { overview, scope } = createOverview({
      state,
      client: { queryMamPage, queryMamThreadPage } as TestOverviewClient,
    });

    await nextTick();
    await Promise.resolve();
    expect(queryMamPage).toHaveBeenCalledTimes(1);

    scope.stop();
    expect(overview.isLoading.value).toBe(false);
    staleChannelPage.resolve(mamPage([
      liveRoomMessage({
        id: "stale-channel-1",
        body: "stale unread message",
        createdAt: "2026-01-01T00:00:00.000Z",
      }),
    ]));
    await flushOverviewRefresh();

    expect(queryMamThreadPage).not.toHaveBeenCalled();
    expect(overview.groups.value).toEqual([]);
  });

  test("caps per-room thread MAM fan-out", async () => {
    const state = applyEntries(
      createInboxState(),
      Array.from({ length: 6 }, (_, index) =>
        muc({
          partner: ROOM,
          thread: `thread-${index + 1}`,
          threadTitle: `Thread ${index + 1}`,
          unread: 1,
          lastUpdated: 100 + index,
        })),
    );
    const queryMamPage = mock(async () => mamPage([]));
    const pending: {
      threadId: string;
      resolve: (page: MamHistoryPage<LiveRoomMessage>) => void;
    }[] = [];
    let inFlight = 0;
    let maxInFlight = 0;
    const queryMamThreadPage = mock((
      _spaceId: string,
      _channelId: string,
      threadId: string,
    ) => {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      const page = deferred<MamHistoryPage<LiveRoomMessage>>();
      pending.push({ threadId, resolve: page.resolve });
      return page.promise.finally(() => {
        inFlight -= 1;
      });
    });
    const { overview, scope } = createOverview({
      state,
      client: { queryMamPage, queryMamThreadPage } as TestOverviewClient,
    });

    await flushOverviewRefresh();
    expect(queryMamThreadPage).toHaveBeenCalledTimes(4);

    let guard = 0;
    while (queryMamThreadPage.mock.calls.length < 6 || pending.length > 0) {
      guard += 1;
      if (guard > 10) throw new Error("thread fan-out test did not settle");
      if (pending.length === 0) throw new Error("thread fan-out stalled before scheduling all work");
      const batch = pending.splice(0);
      for (const item of batch) {
        item.resolve(mamPage([
          liveRoomMessage({
            id: `${item.threadId}-message`,
            threadId: item.threadId,
            body: item.threadId,
            createdAt: "2026-01-01T00:00:00.000Z",
          }),
        ]));
      }
      await flushOverviewRefresh();
    }

    expect(maxInFlight).toBeLessThanOrEqual(4);
    expect(overview.groups.value[0]?.threads).toHaveLength(6);
    scope.stop();
  });
});
