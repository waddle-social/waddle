import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { nextTick, ref } from "vue";
import { useDmMessaging } from "../src/composables/useDmMessaging";
import { useMessaging } from "../src/composables/useMessaging";
import { handlerStubs } from "./helpers/xmpp-client-mock";
import { useScrollDirection } from "../src/composables/useScrollDirection";
import {
  getNewMessagesDividerPlacement,
  orderTimelineForScrollDirection,
  readStoredScrollDirection,
} from "../src/lib/scroll-direction";
import { dmKey, roomKey, setLastSeen } from "../src/lib/last-seen-store";
import type { WaddleSession } from "../src/lib/server-auth";
import type { LiveRoomMessage } from "../src/lib/xmpp-client";

function createStorageMock() {
  const values = new Map<string, string>();
  return {
    getItem(key: string) {
      return values.has(key) ? values.get(key)! : null;
    },
    setItem(key: string, value: string) {
      values.set(key, value);
    },
    removeItem(key: string) {
      values.delete(key);
    },
    clear() {
      values.clear();
    },
  };
}

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

const originalWindow = globalThis.window;
const originalLocalStorage = globalThis.localStorage;

describe("scroll direction preference", () => {
  const { mode, setScrollDirection } = useScrollDirection();

  beforeEach(() => {
    const storage = createStorageMock();
    (globalThis as typeof globalThis & { localStorage: typeof storage }).localStorage = storage;
    (globalThis as typeof globalThis & { window: Window & { localStorage: typeof storage } }).window = {
      ...(originalWindow ?? {}),
      localStorage: storage,
    } as Window & { localStorage: typeof storage };
    localStorage.clear();
    setScrollDirection("chat");
  });

  afterEach(() => {
    localStorage.clear();
    setScrollDirection("chat");
    if (originalLocalStorage === undefined) {
      Reflect.deleteProperty(globalThis, "localStorage");
    } else {
      (globalThis as typeof globalThis & { localStorage: Storage }).localStorage = originalLocalStorage;
    }
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, "window");
    } else {
      (globalThis as typeof globalThis & { window: Window & typeof globalThis }).window = originalWindow;
    }
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
    const dm = useDmMessaging(
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const dm = useDmMessaging(
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
    const dm = useDmMessaging(
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
    const dm = useDmMessaging(
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
    const dm = useDmMessaging(
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
    const dm = useDmMessaging(
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
    const dm = useDmMessaging(
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
    const dm = useDmMessaging(
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
    const messaging = useMessaging(
      ref(session()),
      ref(null),
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
    const dm = useDmMessaging(
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
