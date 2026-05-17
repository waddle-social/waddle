// Bootstrap race: live messages that arrive between `messages.value =
// appendQueuedMessages([], …)` and the `messages.value = timelineWithQueue`
// wholesale assignment in `loadMessages` get silently overwritten by the
// MAM page. See issue #675 — distinct from #603/#607 which covered the
// suspend/resume live-merge path, not the initial-load bootstrap path.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelMamPaging } from "../src/channels/mam-paging";
import { useChannelLiveMerge } from "../src/channels/live-merge";
import { useDmMamPaging } from "../src/dms/mam-paging";
import { useDmLiveMerge } from "../src/dms/live-merge";
import type { BrowserXmppClient, LiveDmMessage, LiveRoomMessage } from "../src/lib/xmpp-client";
import type { ChannelSummary } from "../src/lib/chat-types";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";

const session: WaddleSession = {
  username: "alice",
  jid: "alice@example.com/desktop",
  session_id: "tok",
  xmpp_websocket_url: "wss://example.com/ws",
};

const channel: ChannelSummary = { id: "space_room", name: "Room" };

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function archivedRoomMessage(id: string, body: string, createdAt: string): LiveRoomMessage {
  return {
    id,
    type: "message",
    roomJid: "space_room@muc.example.com",
    fromJid: "space_room@muc.example.com/charlie",
    nick: "charlie",
    body,
    createdAt,
    isSelf: false,
  } as unknown as LiveRoomMessage;
}

function archivedDmMessage(id: string, body: string, createdAt: string, peerJid: string): LiveDmMessage {
  return {
    id,
    type: "message",
    peerJid,
    fromJid: peerJid,
    nick: peerJid.split("@")[0] ?? "peer",
    body,
    createdAt,
    isSelf: false,
  } as unknown as LiveDmMessage;
}

describe("useChannelMamPaging.loadMessages — bootstrap race (#675)", () => {
  test("preserves a live message that arrives during the queryMamPage await", async () => {
    const mam = deferred<{ messages: LiveRoomMessage[]; firstArchiveId?: string; complete?: boolean }>();
    const queryMamPage = mock(async (_space: string, _channel: string, _max: number, param: { type: string }) => {
      if (param.type !== "latest") throw new Error(`unexpected paging type ${param.type}`);
      return mam.promise;
    });
    const xmppClient = {
      queryMamPage,
      queryMam: mock(async () => []),
      fetchRoomPins: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const pendingEchoClientIds = new Set<string>();

    const liveMerge = useChannelLiveMerge({
      session: ref(session),
      messages,
      activeChannelId: ref("space_room"),
      pendingEchoClientIds,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });
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
      pinnedEdgeScroller: { disconnect: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      normalizeError: (e) => String(e),
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    // Start the bootstrap. Does not await — `queryMamPage` is deferred.
    const loadPromise = paging.loadMessages("space", "space_room");

    // Let the synchronous body of loadMessages run (reset messages,
    // send queryMamPage, hit its await).
    await Promise.resolve();
    await Promise.resolve();
    expect(messages.value).toEqual([]);

    // Live message arrives during the MAM await window.
    const live: LiveRoomMessage = {
      id: "live-race-1",
      type: "message",
      roomJid: "space_room@muc.example.com",
      fromJid: "space_room@muc.example.com/dana",
      nick: "dana",
      body: "i arrived during the race",
      createdAt: "2026-05-17T12:00:00.000Z",
      isSelf: false,
    } as unknown as LiveRoomMessage;
    liveMerge.handleRoomMessage(live);
    expect(messages.value.find((m) => m.id === "live-race-1")).toBeDefined();

    // MAM page lands. No alias overlaps with the live id.
    mam.resolve({
      messages: [
        archivedRoomMessage("old-1", "first", "2026-05-17T11:59:50.000Z"),
        archivedRoomMessage("old-2", "second", "2026-05-17T11:59:55.000Z"),
      ],
      firstArchiveId: "arch-1",
      complete: false,
    });
    await loadPromise;

    // Bug #675: the live message is silently overwritten by the MAM page.
    expect(
      messages.value.find((m) => m.id === "live-race-1"),
      "live message that arrived during bootstrap must survive the MAM page assignment",
    ).toBeDefined();
    // And it must sort chronologically after the older MAM rows.
    const ids = messages.value.map((m) => m.id);
    expect(ids).toEqual(["old-1", "old-2", "live-race-1"]);
  });
});

describe("useDmMamPaging.loadMessages — bootstrap race (#675)", () => {
  test("preserves a live DM that arrives during the queryPersonalMamPage await", async () => {
    const peerJid = "bob@example.com";
    const mam = deferred<{ messages: LiveDmMessage[]; firstArchiveId?: string; complete?: boolean }>();
    const queryPersonalMamPage = mock(async (_peerJid: string, _max: number, param: { type: string }) => {
      if (param.type !== "latest") throw new Error(`unexpected paging type ${param.type}`);
      return mam.promise;
    });
    const xmppClient = {
      queryPersonalMamPage,
      queryPersonalMam: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
    const pendingEchoClientIds = new Set<string>();

    const liveMerge = useDmLiveMerge({
      session: ref(session),
      messages,
      activePeerJid: ref(peerJid),
      pendingEchoClientIds,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
      isFeedVisible: () => true,
    });
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
      pinnedEdgeScroller: { disconnect: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      pendingEchoClientIds,
      appendQueuedMessages: (timeline) => timeline,
      scrollToPinnedEdgeAndPin: async () => true,
      isFeedVisible: () => true,
      persistLastSeen: () => {},
      dmLoadErrorMessage: () => "",
    });

    const loadPromise = paging.loadMessages(peerJid);

    await Promise.resolve();
    await Promise.resolve();
    expect(messages.value).toEqual([]);

    const live: LiveDmMessage = {
      id: "live-dm-race-1",
      type: "message",
      peerJid,
      fromJid: peerJid,
      nick: "bob",
      body: "dm during race",
      createdAt: "2026-05-17T12:00:00.000Z",
      isSelf: false,
    } as unknown as LiveDmMessage;
    liveMerge.handleIncomingMessage(live);
    expect(messages.value.find((m) => m.id === "live-dm-race-1")).toBeDefined();

    mam.resolve({
      messages: [
        archivedDmMessage("old-dm-1", "first", "2026-05-17T11:59:50.000Z", peerJid),
        archivedDmMessage("old-dm-2", "second", "2026-05-17T11:59:55.000Z", peerJid),
      ],
      firstArchiveId: "arch-dm-1",
      complete: false,
    });
    await loadPromise;

    expect(
      messages.value.find((m) => m.id === "live-dm-race-1"),
      "live DM that arrived during bootstrap must survive the MAM page assignment",
    ).toBeDefined();
    const ids = messages.value.map((m) => m.id);
    expect(ids).toEqual(["old-dm-1", "old-dm-2", "live-dm-race-1"]);
  });
});
