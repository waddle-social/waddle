import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { installMockBrowserGlobals } from "./helpers/mock-browser-storage";
import { nextTick, ref } from "vue";
import { useDirectMessages } from "../src/dms/messages";
import { useChannelMessages } from "../src/channels/messages";
import { handlerStubs } from "./helpers/xmpp-client-mock";
import { useScrollDirectionPreference } from "../src/preferences/scroll-direction";
import {
  getNewMessagesDividerPlacement,
  orderTimelineForScrollDirection,
  readStoredScrollDirection,
} from "../src/lib/scroll-direction";
import {
  dmKey,
  mdsChatKey,
  queueMdsDisplayed,
  roomKey,
  setLastSeen,
  setMdsDisplayed,
} from "../src/lib/last-seen-store";
import type { WaddleSession } from "../src/lib/server-auth";
import type { LiveDmMessage, LiveRoomMessage } from "../src/lib/xmpp-client";


function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
    ...partial,
  } as WaddleSession;
}

function makeTimelineEl() {
  return {
    scrollHeight: 480,
    scrollTop: 240,
    children: [],
    querySelector: mock(() => null),
    querySelectorAll: mock(() => []),
  } as unknown as HTMLDivElement;
}

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((innerResolve) => {
    resolve = innerResolve;
  });
  return { promise, resolve };
}

async function flushAsync() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
}

