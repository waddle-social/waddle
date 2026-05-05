import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { EventEmitter } from "events";
import { ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
import { useDmMessaging } from "../src/composables/useDmMessaging";
import { useMessaging } from "../src/composables/useMessaging";
import { BrowserXmppClient, roomBareJidFor, type InboxEntry } from "../src/lib/xmpp-client";
import { listQueuedDmMessages, listQueuedRoomMessages } from "../src/lib/outbound-queue-store";
import { handlerStubs } from "./helpers/xmpp-client-mock";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
    ...partial,
  } as WaddleSession;
}

function normalizeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

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

const originalWindow = globalThis.window;
const originalLocalStorage = globalThis.localStorage;

beforeEach(() => {
  const storage = createStorageMock();
  (globalThis as typeof globalThis & { localStorage: typeof storage }).localStorage = storage;
  (globalThis as typeof globalThis & { window: Window & { localStorage: typeof storage } }).window = {
    ...(originalWindow ?? {}),
    localStorage: storage,
  } as Window & { localStorage: typeof storage };
  localStorage.clear();
});

afterEach(() => {
  localStorage.clear();
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

describe("client send readiness", () => {
  test("room sends immediately when the room is ready", async () => {
    const xmpp = { sendMessage: mock(() => undefined) };
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    // Pre-set the ready state: sendGroupMessage takes the fast path
    // (no awaits, no switchRoom call) when `roomIsReady` is already
    // true at call time. Anything else enqueues optimistically and
    // drives recovery in the background — that's covered by the next
    // test.
    (client as unknown as {
      xmpp: typeof xmpp;
      connected: boolean;
      currentRoom: string | null;
    }).xmpp = xmpp;
    (client as unknown as {
      xmpp: typeof xmpp;
      connected: boolean;
      currentRoom: string | null;
    }).connected = true;
    (client as unknown as {
      xmpp: typeof xmpp;
      connected: boolean;
      currentRoom: string | null;
    }).currentRoom = roomJid;

    const result = await client.sendGroupMessage("w1", "c1", "hello room");

    expect(typeof result?.id).toBe("string");
    expect(result?.state).toBe("sending");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
  });

  test("XEP-0201: BrowserXmppClient.sendGroupMessage accepts bodyless thread metadata sends", async () => {
    // XEP-0201 thread create / thread reply payloads are bodyless. The
    // browser client wrapper must not short-circuit them; otherwise standard
    // MUC threads can never leave the browser through the regular send path.
    const xmpp = { sendMessage: mock(() => undefined) };
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    (client as unknown as {
      xmpp: typeof xmpp;
      connected: boolean;
      currentRoom: string | null;
    }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;

    const result = await client.sendGroupMessage("w1", "c1", "", {
      threadId: "thread-root",
    });

    expect(result?.state).toBe("sending");
    expect(typeof result?.id).toBe("string");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({ thread: "thread-root", body: "" }),
    );
  });

  test("room send queues when the room is unavailable", async () => {
    const xmpp = { sendMessage: mock(() => undefined) };
    const client = new BrowserXmppClient(session());
    (client as unknown as { switchRoom: ReturnType<typeof mock> }).switchRoom = mock(async () => {
      throw new Error("Reconnection timed out");
    });
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => {
      throw new Error("Reconnection timed out");
    });

    const result = await client.sendGroupMessage("w1", "c1", "hello room");

    expect(result?.state).toBe("queued");
    expect(listQueuedRoomMessages("alice@example.com", roomBareJidFor(session(), "c1"))).toHaveLength(1);
    await expect(client.sendReaction("w1", "c1", "msg-1", ["👍"])).rejects.toThrow("Reconnection timed out");
    await expect(client.sendDisplayed("w1", "c1", "msg-1")).rejects.toThrow("Reconnection timed out");
    await expect(client.sendChatState("w1", "c1", "composing")).rejects.toThrow("Reconnection timed out");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(0);
  });

  test("DM send queues when the session is unavailable", async () => {
    const xmpp = { sendMessage: mock(() => undefined) };
    const client = new BrowserXmppClient(session());
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => {
      throw new Error("Reconnection timed out");
    });

    const result = await client.sendDirectMessage("bob@example.com", "hello");

    expect(result?.state).toBe("queued");
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com")).toHaveLength(1);
    await expect(client.sendDmReaction("bob@example.com", "msg-1", ["👍"])).rejects.toThrow(
      "Reconnection timed out",
    );
    await expect(client.sendDmDisplayed("bob@example.com", "msg-1")).rejects.toThrow("Reconnection timed out");
    await expect(client.sendDmChatState("bob@example.com", "composing")).rejects.toThrow(
      "Reconnection timed out",
    );
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(0);
  });
});

