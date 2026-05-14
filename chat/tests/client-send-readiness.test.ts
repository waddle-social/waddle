import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { EventEmitter } from "events";
import { ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
import { useDirectMessages } from "../src/dms/messages";
import { useChannelMessages } from "../src/channels/messages";
import { BrowserXmppClient, roomBareJidFor, type InboxEntry, type LiveDmMessage, type RoomActivityEvent } from "../src/lib/xmpp-client";
import { enqueueQueuedMessage, listQueuedDmMessages, listQueuedRoomMessages } from "../src/lib/outbound-queue-store";
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

function roomWasmMessage(partial: Record<string, unknown> = {}) {
  return {
    mam_id: "mam-room-1",
    id: "room-1",
    from: "room@conference.example.com/bob",
    to: "alice@example.com/desktop",
    message_type: "groupchat",
    body: "hello from another room",
    timestamp: "2024-01-01T00:00:01.000Z",
    reaction_emojis: [],
    is_muc: true,
    markup_spans: [],
    mention_uris: [],
    references: [],
    is_sticker: false,
    shared_files: [],
    ...partial,
  };
}

function dmMessage(partial: Partial<LiveDmMessage> = {}): LiveDmMessage {
  return {
    id: "dm-1",
    peerJid: "bob@example.com",
    fromJid: "bob@example.com/mobile",
    nick: "bob",
    body: "",
    createdAt: "2026-05-08T13:00:00Z",
    type: "message",
    ...partial,
  };
}

async function settleReconnectCatchup(): Promise<void> {
  for (let i = 0; i < 6; i += 1) await Promise.resolve();
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
  test("DM load failures use deterministic UI copy and keep raw errors out of actionError", async () => {
    const rawError = new Error("remote-server-timeout at xmpp.example.com for bob@example.com");
    const client = {
      queryPersonalMamPage: mock(async () => {
        throw rawError;
      }),
    } as unknown as BrowserXmppClient;
    const actionError = ref("");
    const normalize = mock(() => "Something went wrong. remote-server-timeout at xmpp.example.com for bob@example.com");
    const originalConsoleWarn = console.warn;
    const consoleWarn = mock(() => undefined);
    console.warn = consoleWarn as typeof console.warn;
    try {
      const dm = useDirectMessages(
        ref<WaddleSession | null>(session()),
        ref<BrowserXmppClient | null>(client),
        ref("bob@example.com"),
        normalize,
        actionError,
        () => {
          actionError.value = "";
        },
      );

      await dm.loadMessages("bob@example.com");

      expect(actionError.value).toBe("Could not load @bob. Check the connection and try again.");
      expect(actionError.value).not.toContain("xmpp.example.com");
      expect(actionError.value).not.toContain("bob@example.com");
      expect(normalize).not.toHaveBeenCalled();
      expect(consoleWarn).toHaveBeenCalledWith("Could not load DM conversation");
      expect(consoleWarn.mock.calls.flat().join(" ")).not.toContain("xmpp.example.com");
      expect(consoleWarn.mock.calls.flat().join(" ")).not.toContain("bob@example.com");
      expect(dm.loadErrorPeerJid.value).toBe("bob@example.com");
    } finally {
      console.warn = originalConsoleWarn;
    }
  });

  test("DM load failures keep queued messages visible with a retryable warning", async () => {
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "queued-dm-1",
      createdAt: "2026-05-07T00:00:00Z",
      peerJid: "bob@example.com",
      body: "queued while offline",
    });
    const client = {
      queryPersonalMamPage: mock(async () => {
        throw new Error("remote-server-timeout at xmpp.example.com for bob@example.com");
      }),
    } as unknown as BrowserXmppClient;
    const actionError = ref("");
    const originalConsoleWarn = console.warn;
    console.warn = mock(() => undefined) as typeof console.warn;
    try {
      const dm = useDirectMessages(
        ref<WaddleSession | null>(session()),
        ref<BrowserXmppClient | null>(client),
        ref("bob@example.com"),
        normalizeError,
        actionError,
        () => {
          actionError.value = "";
        },
      );

      await dm.loadMessages("bob@example.com");

      expect(dm.messages.value).toHaveLength(1);
      expect(dm.messages.value[0]).toMatchObject({
        id: "queued-dm-1",
        body: "queued while offline",
        deliveryStatus: "queued",
      });
      expect(actionError.value).toBe(
        "Could not load @bob history. Showing queued messages only. Check the connection and try again.",
      );
      expect(dm.loadErrorPeerJid.value).toBe("bob@example.com");
      expect(dm.loadErrorMessage.value).toBe(actionError.value);
    } finally {
      console.warn = originalConsoleWarn;
    }
  });

  test("DM live retractions require the same bare sender as the target", () => {
    const actionError = ref("");
    const dm = useDirectMessages(
      ref<WaddleSession | null>(session()),
      ref<BrowserXmppClient | null>({} as BrowserXmppClient),
      ref("bob@example.com"),
      normalizeError,
      actionError,
      () => {
        actionError.value = "";
      },
    );
    dm.messages.value = [{
      id: "target-client-id",
      wireIds: ["target-origin-id"],
      author: "bob",
      authorJid: "bob@example.com/mobile",
      body: "keep me",
      createdAt: "2026-05-08T13:00:00Z",
      isSelf: false,
      markup: [{ type: "span", start: 0, end: 4, styles: ["strong"] }],
      references: [{ type: "data", uri: "https://example.com", begin: 0, end: 4 }],
      sharedFiles: [{ url: "https://example.com/file.png", disposition: "inline" }],
      extensionAnnotations: [],
      mentions: ["alice@example.com"],
    }];

    dm.onIncomingMessage(dmMessage({ id: "spoofed-retract", fromJid: "mallory@example.com/home", retractsId: "target-origin-id" }));
    expect(dm.messages.value[0]?.isRetracted).toBeUndefined();

    dm.onIncomingMessage(dmMessage({ id: "valid-retract", fromJid: "bob@example.com/laptop", retractsId: "target-origin-id" }));
    expect(dm.messages.value[0]?.isRetracted).toBe(true);
    expect(dm.messages.value[0]?.markup).toBeUndefined();
    expect(dm.messages.value[0]?.references).toBeUndefined();
    expect(dm.messages.value[0]?.sharedFiles).toBeUndefined();
    expect(dm.messages.value[0]?.extensionAnnotations).toBeUndefined();
    expect(dm.messages.value[0]?.mentions).toBeUndefined();
  });

  test("room sends immediately when the room is ready", async () => {
    const xmpp = { send_groupchat_message: mock(async (_room: string, _body: string, opts: { stanza_id?: string }) => opts.stanza_id) };
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
    expect(xmpp.send_groupchat_message).toHaveBeenCalledTimes(1);
  });

  test("XEP-0201: BrowserXmppClient.sendGroupMessage accepts bodyless thread metadata sends", async () => {
    // XEP-0201 thread create / thread reply payloads are bodyless. The
    // browser client wrapper must not short-circuit them; otherwise standard
    // MUC threads can never leave the browser through the regular send path.
    const xmpp = { send_groupchat_message: mock(async (_room: string, _body: string, opts: { stanza_id?: string }) => opts.stanza_id) };
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
    expect(xmpp.send_groupchat_message).toHaveBeenCalledTimes(1);
    expect(xmpp.send_groupchat_message).toHaveBeenCalledWith(
      roomJid,
      "",
      expect.objectContaining({ thread: { id: "thread-root" } }),
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

  test("does not call removed history helper on stream-management resume", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as { catchup: { recordDmSeen: (peer: string, ts: string) => void; onSessionStarted: () => unknown[] } }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z");
    // Prime initial login marker so the next reconnect path performs catch-up.
    catchup.onSessionStarted();
    const removedHistoryHelper = mock(async () => ({ results: [] }));

    const xmpp = Object.assign(new EventEmitter(), {
      enableKeepAlive: mock((_opts: { interval: number; timeout: number }) => undefined),
      disableKeepAlive: mock(() => undefined),
      getTime: mock(async () => ({ utc: new Date("2024-01-01T00:00:00Z") })),
      removedHistoryHelper,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await Promise.resolve();
    await Promise.resolve();

    expect(removedHistoryHelper).not.toHaveBeenCalled();
  });

  test("runs Rust MAM catch-up for tracked DMs on stream-management resume", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as { catchup: { recordDmSeen: (peer: string, ts: string) => void; onSessionStarted: () => unknown[] } }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z");
    catchup.onSessionStarted();
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const fetchDmHistoryPage = mock(async () => ({
      messages: [{
        mam_id: "mam-2",
        id: "dm-2",
        from: "bob@example.com/phone",
        to: "alice@example.com/desktop",
        message_type: "chat",
        body: "missed while suspended",
        timestamp: "2024-01-01T00:00:01.000Z",
        reaction_emojis: [],
        shared_files: [],
      }],
      complete: true,
    }));

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await Promise.resolve();
    await Promise.resolve();

    expect(fetchDmHistoryPage).toHaveBeenCalledWith("bob@example.com", 100, { type: "latest" });
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "dm-2",
      body: "missed while suspended",
      peerJid: "bob@example.com",
    }));
  });

  test("runs tracked catch-up on subsequent fresh session starts", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z", undefined, ["dm-already-seen"]);
    const fetchDmHistoryPage = mock(async () => ({
      messages: [{
        mam_id: "mam-fresh-2",
        id: "dm-fresh-2",
        from: "bob@example.com/phone",
        to: "alice@example.com/desktop",
        message_type: "chat",
        body: "fresh reconnect catch-up",
        timestamp: "2024-01-01T00:00:01.000Z",
        reaction_emojis: [],
        shared_files: [],
      }],
      is_complete: true,
    }));

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("session:started");
    await settleReconnectCatchup();
    expect(fetchDmHistoryPage).toHaveBeenCalledTimes(0);

    xmpp.emit("session:started");
    await settleReconnectCatchup();

    expect(fetchDmHistoryPage).toHaveBeenCalledTimes(1);
    expect(fetchDmHistoryPage).toHaveBeenCalledWith("bob@example.com", 100, { type: "latest" });
  });

  test("timestamp fallback catch-up skips already-seen messages and learns archive ids", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z", undefined, ["dm-already-seen"]);
    catchup.onSessionStarted();
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const fetchDmHistoryPage = mock(async () => ({
      messages: [
        {
          mam_id: "mam-already-seen",
          id: "dm-already-seen",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          body: "already seen",
          timestamp: "2024-01-01T01:00:00+01:00",
          reaction_emojis: [],
          shared_files: [],
        },
        {
          mam_id: "mam-newer",
          id: "dm-newer",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          body: "newer missed message",
          timestamp: "2024-01-01T00:00:01.000Z",
          reaction_emojis: [],
          shared_files: [],
        },
      ],
      last_id: "mam-newer",
      is_complete: true,
    }));

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(dmHandler).toHaveBeenCalledTimes(1);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "dm-newer",
      body: "newer missed message",
      peerJid: "bob@example.com",
    }));
    expect(catchup.onSessionStarted()).toEqual([
      { kind: "dm", key: "bob@example.com", after: "mam-newer", seenIds: ["dm-newer"] },
    ]);
  });

  test("timestamp fallback pages backward until it crosses the last seen timestamp", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z", undefined, ["dm-seen"]);
    catchup.onSessionStarted();
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const fetchDmHistoryPage = mock(async (_peer: string, _max: number, pageParam: { type: string; before?: string }) => {
      if (pageParam.type === "latest") {
        return {
          messages: [{
            mam_id: "mam-newer-2",
            id: "dm-newer-2",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            message_type: "chat",
            body: "newest missed message",
            timestamp: "2024-01-01T00:00:03.000Z",
            reaction_emojis: [],
            shared_files: [],
          }],
          first_id: "mam-newer-2",
          last_id: "mam-newer-2",
          is_complete: false,
        };
      }
      if (pageParam.before === "mam-newer-2") {
        return {
          messages: [
            {
              mam_id: "mam-same-time-missed",
              id: "dm-same-time-missed",
              from: "bob@example.com/phone",
              to: "alice@example.com/desktop",
              message_type: "chat",
              body: "same timestamp missed message",
              timestamp: "2024-01-01T00:00:00.000Z",
              reaction_emojis: [],
              shared_files: [],
            },
            {
              mam_id: "mam-seen",
              id: "dm-seen",
              from: "bob@example.com/phone",
              to: "alice@example.com/desktop",
              message_type: "chat",
              body: "seen boundary",
              timestamp: "2024-01-01T00:00:00.000Z",
              reaction_emojis: [],
              shared_files: [],
            },
            {
              mam_id: "mam-newer-1",
              id: "dm-newer-1",
              from: "bob@example.com/phone",
              to: "alice@example.com/desktop",
              message_type: "chat",
              body: "older missed message beyond first page",
              timestamp: "2024-01-01T00:00:02.000Z",
              reaction_emojis: [],
              shared_files: [],
            },
          ],
          first_id: "mam-same-time-missed",
          last_id: "mam-newer-1",
          is_complete: false,
        };
      }
      return {
        messages: [
          {
            mam_id: "mam-older-than-boundary",
            id: "dm-older-than-boundary",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            message_type: "chat",
            body: "older than boundary",
            timestamp: "2023-12-31T23:59:59.000Z",
            reaction_emojis: [],
            shared_files: [],
          },
        ],
        first_id: "mam-older-than-boundary",
        last_id: "mam-newer-1",
        is_complete: false,
      };
    });

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchDmHistoryPage).toHaveBeenNthCalledWith(1, "bob@example.com", 100, { type: "latest" });
    expect(fetchDmHistoryPage).toHaveBeenNthCalledWith(2, "bob@example.com", 100, {
      type: "before",
      before: "mam-newer-2",
    });
    expect(fetchDmHistoryPage).toHaveBeenNthCalledWith(3, "bob@example.com", 100, {
      type: "before",
      before: "mam-same-time-missed",
    });
    expect(fetchDmHistoryPage).toHaveBeenCalledTimes(3);
    expect(dmHandler).toHaveBeenCalledTimes(3);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "dm-newer-2" }));
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "dm-newer-1" }));
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "dm-same-time-missed" }));
  });

  test("stale archive cursor catch-up filters live messages already seen after that cursor", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z", "mam-1");
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:01.000Z", undefined, ["dm-live-already-seen"]);
    catchup.onSessionStarted();
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const fetchDmHistoryPage = mock(async () => ({
      messages: [
        {
          mam_id: "mam-between-archive-and-live",
          id: "dm-between-archive-and-live",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          body: "missed between archived cursor and live watermark",
          timestamp: "2024-01-01T00:00:00.500Z",
          reaction_emojis: [],
          shared_files: [],
        },
        {
          mam_id: "mam-2",
          id: "dm-live-already-seen",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          body: "already seen live",
          timestamp: "2024-01-01T00:00:01.000Z",
          reaction_emojis: [],
          shared_files: [],
        },
        {
          mam_id: "mam-3",
          id: "dm-actually-missed",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          body: "actually missed",
          timestamp: "2024-01-01T00:00:02.000Z",
          reaction_emojis: [],
          shared_files: [],
        },
      ],
      last_id: "mam-3",
      is_complete: true,
    }));

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchDmHistoryPage).toHaveBeenCalledWith("bob@example.com", 100, {
      type: "after",
      after: "mam-1",
    });
    expect(dmHandler).toHaveBeenCalledTimes(2);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "dm-between-archive-and-live",
      body: "missed between archived cursor and live watermark",
    }));
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "dm-actually-missed",
      body: "actually missed",
    }));
  });

  test("pages tracked DM catch-up forward from the last MAM archive id", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z", "mam-1");
    catchup.onSessionStarted();
    let resolveSecondCatchup: () => void = () => undefined;
    const secondCatchupHandled = new Promise<void>((resolve) => {
      resolveSecondCatchup = resolve;
    });
    const dmHandler = mock((message: LiveDmMessage) => {
      if (message.id === "dm-3") resolveSecondCatchup();
    });
    client.setDirectMessageHandler(dmHandler);
    const fetchDmHistoryPage = mock(async (_peer: string, _max: number, pageParam: { type: string; after?: string }) => {
      if (pageParam.after === "mam-1") {
        return {
          messages: [{
            mam_id: "mam-2",
            id: "dm-2",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            message_type: "chat",
            body: "first missed page",
            timestamp: "2024-01-01T00:00:01.000Z",
            reaction_emojis: [],
            shared_files: [],
          }],
          last_id: "mam-2",
          is_complete: false,
        };
      }
      return {
        messages: [{
          mam_id: "mam-3",
          id: "dm-3",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          body: "second missed page",
          timestamp: "2024-01-01T00:00:02.000Z",
          reaction_emojis: [],
          shared_files: [],
        }],
        last_id: "mam-3",
        is_complete: true,
      };
    });

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await secondCatchupHandled;

    expect(fetchDmHistoryPage).toHaveBeenNthCalledWith(1, "bob@example.com", 100, {
      type: "after",
      after: "mam-1",
    });
    expect(fetchDmHistoryPage).toHaveBeenNthCalledWith(2, "bob@example.com", 100, {
      type: "after",
      after: "mam-2",
    });
    expect(fetchDmHistoryPage).toHaveBeenCalledTimes(2);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "dm-2",
      body: "first missed page",
      peerJid: "bob@example.com",
    }));
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "dm-3",
      body: "second missed page",
      peerJid: "bob@example.com",
    }));
    expect(dmHandler).toHaveBeenCalledTimes(2);
  });

  test("queryPersonalMamPage seeds reconnect catch-up from XEP-0313 archive ids", async () => {
    const client = new BrowserXmppClient(session());
    (client as unknown as { connect: () => Promise<void> }).connect = async () => undefined;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup.onSessionStarted();
    const fetchDmHistoryPage = mock(async () => ({
      messages: [{
        mam_id: "mam-loaded-1",
        id: "dm-loaded-1",
        from: "bob@example.com/phone",
        to: "alice@example.com/desktop",
        message_type: "chat",
        body: "loaded from mam",
        timestamp: "2024-01-01T00:00:01.000Z",
        reaction_emojis: [],
        shared_files: [],
      }],
      last_id: "mam-loaded-1",
      is_complete: true,
    }));
    (client as unknown as { xmpp: Agent }).xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;

    await client.queryPersonalMamPage("bob@example.com", 100, { type: "latest" });

    expect((client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup.onSessionStarted()).toEqual([
      { kind: "dm", key: "bob@example.com", after: "mam-loaded-1", seenIds: ["dm-loaded-1"] },
    ]);
  });

  test("room timestamp fallback skips already-seen messages and learns archive ids", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const catchup = (client as unknown as {
      catchup: {
        recordRoomSeen: (room: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordRoomSeen(roomJid, "2024-01-01T00:00:00.000Z", undefined, ["room-seen"]);
    catchup.onSessionStarted();
    const activityHandler = mock(() => undefined);
    client.setActivityHandler(activityHandler);
    const fetchRoomHistoryPage = mock(async () => ({
      messages: [
        roomWasmMessage({
          mam_id: "mam-room-seen",
          id: "room-seen",
          from: `${roomJid}/bob`,
          body: "already seen room message",
          timestamp: "2024-01-01T01:00:00+01:00",
        }),
        roomWasmMessage({
          mam_id: "mam-room-newer",
          id: "room-newer",
          from: `${roomJid}/bob`,
          body: "newer missed room message",
          timestamp: "2024-01-01T00:00:01.000Z",
        }),
      ],
      last_id: "mam-room-newer",
      is_complete: true,
    }));
    const xmpp = Object.assign(new EventEmitter(), {
      fetch_room_history_page: fetchRoomHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(activityHandler).toHaveBeenCalledTimes(1);
    expect(activityHandler).toHaveBeenCalledWith(expect.objectContaining({
      roomJid,
      body: "newer missed room message",
    }));
    expect(catchup.onSessionStarted()).toEqual([
      { kind: "room", key: roomJid, after: "mam-room-newer", seenIds: ["room-newer"] },
    ]);
  });

  test("room timestamp fallback pages backward until it crosses the last seen timestamp", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;
    const catchup = (client as unknown as {
      catchup: {
        recordRoomSeen: (room: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
      };
    }).catchup;
    catchup.recordRoomSeen(roomJid, "2024-01-01T00:00:00.000Z", undefined, ["room-seen"]);
    (client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup.onSessionStarted();
    const roomHandler = mock(() => undefined);
    client.setMessageHandler(roomHandler);
    const fetchRoomHistoryPage = mock(async (_room: string, _max: number, pageParam: { type: string; before?: string }) => {
      if (pageParam.type === "latest") {
        return {
          messages: [roomWasmMessage({
            mam_id: "mam-room-newer-2",
            id: "room-newer-2",
            from: `${roomJid}/bob`,
            body: "newest missed room message",
            timestamp: "2024-01-01T00:00:03.000Z",
          })],
          first_id: "mam-room-newer-2",
          last_id: "mam-room-newer-2",
          is_complete: false,
        };
      }
      if (pageParam.before === "mam-room-newer-2") {
        return {
          messages: [
            roomWasmMessage({
              mam_id: "mam-room-same-time-missed",
              id: "room-same-time-missed",
              from: `${roomJid}/bob`,
              body: "same timestamp missed room message",
              timestamp: "2024-01-01T00:00:00.000Z",
            }),
            roomWasmMessage({
              mam_id: "mam-room-seen",
              id: "room-seen",
              from: `${roomJid}/bob`,
              body: "seen room boundary",
              timestamp: "2024-01-01T00:00:00.000Z",
            }),
            roomWasmMessage({
              mam_id: "mam-room-newer-1",
              id: "room-newer-1",
              from: `${roomJid}/bob`,
              body: "older missed room message beyond first page",
              timestamp: "2024-01-01T00:00:02.000Z",
            }),
          ],
          first_id: "mam-room-same-time-missed",
          last_id: "mam-room-newer-1",
          is_complete: false,
        };
      }
      return {
        messages: [
          roomWasmMessage({
            mam_id: "mam-room-older-than-boundary",
            id: "room-older-than-boundary",
            from: `${roomJid}/bob`,
            body: "older than room boundary",
            timestamp: "2023-12-31T23:59:59.000Z",
          }),
        ],
        first_id: "mam-room-older-than-boundary",
        last_id: "mam-room-newer-1",
        is_complete: false,
      };
    });
    const xmpp = Object.assign(new EventEmitter(), {
      fetch_room_history_page: fetchRoomHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchRoomHistoryPage).toHaveBeenNthCalledWith(1, roomJid, 100, { type: "latest" });
    expect(fetchRoomHistoryPage).toHaveBeenNthCalledWith(2, roomJid, 100, {
      type: "before",
      before: "mam-room-newer-2",
    });
    expect(fetchRoomHistoryPage).toHaveBeenNthCalledWith(3, roomJid, 100, {
      type: "before",
      before: "mam-room-same-time-missed",
    });
    expect(fetchRoomHistoryPage).toHaveBeenCalledTimes(3);
    expect(roomHandler).toHaveBeenCalledTimes(3);
    expect(roomHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "room-newer-2" }));
    expect(roomHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "room-newer-1" }));
    expect(roomHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "room-same-time-missed" }));
  });

  test("room stale archive cursor catch-up filters live activity already seen after that cursor", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const catchup = (client as unknown as {
      catchup: {
        recordRoomSeen: (room: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordRoomSeen(roomJid, "2024-01-01T00:00:00.000Z", "mam-room-1");
    catchup.onSessionStarted();
    const activityHandler = mock(() => undefined);
    const fetchRoomHistoryPage = mock(async () => ({
      messages: [
        roomWasmMessage({
          mam_id: "mam-room-between-archive-and-live",
          id: "room-between-archive-and-live",
          from: `${roomJid}/bob`,
          body: "missed room message between archived cursor and live watermark",
          timestamp: "2024-01-01T00:00:00.500Z",
        }),
        roomWasmMessage({
          mam_id: "mam-room-2",
          id: undefined,
          from: `${roomJid}/bob`,
          body: "already seen room live",
          timestamp: "2024-01-01T00:00:01.000Z",
          stanza_id: "foreign-stanza-id",
          stanza_id_by: "archive.example.com",
          stanza_ids: [
            { id: "foreign-stanza-id", by: "archive.example.com" },
            { id: "room-scoped-already-seen", by: roomJid },
          ],
        }),
        roomWasmMessage({
          mam_id: "mam-room-3",
          id: "room-actually-missed",
          from: `${roomJid}/bob`,
          body: "actually missed room message",
          timestamp: "2024-01-01T00:00:02.000Z",
        }),
      ],
      last_id: "mam-room-3",
      is_complete: true,
    }));
    const xmpp = Object.assign(new EventEmitter(), {
      fetch_room_history_page: fetchRoomHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);
    xmpp.emit("message", roomWasmMessage({
      id: undefined,
      mam_id: "live-fallback-only",
      from: `${roomJid}/bob`,
      body: "already seen room live",
      timestamp: "2024-01-01T00:00:01.000Z",
      stanza_id: "foreign-stanza-id",
      stanza_id_by: "archive.example.com",
      stanza_ids: [
        { id: "foreign-stanza-id", by: "archive.example.com" },
        { id: "room-scoped-already-seen", by: roomJid },
      ],
    }));
    client.setActivityHandler(activityHandler);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchRoomHistoryPage).toHaveBeenCalledWith(roomJid, 100, {
      type: "after",
      after: "mam-room-1",
    });
    expect(activityHandler).toHaveBeenCalledTimes(2);
    expect(activityHandler).toHaveBeenCalledWith(expect.objectContaining({
      roomJid,
      body: "missed room message between archived cursor and live watermark",
    }));
    expect(activityHandler).toHaveBeenCalledWith(expect.objectContaining({
      roomJid,
      body: "actually missed room message",
    }));
  });

  test("pages tracked room catch-up forward from the last MAM archive id", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    (client as unknown as { connect: () => Promise<void> }).connect = async () => undefined;
    (client as unknown as { switchRoom: () => Promise<void> }).switchRoom = async () => undefined;
    (client as unknown as { connected: boolean }).connected = true;
    const catchup = (client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup;
    catchup.onSessionStarted();
    const roomHandler = mock(() => undefined);
    client.setMessageHandler(roomHandler);
    const fetchRoomHistoryPage = mock(async (_room: string, _max: number, pageParam: { type: string; after?: string }) => {
      if (pageParam.type === "latest") {
        return {
          messages: [roomWasmMessage({
            mam_id: "mam-room-1",
            id: "room-1",
            from: `${roomJid}/bob`,
            body: "loaded room history",
            timestamp: "2024-01-01T00:00:00.000Z",
          })],
          last_id: "mam-room-1",
          is_complete: true,
        };
      }
      if (pageParam.after === "mam-room-1") {
        return {
          messages: [roomWasmMessage({
            mam_id: "mam-room-2",
            id: "room-2",
            from: `${roomJid}/bob`,
            body: "first missed room page",
            timestamp: "2024-01-01T00:00:01.000Z",
          })],
          last_id: "mam-room-2",
          is_complete: false,
        };
      }
      return {
        messages: [roomWasmMessage({
          mam_id: "mam-room-3",
          id: "room-3",
          from: `${roomJid}/bob`,
          body: "second missed room page",
          timestamp: "2024-01-01T00:00:02.000Z",
        })],
        last_id: "mam-room-3",
        is_complete: true,
      };
    });
    const xmpp = Object.assign(new EventEmitter(), {
      fetch_room_history_page: fetchRoomHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;

    await client.queryMamPage("w1", "c1", 100, { type: "latest" });
    expect(catchup.onSessionStarted()).toEqual([
      { kind: "room", key: roomJid, after: "mam-room-1", seenIds: ["room-1"] },
    ]);

    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);
    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchRoomHistoryPage).toHaveBeenNthCalledWith(2, roomJid, 100, {
      type: "after",
      after: "mam-room-1",
    });
    expect(fetchRoomHistoryPage).toHaveBeenNthCalledWith(3, roomJid, 100, {
      type: "after",
      after: "mam-room-2",
    });
    expect(fetchRoomHistoryPage).toHaveBeenCalledTimes(3);
    expect(roomHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "room-2",
      body: "first missed room page",
      roomJid,
    }));
    expect(roomHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "room-3",
      body: "second missed room page",
      roomJid,
    }));
  });
});

