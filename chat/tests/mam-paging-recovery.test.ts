// Tests for XEP-0313 §4.3.4 stale-cursor recovery in the MAM paging
// composables. These exercise the control-flow path that detects an
// `item-not-found` response on a `{type:"before"}` page request, drops the
// stale cursor, resets loading flags, and re-fetches the tail page — for
// both channel and DM sides.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelMamPaging } from "../src/channels/mam-paging";
import { useDmMamPaging } from "../src/dms/mam-paging";
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

function makeRoomMessage(id: string, body: string): LiveRoomMessage {
  return {
    id,
    type: "message",
    roomJid: "space_room@muc.example.com",
    fromJid: "alice@example.com/desktop",
    nick: "alice",
    body,
    timestamp: Date.now(),
    isSelf: false,
  } as unknown as LiveRoomMessage;
}

function makeDmMessage(id: string, body: string, peerJid: string): LiveDmMessage {
  return {
    id,
    type: "message",
    peerJid,
    fromJid: "alice@example.com/desktop",
    nick: "alice",
    body,
    timestamp: Date.now(),
    isSelf: false,
  } as unknown as LiveDmMessage;
}

describe("useChannelMamPaging — XEP-0313 §4.3.4 recovery", () => {
  test("automatic route hydration does not query a room that still requires access", async () => {
    const queryMamPage = mock(async () => ({
      messages: [],
      firstArchiveId: null,
      complete: true,
    }));
    const fetchRoomPins = mock(async () => []);
    const paging = useChannelMamPaging({
      session: ref(session),
      xmppClient: ref({
        queryMamPage,
        fetchRoomPins,
      } as unknown as BrowserXmppClient),
      activeSpaceId: ref("space"),
      activeChannelId: ref("space_room"),
      currentChannel: ref(channel),
      messages: ref<TimelineMessage[]>([]),
      firstUnseenId: ref<string | null>(null),
      timelineEl: ref(null),
      scrollDirection: ref("bottom"),
      pinnedEdgeScroller: { cancelSettleLock: () => {} },
      actionError: ref(""),
      clearActionError: () => {},
      normalizeError: (e) => String(e),
      pendingEchoClientIds: new Set<string>(),
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      isRoomAccessRequired: () => true,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    await expect(
      paging.loadMessages("space", "space_room", 0, [], {
        allowAccessRetry: false,
      }),
    ).resolves.toBe("loaded");

    expect(queryMamPage).not.toHaveBeenCalled();
    expect(fetchRoomPins).not.toHaveBeenCalled();
    expect(paging.isLoadingMessages.value).toBe(false);
    expect(paging.hasOlderMessages.value).toBe(false);
  });

  test("loadOlderMessages: item-not-found on a before-cursor triggers cursor reset + tail-page refetch", async () => {
    const tailMessages = [makeRoomMessage("m1", "older"), makeRoomMessage("m2", "newer")];

    const queryMamPage = mock(async (_space: string, _channel: string, _max: number, param: { type: string }) => {
      if (param.type === "before") {
        // First before-cursor query returns the §4.3.4 condition.
        throw new Error("stanza error: item-not-found");
      }
      return { messages: tailMessages, firstArchiveId: "arch-1", complete: false };
    });

    const xmppClient = {
      queryMamPage,
      queryMam: mock(async () => tailMessages),
      fetchRoomPins: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
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
      pendingEchoClientIds: new Set<string>(),
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    // Initial load: tail page populates the cursor.
    await paging.loadMessages("space", "space_room");
    expect(paging.hasOlderMessages.value).toBe(true);
    paging.markInitialLatestPagePinned();

    // Trigger loadOlderMessages — the before-cursor query throws
    // item-not-found, which should reset the cursor, clear the loading
    // flag, and re-fetch the tail page silently.
    await paging.loadOlderMessages();

    // Recovery path is silent — loading flag must be false.
    expect(paging.isLoadingOlderMessages.value).toBe(false);

    // Both a `before` and a follow-up `latest` query happened.
    const calledWith = queryMamPage.mock.calls.map((c) => (c[3] as { type: string }).type);
    expect(calledWith).toContain("before");
    expect(calledWith.filter((t) => t === "latest").length).toBeGreaterThanOrEqual(2);
  });

  test("ensureMessageLoaded: item-not-found nulls cursor AND collapses hasOlderMessages", async () => {
    const tailMessages = [makeRoomMessage("m1", "older")];

    const queryMamPage = mock(async (_space: string, _channel: string, _max: number, param: { type: string }) => {
      if (param.type === "before") {
        throw new Error("stanza error: item-not-found");
      }
      return { messages: tailMessages, firstArchiveId: "arch-1", complete: false };
    });

    const xmppClient = {
      queryMamPage,
      queryMam: mock(async () => tailMessages),
      fetchRoomPins: mock(async () => []),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
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
      pendingEchoClientIds: new Set<string>(),
      appendQueuedMessages: (timeline) => timeline,
      roomJidForChannel: () => "space_room@muc.example.com",
      isRoomAccessRequired: () => false,
      scrollToPinnedEdgeAndPin: async () => true,
      persistLastSeen: () => {},
    });

    await paging.loadMessages("space", "space_room");
    expect(paging.hasOlderMessages.value).toBe(true);

    // Chase a message that isn't in the tail page — triggers a before-query
    // that returns item-not-found.
    const found = await paging.ensureMessageLoaded("missing-id");
    expect(found).toBe(false);
    // Cursor null is observable indirectly via hasOlderMessages collapsing
    // so the UI's "load older" sentinel doesn't keep prompting paging that
    // would early-return on !before.
    expect(paging.hasOlderMessages.value).toBe(false);
  });
});

describe("useDmMamPaging — XEP-0313 §4.3.4 recovery", () => {
  test("loadOlderMessages: item-not-found on a before-cursor triggers cursor reset + tail-page refetch", async () => {
    const peerJid = "bob@example.com";
    const tailMessages = [makeDmMessage("dm1", "older", peerJid), makeDmMessage("dm2", "newer", peerJid)];

    const queryPersonalMamPage = mock(async (_peerJid: string, _max: number, param: { type: string }) => {
      if (param.type === "before") {
        throw new Error("stanza error: item-not-found");
      }
      return { messages: tailMessages, firstArchiveId: "arch-1", complete: false };
    });

    const xmppClient = {
      queryPersonalMamPage,
      queryPersonalMam: mock(async () => tailMessages),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
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
      pendingEchoClientIds: new Set<string>(),
      appendQueuedMessages: (timeline) => timeline,
      scrollToPinnedEdgeAndPin: async () => true,
      isFeedVisible: (m) => !m.threadId || m.id === m.threadId,
      persistLastSeen: () => {},
      dmLoadErrorMessage: () => "load error",
    });

    await paging.loadMessages(peerJid);
    expect(paging.hasOlderMessages.value).toBe(true);
    paging.markInitialLatestPagePinned();

    await paging.loadOlderMessages();

    expect(paging.isLoadingOlderMessages.value).toBe(false);
    const calledWith = queryPersonalMamPage.mock.calls.map((c) => (c[2] as { type: string }).type);
    expect(calledWith).toContain("before");
    expect(calledWith.filter((t) => t === "latest").length).toBeGreaterThanOrEqual(2);
  });

  test("ensureMessageLoaded: item-not-found nulls cursor AND collapses hasOlderMessages", async () => {
    const peerJid = "bob@example.com";
    const tailMessages = [makeDmMessage("dm1", "older", peerJid)];

    const queryPersonalMamPage = mock(async (_peerJid: string, _max: number, param: { type: string }) => {
      if (param.type === "before") {
        throw new Error("stanza error: item-not-found");
      }
      return { messages: tailMessages, firstArchiveId: "arch-1", complete: false };
    });

    const xmppClient = {
      queryPersonalMamPage,
      queryPersonalMam: mock(async () => tailMessages),
    } as unknown as BrowserXmppClient;

    const messages = ref<TimelineMessage[]>([]);
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
      pendingEchoClientIds: new Set<string>(),
      appendQueuedMessages: (timeline) => timeline,
      scrollToPinnedEdgeAndPin: async () => true,
      isFeedVisible: (m) => !m.threadId || m.id === m.threadId,
      persistLastSeen: () => {},
      dmLoadErrorMessage: () => "load error",
    });

    await paging.loadMessages(peerJid);
    expect(paging.hasOlderMessages.value).toBe(true);

    const found = await paging.ensureMessageLoaded("missing-id");
    expect(found).toBe(false);
    expect(paging.hasOlderMessages.value).toBe(false);
  });
});