describe("client keepalive lifecycle", () => {
  test("uses stanza keepalive and disables it when the transport disconnects", () => {
    const originalConsoleError = console.error;
    console.error = mock(() => undefined) as typeof console.error;
    try {
      const client = new BrowserXmppClient(session());
      const xmpp = Object.assign(new EventEmitter(), {
        enableKeepAlive: mock((_opts: { interval: number; timeout: number }) => undefined),
        disableKeepAlive: mock(() => undefined),
        getTime: mock(async () => ({ utc: new Date("2024-01-01T00:00:00Z") })),
        enableCarbons: mock(async () => undefined),
        getRoster: mock(async () => ({ items: [] })),
      }) as unknown as Agent;
      (client as unknown as { xmpp: Agent }).xmpp = xmpp;
      (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

      xmpp.emit("session:started");

      expect(xmpp.disableKeepAlive).toHaveBeenCalledTimes(1);
      expect(xmpp.enableKeepAlive).toHaveBeenCalledWith({ interval: 30, timeout: 15 });

      xmpp.emit("disconnected");

      expect(xmpp.disableKeepAlive).toHaveBeenCalledTimes(2);
    } finally {
      console.error = originalConsoleError;
    }
  });

  test("runs reconnect catch-up on stream-management resume", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as { catchup: { recordDmSeen: (peer: string, ts: string) => void; onSessionStarted: () => unknown[] } }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z");
    // Prime initial login marker so the next reconnect path performs catch-up.
    catchup.onSessionStarted();
    const searchHistory = mock(async () => ({ results: [] }));

    const xmpp = Object.assign(new EventEmitter(), {
      enableKeepAlive: mock((_opts: { interval: number; timeout: number }) => undefined),
      disableKeepAlive: mock(() => undefined),
      getTime: mock(async () => ({ utc: new Date("2024-01-01T00:00:00Z") })),
      searchHistory,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await Promise.resolve();
    await Promise.resolve();

    expect(searchHistory).toHaveBeenCalledTimes(1);
    expect(searchHistory).toHaveBeenCalledWith(
      "alice@example.com",
      expect.objectContaining({
        paging: { max: 200 },
      }),
    );
  });
});

describe("carbon forwarding", () => {
  test("ignores carbon-wrapped payloads on generic message event", () => {
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("message", {
      type: "chat",
      from: "alice@example.com",
      to: "bob@example.com",
      body: "wrapper should be ignored here",
      carbon: { sent: true },
    });

    expect(dmHandler).toHaveBeenCalledTimes(0);
  });

  test("forwards carbon:sent messages to the DM handler", () => {
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("carbon:sent", {
      carbon: {
        forward: {
          message: {
            id: "c-sent-1",
            type: "chat",
            from: "alice@example.com/phone",
            to: "bob@example.com/desktop",
            body: "hello from sibling sender",
          },
        },
      },
    });

    expect(dmHandler).toHaveBeenCalledTimes(1);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "c-sent-1",
      peerJid: "bob@example.com",
      body: "hello from sibling sender",
    }));
  });

  test("does not double-process carbon:received + forwarded message events", () => {
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    const forwarded = {
      id: "c-recv-1",
      type: "chat",
      from: "bob@example.com/phone",
      to: "alice@example.com/desktop",
      body: "hello from another resource path",
    };
    // stanza emits both `carbon:received` and a synthetic `message` event for
    // the same forwarded stanza.
    xmpp.emit("carbon:received", {
      carbon: {
        forward: {
          message: forwarded,
        },
      },
    });
    xmpp.emit("message", forwarded);

    expect(dmHandler).toHaveBeenCalledTimes(1);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "c-recv-1",
      peerJid: "bob@example.com",
      body: "hello from another resource path",
    }));
  });
});