describe("room activity adapter", () => {
  test("does not emit XEP-0308 corrections or retractions as off-room activity", () => {
    const client = new BrowserXmppClient(session());
    const activity: RoomActivityEvent[] = [];
    const roomMessages: unknown[] = [];
    client.setActivityHandler((event) => activity.push(event));
    client.setMessageHandler((message) => roomMessages.push(message));
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("message", roomWasmMessage({
      id: "normal-message",
      body: "hello alice",
      mention_uris: ["xmpp:alice@example.com"],
    }));
    xmpp.emit("message", roomWasmMessage({
      id: "edited-message",
      body: "edited hello alice",
      replaces_id: "normal-message",
      mention_uris: ["xmpp:alice@example.com"],
    }));
    xmpp.emit("message", roomWasmMessage({
      id: "retracted-message",
      from: "room@conference.example.com",
      body: "removed hello alice",
      retracts_id: "normal-message",
      moderation_target_id: "normal-message",
      mention_uris: ["xmpp:alice@example.com"],
    }));

    expect(activity).toEqual([
      {
        roomJid: "room@conference.example.com",
        nick: "bob",
        body: "hello alice",
        mentions: ["alice@example.com"],
      },
    ]);
    expect(roomMessages).toEqual([
      expect.objectContaining({ id: "edited-message", replacesId: "normal-message" }),
      expect.objectContaining({ id: "retracted-message", retractsId: "normal-message" }),
    ]);
  });

  test("does not project reconnect catch-up corrections or retractions as room activity", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as { catchup: { recordRoomSeen: (room: string, ts: string) => void; onSessionStarted: () => unknown[] } }).catchup;
    catchup.recordRoomSeen("room@conference.example.com", "2024-01-01T00:00:00.000Z");
    catchup.onSessionStarted();
    const activity: RoomActivityEvent[] = [];
    client.setActivityHandler((event) => activity.push(event));
    const fetchRoomHistoryPage = mock(async () => ({
      messages: [
        roomWasmMessage({
          mam_id: "mam-normal-message",
          id: "normal-message",
          body: "missed hello alice",
          mention_uris: ["xmpp:alice@example.com"],
        }),
        roomWasmMessage({
          mam_id: "mam-edited-message",
          id: "edited-message",
          body: "edited missed hello alice",
          replaces_id: "normal-message",
          mention_uris: ["xmpp:alice@example.com"],
        }),
        roomWasmMessage({
          mam_id: "mam-retracted-message",
          id: "retracted-message",
          from: "room@conference.example.com",
          body: "removed missed hello alice",
          retracts_id: "normal-message",
          moderation_target_id: "normal-message",
          mention_uris: ["xmpp:alice@example.com"],
        }),
      ],
      is_complete: true,
    }));
    const xmpp = Object.assign(new EventEmitter(), {
      fetch_room_history_page: fetchRoomHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await Promise.resolve();
    await Promise.resolve();

    expect(fetchRoomHistoryPage).toHaveBeenCalledWith("room@conference.example.com", 100, { type: "latest" });
    expect(activity).toEqual([
      {
        roomJid: "room@conference.example.com",
        nick: "bob",
        body: "missed hello alice",
        mentions: ["alice@example.com"],
      },
    ]);
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
    const messaging = useChannelMessages(
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

  test("room programmatic sends do not clear the composer draft", async () => {
    const sendGroupMessage = mock(async () => ({ id: "public-prompt-1", state: "queued" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
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
    messaging.draft.value = "/ai tell the room";

    await messaging.sendMessage("tell the room");

    expect(sendGroupMessage).toHaveBeenCalledWith(
      "w1",
      "c1",
      "tell the room",
      expect.any(Object),
    );
    expect(messaging.draft.value).toBe("/ai tell the room");
    expect(actionError.value).toBe("");
  });

  test("room composer allows bodyless standard MUC thread metadata", async () => {
    const sendGroupMessage = mock(async () => ({ id: "thread-marker-1", state: "sending" as const }));
    const sendChatState = mock(async () => undefined);
    const actionError = ref("");
    const messaging = useChannelMessages(
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
    const messaging = useDirectMessages(
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
    const messaging = useChannelMessages(
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
