import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { nextTick, ref } from "vue";
import { useDmMessaging } from "../src/composables/useDmMessaging";
import { useMessaging } from "../src/composables/useMessaging";
import { useScrollDirection } from "../src/composables/useScrollDirection";
import {
  getNewMessagesDividerPlacement,
  orderTimelineForScrollDirection,
  readStoredScrollDirection,
} from "../src/lib/scroll-direction";
import type { WaddleSession } from "../src/lib/server-auth";

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
    xmpp_websocket_url: "wss://example.com/xmpp",
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
    const sendGroupMessage = mock(async () => "client-1");
    const actionError = ref("");
    const messaging = useMessaging(
      ref(session()),
      ref(null),
      ref({
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
      ref({ queryMam } as never) as never,
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