describe("inbox push adapter", () => {
  test("preserves thread metadata from inbound inbox pushes", () => {
    const client = new BrowserXmppClient(session());
    const inboxEntries: InboxEntry[] = [];
    client.setInboxPushHandler((entry) => {
      inboxEntries.push(entry);
    });
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("message", {
      type: "headline",
      from: "example.com",
      to: "alice@example.com/desktop",
      inboxPush: {
        partner: "space_channel@muc.example.com",
        kind: "muc",
        lastStanzaId: "sid-thread",
        lastUpdated: 1_700_000,
        unread: 2,
        preview: "thread reply",
        thread: "thread-1",
        threadTitle: "Planning",
        replyCount: 5,
        author: "bob",
      },
    });

    expect(inboxEntries).toEqual([
      {
        partner: "space_channel@muc.example.com",
        kind: "muc",
        lastStanzaId: "sid-thread",
        lastUpdated: 1_700_000,
        unread: 2,
        preview: "thread reply",
        thread: "thread-1",
        threadTitle: "Planning",
        replyCount: 5,
        author: "bob",
      },
    ]);
  });
});

describe("optimistic UI waits for successful sends", () => {
  test("room composer keeps queued messages visible and durable", async () => {
    const sendGroupMessage = mock(async () => ({ id: "queued-room-1", state: "queued" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useMessaging(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      normalizeError,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.sendMessage("hello room", []);

    expect(sendGroupMessage).toHaveBeenCalledTimes(1);
    expect(sendChatState).not.toHaveBeenCalled();
    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0]?.deliveryStatus).toBe("queued");
    expect(actionError.value).toBe("");
  });

  test("room composer allows bodyless standard MUC thread metadata", async () => {
    const sendGroupMessage = mock(async () => ({ id: "thread-marker-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useMessaging(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      normalizeError,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.sendMessage("", [], [], undefined, undefined, { threadId: "thread-root" });

    expect(sendGroupMessage).toHaveBeenCalledWith(
      "w1",
      "c1",
      "",
      expect.objectContaining({
        threadId: "thread-root",
      }),
    );
    expect(messaging.messages.value.at(-1)).toMatchObject({
      id: "thread-marker-1",
      body: "",
      threadId: "thread-root",
    });
  });

  test("DM composer keeps queued messages visible and durable", async () => {
    const sendDirectMessage = mock(async () => ({ id: "queued-dm-1", state: "queued" as const }));
    const sendDmChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useDmMessaging(
      ref(session()),
      ref({ sendDirectMessage, sendDmChatState } as never),
      ref("bob@example.com"),
      normalizeError,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.sendMessage("hello dm", []);

    expect(sendDirectMessage).toHaveBeenCalledTimes(1);
    expect(sendDmChatState).not.toHaveBeenCalled();
    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0]?.deliveryStatus).toBe("queued");
    expect(actionError.value).toBe("");
  });

  test("room composer ignores chat-state failures after the message send succeeds", async () => {
    const sendGroupMessage = mock(async () => ({ id: "msg-1", state: "sending" as const }));
    const sendChatState = mock(async () => {
      throw new Error("typing unavailable");
    });
    const actionError = ref("");
    const messaging = useMessaging(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendGroupMessage, sendChatState } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      normalizeError,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.sendMessage("hello room", []);
    await Promise.resolve();

    expect(sendGroupMessage).toHaveBeenCalledTimes(1);
    expect(sendChatState).toHaveBeenCalledTimes(1);
    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0]?.id).toBe("msg-1");
    expect(actionError.value).toBe("");
  });
});