describe("scroll direction preference", () => {
  const { mode, setScrollDirection } = useScrollDirectionPreference();

  installMockBrowserGlobals({
    beforeEachExtra: () => {
      setScrollDirection("chat");
    },
    afterEachExtra: () => {
      setScrollDirection("chat");
    },
  });

  test("defaults to chat mode and only persists non-default values", () => {
    expect(readStoredScrollDirection()).toBe("chat");
    expect(mode.value).toBe("chat");

    setScrollDirection("social");
    expect(mode.value).toBe("social");
    expect(localStorage.getItem("waddle:scroll-direction")).toBe("social");

    setScrollDirection("chat");
    expect(localStorage.getItem("waddle:scroll-direction")).toBeNull();
  });

  test("reorders timelines and flips divider placement for social mode", () => {
    expect(orderTimelineForScrollDirection(["one", "two", "three"], "chat")).toEqual([
      "one",
      "two",
      "three",
    ]);
    expect(orderTimelineForScrollDirection(["one", "two", "three"], "social")).toEqual([
      "three",
      "two",
      "one",
    ]);
    expect(getNewMessagesDividerPlacement("chat")).toBe("before");
    expect(getNewMessagesDividerPlacement("social")).toBe("after");
  });

  test("pins DM timelines to the top in social mode without reordering stored messages", async () => {
    setScrollDirection("social");
    const queryPersonalMam = mock(async () => [
      {
        id: "dm-1",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/phone",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "dm-2",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/laptop",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({ queryPersonalMam } as never) as never,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    const timelineEl = makeTimelineEl();
    dm.timelineEl.value = timelineEl;

    await dm.loadMessages("bob@example.com");

    expect(dm.messages.value.map((message) => message.id)).toEqual(["dm-1", "dm-2"]);
    expect(timelineEl.scrollTop).toBe(0);
  });

  test("pins room timelines to the top for optimistic sends in social mode", async () => {
    setScrollDirection("social");
    const sendChatState = mock(async () => undefined);
    const sendGroupMessage = mock(async () => ({ id: "client-1", state: "sending" as const }));
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({
        ...handlerStubs(),
        sendChatState,
        sendGroupMessage,
      } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    const timelineEl = makeTimelineEl();
    messaging.timelineEl.value = timelineEl;

    await messaging.sendMessage("hello world");
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
    await nextTick();

    expect(timelineEl.scrollTop).toBe(0);
    expect(messaging.messages.value.at(-1)?.body).toBe("hello world");
  });

  test("pins room timelines to the bottom for optimistic sends in chat mode", async () => {
    const sendChatState = mock(async () => undefined);
    const sendGroupMessage = mock(async () => ({ id: "client-1", state: "sending" as const }));
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({
        ...handlerStubs(),
        sendChatState,
        sendGroupMessage,
      } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    const timelineEl = makeTimelineEl();
    messaging.timelineEl.value = timelineEl;

    await messaging.sendMessage("hello world");
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
    await nextTick();

    expect(timelineEl.scrollTop).toBe(480);
    expect(messaging.messages.value.at(-1)?.body).toBe("hello world");
  });

  test("pins room timelines to the top in social mode without reordering stored messages", async () => {
    setScrollDirection("social");
    const queryMam = mock(async () => [
      {
        id: "room-1",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "room-2",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), queryMam } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    const timelineEl = makeTimelineEl();
    messaging.timelineEl.value = timelineEl;

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value.map((message) => message.id)).toEqual(["room-1", "room-2"]);
    expect(timelineEl.scrollTop).toBe(0);
  });

  test("room initial load ignores older last-seen and persists the newest visible message", async () => {
    setLastSeen(roomKey("c1"), "room-1");
    const queryMam = mock(async () => [
      {
        id: "room-1",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "room-2",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), queryMam } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    const timelineEl = makeTimelineEl();
    messaging.timelineEl.value = timelineEl;

    await messaging.loadMessages("w1", "c1");

    expect(messaging.firstUnseenId.value).toBeNull();
    expect(timelineEl.scrollTop).toBe(480);
    expect(localStorage.getItem(roomKey("c1"))).toBe("room-2");
  });

  test("room initial load advances first unseen from stored MDS displayed state", async () => {
    setMdsDisplayed(mdsChatKey("w1-c1@rooms.example.com"), {
      stanzaId: "room-stanza-1",
      stanzaIdBy: "w1-c1@rooms.example.com",
    });
    const queryMam = mock(async () => [
      {
        id: "room-1",
        stanzaId: "room-stanza-1",
        stanzaIdBy: "w1-c1@rooms.example.com",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "room-2",
        stanzaId: "room-stanza-2",
        stanzaIdBy: "w1-c1@rooms.example.com",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), queryMam } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", jid: "w1-c1@rooms.example.com", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    messaging.timelineEl.value = makeTimelineEl();

    await messaging.loadMessages("w1", "c1", 2);

    expect(messaging.firstUnseenId.value).toBe("room-2");
    expect(messaging.applyMdsDisplayed("w1-c1@rooms.example.com/alice", {
      stanzaId: "room-stanza-2",
      stanzaIdBy: "w1-c1@rooms.example.com",
    })).toBe(false);
  });

  test("room initial load re-pins when the virtual timeline ref appears after messages", async () => {
    const queryMam = mock(async () => [
      {
        id: "room-1",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "room-2",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const virtualScroll = mock(async () => true);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), queryMam } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");
    expect(virtualScroll).not.toHaveBeenCalled();
    expect(localStorage.getItem(roomKey("c1"))).toBeNull();

    messaging.timelineEl.value = makeTimelineEl();
    messaging.timelineEdgeScroller.value = virtualScroll;
    await nextTick();
    await nextTick();
    await nextTick();
    await nextTick();
    await flushAsync();

    expect(virtualScroll).toHaveBeenCalledWith("chat");
    expect(localStorage.getItem(roomKey("c1"))).toBe("room-2");
  });

  test("room initial load waits for a scroll target before persisting last-seen or older loading", async () => {
    const queryMamPage = mock(async (_spaceId, _channelId, _max, pageParam) => {
      if (pageParam.type === "before") {
        return {
          messages: [
            {
              id: "room-0",
              roomJid: "w1-c1@rooms.example.com",
              nick: "bob",
              body: "oldest",
              createdAt: "2023-12-31T23:59:50Z",
              type: "message" as const,
            },
          ],
          firstArchiveId: "room-0",
          complete: true,
        };
      }
      return {
        messages: [
          {
            id: "room-1",
            roomJid: "w1-c1@rooms.example.com",
            nick: "bob",
            body: "earlier",
            createdAt: "2024-01-01T00:00:00Z",
            type: "message" as const,
          },
          {
            id: "room-2",
            roomJid: "w1-c1@rooms.example.com",
            nick: "bob",
            body: "later",
            createdAt: "2024-01-01T00:00:10Z",
            type: "message" as const,
          },
        ],
        firstArchiveId: "room-1",
        complete: false,
      };
    });
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), queryMamPage } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");
    await messaging.loadOlderMessages();

    expect(localStorage.getItem(roomKey("c1"))).toBeNull();
    expect(queryMamPage).toHaveBeenCalledTimes(1);
  });

  test("room initial load does not persist last-seen after the active channel changes during edge pin", async () => {
    const activeChannelId = ref("c1");
    const pin = deferred();
    const queryMam = mock(async () => [
      {
        id: "room-1",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "room-2",
        roomJid: "w1-c1@rooms.example.com",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const virtualScroll = mock(async () => {
      await pin.promise;
      return true;
    });
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), queryMam } as never) as never,
      ref("w1"),
      activeChannelId,
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    messaging.timelineEl.value = makeTimelineEl();
    messaging.timelineEdgeScroller.value = virtualScroll;

    const load = messaging.loadMessages("w1", "c1");
    await flushAsync();
    expect(virtualScroll).toHaveBeenCalledWith("chat");

    activeChannelId.value = "c2";
    pin.resolve();
    await load;

    expect(localStorage.getItem(roomKey("c1"))).toBeNull();
  });

  test("room older-history loads are blocked until the latest page is pinned", async () => {
    const pin = deferred();
    const queryMamPage = mock(async (_spaceId, _channelId, _max, pageParam) => {
      if (pageParam.type === "before") {
        return {
          messages: [
            {
              id: "room-0",
              roomJid: "w1-c1@rooms.example.com",
              nick: "bob",
              body: "oldest",
              createdAt: "2023-12-31T23:59:50Z",
              type: "message" as const,
            },
          ],
          firstArchiveId: "room-0",
          complete: true,
        };
      }
      return {
        messages: [
          {
            id: "room-1",
            roomJid: "w1-c1@rooms.example.com",
            nick: "bob",
            body: "earlier",
            createdAt: "2024-01-01T00:00:00Z",
            type: "message" as const,
          },
          {
            id: "room-2",
            roomJid: "w1-c1@rooms.example.com",
            nick: "bob",
            body: "later",
            createdAt: "2024-01-01T00:00:10Z",
            type: "message" as const,
          },
        ],
        firstArchiveId: "room-1",
        complete: false,
      };
    });
    const virtualScroll = mock(async () => {
      await pin.promise;
      return true;
    });
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), queryMamPage } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    messaging.timelineEl.value = makeTimelineEl();
    messaging.timelineEdgeScroller.value = virtualScroll;

    const load = messaging.loadMessages("w1", "c1");
    await flushAsync();
    await messaging.loadOlderMessages();
    expect(queryMamPage).toHaveBeenCalledTimes(1);

    pin.resolve();
    await load;
    await messaging.loadOlderMessages();

    expect(queryMamPage).toHaveBeenCalledTimes(2);
    expect(messaging.messages.value.map((message) => message.id)).toContain("room-0");
  });

  test("room clear blocks older-history loads until a fresh latest page is pinned", async () => {
    const queryMamPage = mock(async (_spaceId, _channelId, _max, pageParam) => {
      if (pageParam.type === "before") {
        return {
          messages: [
            {
              id: "room-0",
              roomJid: "w1-c1@rooms.example.com",
              nick: "bob",
              body: "oldest",
              createdAt: "2023-12-31T23:59:50Z",
              type: "message" as const,
            },
          ],
          firstArchiveId: "room-0",
          complete: true,
        };
      }
      return {
        messages: [
          {
            id: "room-1",
            roomJid: "w1-c1@rooms.example.com",
            nick: "bob",
            body: "earlier",
            createdAt: "2024-01-01T00:00:00Z",
            type: "message" as const,
          },
          {
            id: "room-2",
            roomJid: "w1-c1@rooms.example.com",
            nick: "bob",
            body: "later",
            createdAt: "2024-01-01T00:00:10Z",
            type: "message" as const,
          },
        ],
        firstArchiveId: "room-1",
        complete: false,
      };
    });
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({ ...handlerStubs(), queryMamPage } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    messaging.timelineEl.value = makeTimelineEl();

    await messaging.loadMessages("w1", "c1");
    await messaging.loadOlderMessages();
    expect(queryMamPage).toHaveBeenCalledTimes(2);

    messaging.clearMessages();
    await messaging.loadOlderMessages();
    expect(queryMamPage).toHaveBeenCalledTimes(2);
  });

  test("DM initial load ignores older last-seen and persists the newest visible message", async () => {
    setLastSeen(dmKey("bob@example.com"), "dm-1");
    const queryPersonalMam = mock(async () => [
      {
        id: "dm-1",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/phone",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "dm-2",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/laptop",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({ queryPersonalMam } as never) as never,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    const timelineEl = makeTimelineEl();
    dm.timelineEl.value = timelineEl;

    await dm.loadMessages("bob@example.com");

    expect(dm.firstUnseenId.value).toBeNull();
    expect(timelineEl.scrollTop).toBe(480);
    expect(localStorage.getItem(dmKey("bob@example.com"))).toBe("dm-2");
  });

  test("DM initial load advances first unseen from stored MDS displayed state", async () => {
    setMdsDisplayed(mdsChatKey("bob@example.com"), {
      stanzaId: "dm-stanza-1",
      stanzaIdBy: "example.com",
    });
    const queryPersonalMam = mock(async () => [
      {
        id: "dm-1",
        stanzaId: "dm-stanza-1",
        stanzaIdBy: "example.com",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/phone",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "dm-2",
        stanzaId: "dm-stanza-2",
        stanzaIdBy: "example.com",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/laptop",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({ queryPersonalMam } as never) as never,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    dm.timelineEl.value = makeTimelineEl();

    await dm.loadMessages("bob@example.com", 2);

    expect(dm.firstUnseenId.value).toBe("dm-2");
  });

  test("MUC-PM paging and live MDS isolate two occupants in the same room", async () => {
    const alice = "room@conference.example/alice";
    const bob = "room@conference.example/bob";
    setMdsDisplayed(mdsChatKey(alice), {
      stanzaId: "alice-stanza-1",
      stanzaIdBy: "example.com",
    });
    setMdsDisplayed(mdsChatKey(bob), {
      stanzaId: "bob-stanza-9",
      stanzaIdBy: "example.com",
    });
    const queryPersonalMam = mock(async () => [
      {
        id: "alice-1",
        stanzaId: "alice-stanza-1",
        stanzaIdBy: "example.com",
        peerJid: alice,
        fromJid: alice,
        nick: "alice",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
        mucPm: true,
      },
      {
        id: "alice-2",
        stanzaId: "alice-stanza-2",
        stanzaIdBy: "example.com",
        peerJid: alice,
        fromJid: alice,
        nick: "alice",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
        mucPm: true,
      },
    ]);
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({
        queryPersonalMam,
        isMucPmPeer: (peerJid: string) => peerJid.startsWith("room@conference.example/"),
      } as never) as never,
      ref(alice),
      String,
      actionError,
      () => { actionError.value = ""; },
    );
    dm.timelineEl.value = makeTimelineEl();

    await dm.loadMessages(alice, 2);

    expect(dm.firstUnseenId.value).toBe("alice-2");
    expect(localStorage.getItem(dmKey(alice))).toBe("alice-2");
    expect(localStorage.getItem(dmKey(bob))).toBeNull();
    expect(dm.applyMdsDisplayed(bob, {
      stanzaId: "bob-stanza-9",
      stanzaIdBy: "example.com",
    })).toBe(false);
    expect(dm.applyMdsDisplayed(alice, {
      stanzaId: "alice-stanza-2",
      stanzaIdBy: "example.com",
    })).toBe(true);
    expect(dm.firstUnseenId.value).toBeNull();
  });

  test("MUC-PM paging keeps request-start identity when the client clears during await", async () => {
    const alice = "room@rooms.custom.example/alice";
    setMdsDisplayed(mdsChatKey(alice), {
      stanzaId: "alice-stanza-1",
      stanzaIdBy: "example.com",
    });
    let resolvePage!: (messages: LiveDmMessage[]) => void;
    const queryPersonalMam = mock(async () => new Promise<LiveDmMessage[]>((resolve) => {
      resolvePage = resolve;
    }));
    const client = {
      queryPersonalMam,
      isMucPmPeer: (peerJid: string) => peerJid === alice,
    } as never;
    const clientRef = ref(client);
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      clientRef,
      ref(alice),
      String,
      actionError,
      () => { actionError.value = ""; },
    );
    dm.timelineEl.value = makeTimelineEl();

    const load = dm.loadMessages(alice, 2);
    await Promise.resolve();
    clientRef.value = null as never;
    resolvePage([
      {
        id: "alice-1",
        stanzaId: "alice-stanza-1",
        stanzaIdBy: "example.com",
        peerJid: alice,
        fromJid: alice,
        nick: "alice",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message",
        mucPm: true,
      },
      {
        id: "alice-2",
        stanzaId: "alice-stanza-2",
        stanzaIdBy: "example.com",
        peerJid: alice,
        fromJid: alice,
        nick: "alice",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message",
        mucPm: true,
      },
    ]);
    await load;

    expect(dm.firstUnseenId.value).toBe("alice-2");
    expect(localStorage.getItem(dmKey(alice))).toBe("alice-2");
  });

  test("typed inbound MDS isolates an unknown occupant before custom-service discovery", async () => {
    const alice = "room@rooms.custom.example/alice";
    const bob = "room@rooms.custom.example/bob";
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({
        queryPersonalMam: mock(async () => [{
          id: "bob-1",
          stanzaId: "shared-stanza-id",
          stanzaIdBy: "example.com",
          peerJid: bob,
          fromJid: bob,
          nick: "bob",
          body: "hello",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message" as const,
          mucPm: true,
        }]),
        // Reproduces the pre-discovery window: the custom MUC service is
        // not known locally yet, but the inbound item ID is already typed.
        isMucPmPeer: () => false,
      } as never) as never,
      ref(bob),
      String,
      actionError,
      () => { actionError.value = ""; },
    );
    dm.timelineEl.value = makeTimelineEl();
    await dm.loadMessages(bob, 1);

    expect(dm.applyMdsDisplayed(alice, {
      stanzaId: "shared-stanza-id",
      stanzaIdBy: "example.com",
    })).toBe(false);
    expect(dm.applyMdsDisplayed(bob, {
      stanzaId: "shared-stanza-id",
      stanzaIdBy: "example.com",
    })).toBe(true);
  });

  test("restored occupant scope reconciles the full inbound MDS key before discovery", async () => {
    const alice = "room@rooms.custom.example/alice";
    setMdsDisplayed(mdsChatKey(alice), {
      stanzaId: "alice-restored-sid",
      stanzaIdBy: "example.com",
    });
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({
        queryPersonalMam: mock(async () => [{
          id: "alice-restored",
          stanzaId: "alice-restored-sid",
          stanzaIdBy: "example.com",
          peerJid: alice,
          fromJid: alice,
          nick: "alice",
          body: "restored",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message" as const,
          mucPm: true,
        }]),
        isMucPmPeer: () => false,
      } as never) as never,
      ref(alice),
      String,
      actionError,
      () => { actionError.value = ""; },
      ref("muc-occupant"),
    );
    dm.timelineEl.value = makeTimelineEl();

    await dm.loadMessages(alice, 1);

    expect(dm.firstUnseenId.value).toBeNull();
    expect(localStorage.getItem(dmKey(alice))).toBe("alice-restored");
    expect(localStorage.getItem(dmKey("room@rooms.custom.example"))).toBeNull();
  });

  test("DM initial load reconciles queued inactive MDS candidates without moving backward", async () => {
    const key = mdsChatKey("bob@example.com");
    setMdsDisplayed(key, {
      stanzaId: "dm-stanza-1",
      stanzaIdBy: "example.com",
    });
    queueMdsDisplayed(key, {
      stanzaId: "dm-stanza-2",
      stanzaIdBy: "example.com",
    });
    const queryPersonalMam = mock(async () => [
      {
        id: "dm-1",
        stanzaId: "dm-stanza-1",
        stanzaIdBy: "example.com",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/phone",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "dm-2",
        stanzaId: "dm-stanza-2",
        stanzaIdBy: "example.com",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/laptop",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({ queryPersonalMam } as never) as never,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    dm.timelineEl.value = makeTimelineEl();

    await dm.loadMessages("bob@example.com", 2);

    expect(dm.firstUnseenId.value).toBeNull();
  });

  test("DM initial load re-pins when the virtual timeline ref appears after messages", async () => {
    const queryPersonalMam = mock(async () => [
      {
        id: "dm-1",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/phone",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "dm-2",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/laptop",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const virtualScroll = mock(async () => true);
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({ queryPersonalMam } as never) as never,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await dm.loadMessages("bob@example.com");
    expect(virtualScroll).not.toHaveBeenCalled();
    expect(localStorage.getItem(dmKey("bob@example.com"))).toBeNull();

    dm.timelineEl.value = makeTimelineEl();
    dm.timelineEdgeScroller.value = virtualScroll;
    await nextTick();
    await nextTick();
    await nextTick();
    await nextTick();
    await flushAsync();

    expect(virtualScroll).toHaveBeenCalledWith("chat");
    expect(localStorage.getItem(dmKey("bob@example.com"))).toBe("dm-2");
  });

  test("DM initial load waits for a scroll target before persisting last-seen or older loading", async () => {
    const queryPersonalMamPage = mock(async (_peerJid, _max, pageParam) => {
      if (pageParam.type === "before") {
        return {
          messages: [
            {
              id: "dm-0",
              peerJid: "bob@example.com",
              fromJid: "bob@example.com/tablet",
              nick: "bob",
              body: "oldest",
              createdAt: "2023-12-31T23:59:50Z",
              type: "message" as const,
            },
          ],
          firstArchiveId: "dm-0",
          complete: true,
        };
      }
      return {
        messages: [
          {
            id: "dm-1",
            peerJid: "bob@example.com",
            fromJid: "bob@example.com/phone",
            nick: "bob",
            body: "earlier",
            createdAt: "2024-01-01T00:00:00Z",
            type: "message" as const,
          },
          {
            id: "dm-2",
            peerJid: "bob@example.com",
            fromJid: "bob@example.com/laptop",
            nick: "bob",
            body: "later",
            createdAt: "2024-01-01T00:00:10Z",
            type: "message" as const,
          },
        ],
        firstArchiveId: "dm-1",
        complete: false,
      };
    });
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({ queryPersonalMamPage } as never) as never,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await dm.loadMessages("bob@example.com");
    await dm.loadOlderMessages();

    expect(localStorage.getItem(dmKey("bob@example.com"))).toBeNull();
    expect(queryPersonalMamPage).toHaveBeenCalledTimes(1);
  });

  test("DM initial load does not persist last-seen after the active peer changes during edge pin", async () => {
    const activePeerJid = ref("bob@example.com");
    const pin = deferred();
    const queryPersonalMam = mock(async () => [
      {
        id: "dm-1",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/phone",
        nick: "bob",
        body: "earlier",
        createdAt: "2024-01-01T00:00:00Z",
        type: "message" as const,
      },
      {
        id: "dm-2",
        peerJid: "bob@example.com",
        fromJid: "bob@example.com/laptop",
        nick: "bob",
        body: "later",
        createdAt: "2024-01-01T00:00:10Z",
        type: "message" as const,
      },
    ]);
    const virtualScroll = mock(async () => {
      await pin.promise;
      return true;
    });
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({ queryPersonalMam } as never) as never,
      activePeerJid,
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    dm.timelineEl.value = makeTimelineEl();
    dm.timelineEdgeScroller.value = virtualScroll;

    const load = dm.loadMessages("bob@example.com");
    await flushAsync();
    expect(virtualScroll).toHaveBeenCalledWith("chat");

    activePeerJid.value = "carol@example.com";
    pin.resolve();
    await load;

    expect(localStorage.getItem(dmKey("bob@example.com"))).toBeNull();
  });

  test("DM older-history loads are blocked until the latest page is pinned", async () => {
    const pin = deferred();
    const queryPersonalMamPage = mock(async (_peerJid, _max, pageParam) => {
      if (pageParam.type === "before") {
        return {
          messages: [
            {
              id: "dm-0",
              peerJid: "bob@example.com",
              fromJid: "bob@example.com/tablet",
              nick: "bob",
              body: "oldest",
              createdAt: "2023-12-31T23:59:50Z",
              type: "message" as const,
            },
          ],
          firstArchiveId: "dm-0",
          complete: true,
        };
      }
      return {
        messages: [
          {
            id: "dm-1",
            peerJid: "bob@example.com",
            fromJid: "bob@example.com/phone",
            nick: "bob",
            body: "earlier",
            createdAt: "2024-01-01T00:00:00Z",
            type: "message" as const,
          },
          {
            id: "dm-2",
            peerJid: "bob@example.com",
            fromJid: "bob@example.com/laptop",
            nick: "bob",
            body: "later",
            createdAt: "2024-01-01T00:00:10Z",
            type: "message" as const,
          },
        ],
        firstArchiveId: "dm-1",
        complete: false,
      };
    });
    const virtualScroll = mock(async () => {
      await pin.promise;
      return true;
    });
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({ queryPersonalMamPage } as never) as never,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    dm.timelineEl.value = makeTimelineEl();
    dm.timelineEdgeScroller.value = virtualScroll;

    const load = dm.loadMessages("bob@example.com");
    await flushAsync();
    await dm.loadOlderMessages();
    expect(queryPersonalMamPage).toHaveBeenCalledTimes(1);

    pin.resolve();
    await load;
    await dm.loadOlderMessages();

    expect(queryPersonalMamPage).toHaveBeenCalledTimes(2);
    expect(dm.messages.value.map((message) => message.id)).toContain("dm-0");
  });

  test("DM clear blocks older-history loads until a fresh latest page is pinned", async () => {
    const queryPersonalMamPage = mock(async (_peerJid, _max, pageParam) => {
      if (pageParam.type === "before") {
        return {
          messages: [
            {
              id: "dm-0",
              peerJid: "bob@example.com",
              fromJid: "bob@example.com/tablet",
              nick: "bob",
              body: "oldest",
              createdAt: "2023-12-31T23:59:50Z",
              type: "message" as const,
            },
          ],
          firstArchiveId: "dm-0",
          complete: true,
        };
      }
      return {
        messages: [
          {
            id: "dm-1",
            peerJid: "bob@example.com",
            fromJid: "bob@example.com/phone",
            nick: "bob",
            body: "earlier",
            createdAt: "2024-01-01T00:00:00Z",
            type: "message" as const,
          },
          {
            id: "dm-2",
            peerJid: "bob@example.com",
            fromJid: "bob@example.com/laptop",
            nick: "bob",
            body: "later",
            createdAt: "2024-01-01T00:00:10Z",
            type: "message" as const,
          },
        ],
        firstArchiveId: "dm-1",
        complete: false,
      };
    });
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref({ queryPersonalMamPage } as never) as never,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    dm.timelineEl.value = makeTimelineEl();

    await dm.loadMessages("bob@example.com");
    await dm.loadOlderMessages();
    expect(queryPersonalMamPage).toHaveBeenCalledTimes(2);

    dm.clearMessages();
    await dm.loadOlderMessages();
    expect(queryPersonalMamPage).toHaveBeenCalledTimes(2);
  });

  test("live DM messages pin to the active edge in chat and social modes", async () => {
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref(null),
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    const timelineEl = makeTimelineEl();
    dm.timelineEl.value = timelineEl;

    dm.onIncomingMessage({
      id: "dm-live-chat",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/phone",
      nick: "bob",
      body: "chat edge",
      createdAt: "2024-01-01T00:00:00Z",
      type: "message",
    });
    await nextTick();
    await nextTick();
    expect(timelineEl.scrollTop).toBe(480);

    setScrollDirection("social");
    timelineEl.scrollTop = 240;
    dm.onIncomingMessage({
      id: "dm-live-social",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/phone",
      nick: "bob",
      body: "social edge",
      createdAt: "2024-01-01T00:00:10Z",
      type: "message",
    });
    await nextTick();
    await nextTick();
    expect(timelineEl.scrollTop).toBe(0);
  });

  test("live room messages from discovered MUC JIDs pin through the virtual timeline edge scroller", async () => {
    let liveHandler: ((msg: LiveRoomMessage) => void) | null = null;
    const virtualScroll = mock(async () => true);
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref({
        ...handlerStubs(),
        setMessageHandler: (handler: (msg: LiveRoomMessage) => void) => {
          liveHandler = handler;
        },
      } as never) as never,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", jid: "c1@conference.example.net", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    const timelineEl = makeTimelineEl();
    messaging.timelineEl.value = timelineEl;
    messaging.timelineEdgeScroller.value = virtualScroll;

    liveHandler?.({
      id: "room-live",
      roomJid: "c1@conference.example.net",
      nick: "bob",
      body: "from room",
      createdAt: "2024-01-01T00:00:00Z",
      type: "message",
    });
    await nextTick();
    await nextTick();

    expect(virtualScroll).toHaveBeenCalledWith("chat");
    expect(messaging.messages.value.at(-1)?.id).toBe("room-live");
    expect(timelineEl.scrollTop).toBe(240);
  });

  test("re-pins an open DM timeline when the preference changes", async () => {
    const actionError = ref("");
    const dm = useDirectMessages(
      ref(session()),
      ref(null),
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    const timelineEl = makeTimelineEl();
    dm.timelineEl.value = timelineEl;

    setScrollDirection("social");
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
    await nextTick();

    expect(timelineEl.scrollTop).toBe(0);
  });
});
