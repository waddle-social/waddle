import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { EventEmitter } from "events";
import { ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
import { useDirectMessages } from "../src/dms/messages";
import { useChannelMessages } from "../src/channels/messages";
import {
  BrowserXmppClient,
  MUC_JOIN_RETRY_DELAYS_MS,
  MucJoinRejectedError,
  MucJoinSelfPresenceTimeoutError,
  roomBareJidFor,
  type DmConversationScope,
  type InboxEntry,
  type LiveDmMessage,
  type RoomActivityEvent,
} from "../src/lib/xmpp-client";
import { enqueueQueuedMessage, listQueuedDmMessages, listQueuedRoomMessages } from "../src/lib/outbound-queue-store";
import { applyDmCallEvent, clearDmCallActivities, readDmCallActivity } from "../src/lib/calls/dm-call-activity";
import { $dmCallOutcomeAnchor } from "../src/lib/calls/dm-call-anchor";
import type { ResumePersistence } from "../src/lib/xmpp/resume-persistence";
import type { XmppErrorEvent } from "../src/lib/xmpp/types";
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

function dmWasmMessage(partial: Record<string, unknown> = {}) {
  return {
    mam_id: "mam-dm-1",
    id: "dm-1",
    from: "bob@example.com/phone",
    to: "alice@example.com/desktop",
    message_type: "chat",
    body: "hello from dm",
    timestamp: "2024-01-01T00:00:01.000Z",
    reaction_emojis: [],
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

async function settleReconnectCatchup(turns = 6): Promise<void> {
  for (let i = 0; i < turns; i += 1) await Promise.resolve();
}

async function settleUntil(predicate: () => boolean, turns = 20): Promise<void> {
  for (let i = 0; i < turns; i += 1) {
    if (predicate()) return;
    await Promise.resolve();
  }
  throw new Error("condition did not settle");
}

function markReadyTestRoom(client: BrowserXmppClient, roomJid: string): void {
  const roomKey = roomJid.split("/")[0]?.trim().toLowerCase() ?? "";
  const internals = client as unknown as {
    currentRoom: string | null;
    joinedMucs: Map<string, Promise<void>>;
    joinedMucReady: Set<string>;
  };
  internals.currentRoom = roomJid;
  internals.joinedMucs.set(roomKey, Promise.resolve());
  internals.joinedMucReady.add(roomKey);
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
  clearDmCallActivities();
  $dmCallOutcomeAnchor.set(null);
});

afterEach(() => {
  clearDmCallActivities();
  $dmCallOutcomeAnchor.set(null);
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

  test("failed catch-up fallback reload keeps messages queued during the reload", async () => {
    // #1180: the fallback restores the pre-reload snapshot when its own
    // reload fails — but a message the user queued DURING that reload
    // lives only in the outbound store + the catch's queued-only reset.
    // The restore must merge it in, not clobber it away until echo.
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "queued-mid-reload",
      createdAt: new Date().toISOString(),
      peerJid: "bob@example.com",
      body: "sent while the reload was in flight",
    });
    const client = {
      queryPersonalMamPage: mock(async () => {
        throw new Error("remote-server-timeout");
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
        String,
        actionError,
        () => {
          actionError.value = "";
        },
      );
      dm.messages.value = [
        {
          id: "dm-old",
          author: "bob",
          body: "earlier",
          createdAt: "2024-01-01T00:00:00Z",
          isSelf: false,
        },
      ];

      dm.onCatchupFailed({ kind: "dm", key: "bob@example.com" });
      await settleReconnectCatchup(6);

      const ids = dm.messages.value.map((m) => m.id);
      expect(ids).toContain("dm-old");
      expect(ids).toContain("queued-mid-reload");
    } finally {
      console.warn = originalConsoleWarn;
    }
  });

  test("DM load failures keep queued messages visible with a retryable warning", async () => {
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "queued-dm-1",
      // Recent stamp — the queue store now prunes entries older
      // than 7 days on read (PR4).
      createdAt: new Date().toISOString(),
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
      joinedMucReady: Set<string>;
    }).currentRoom = roomJid;
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);

    const result = await client.sendGroupMessage("w1", "c1", "hello room");

    expect(typeof result?.id).toBe("string");
    expect(result?.state).toBe("sending");
    expect(xmpp.send_groupchat_message).toHaveBeenCalledTimes(1);
    expect(listQueuedRoomMessages("alice@example.com", roomJid).map((message) => message.id)).toEqual([
      result?.id,
    ]);

    (client as unknown as { handleMessageAck: (id: string) => void }).handleMessageAck(result!.id!);
    expect(listQueuedRoomMessages("alice@example.com", roomJid)).toEqual([]);
  });

  test("room send rejects typed WASM failures instead of returning a null sending id", async () => {
    const xmpp = { send_groupchat_message: mock(async () => ({ kind: "not-connected" })) };
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);

    await expect(client.sendGroupMessage("w1", "c1", "hello room", { id: "room-send-1" })).rejects.toThrow("XMPP send failed: not-connected");
    expect(listQueuedRoomMessages("alice@example.com", roomJid).map((message) => message.id)).toEqual(["room-send-1"]);
  });

  test("room send drops deterministic typed WASM failures from the retry queue", async () => {
    const xmpp = { send_groupchat_message: mock(async () => ({ kind: "invalid-options" })) };
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);

    await expect(client.sendGroupMessage("w1", "c1", "hello room", { id: "room-send-invalid" })).rejects.toThrow("XMPP send failed: invalid-options");
    expect(listQueuedRoomMessages("alice@example.com", roomJid)).toEqual([]);
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
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);

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

  test("room queue replay rejects typed WASM failures so queued sends stay retryable", async () => {
    const xmpp = { send_groupchat_message: mock(async () => ({ kind: "transport-error" })) };
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const statuses: Array<[string, "queued" | "sending"]> = [];
    client.setQueuedMessageStatusHandler((id, status) => statuses.push([id, status]));
    enqueueQueuedMessage("alice@example.com", {
      kind: "room",
      id: "room-replay-1",
      createdAt: new Date().toISOString(),
      roomJid,
      body: "queued room",
    });
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);

    await expect((client as unknown as { flushQueuedRoomMessages: (roomJid: string) => Promise<void> }).flushQueuedRoomMessages(roomJid)).rejects.toThrow("XMPP send failed: transport-error");
    expect(listQueuedRoomMessages("alice@example.com", roomJid).map((message) => message.id)).toEqual(["room-replay-1"]);
    expect(statuses).toEqual([["room-replay-1", "sending"]]);
  });

  test("room join readiness waits for this resource's MUC self-presence", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> }).ensureJoined(roomJid);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_jid: "alice@example.com/phone",
    });
    await Promise.resolve();
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(false);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });

    await joined;
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("room join readiness ignores unavailable self-presence", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> }).ensureJoined(roomJid);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "unavailable",
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });
    await Promise.resolve();
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(false);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });

    await joined;
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("room join rejects immediately when the join stanza cannot be sent", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const joinError = new Error("websocket transport error");
    const xmpp = {
      join_room: mock(async () => {
        throw joinError;
      }),
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;

    await expect(
      (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> }).ensureJoined(roomJid),
    ).rejects.toThrow("websocket transport error");

    expect(xmpp.join_room).toHaveBeenCalledWith(roomJid, "alice");
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucJoinTokens: Map<string, symbol> }).joinedMucJoinTokens.has(roomJid)).toBe(false);
    expect((client as unknown as { roomJoinWaiters: Map<string, unknown> }).roomJoinWaiters.size).toBe(0);
  });

  test("expected MUC access denial is non-pageable and does not retry", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const errors: XmppErrorEvent[] = [];
    const staleJoinVisibleAtEmit: boolean[] = [];
    const retryDelays: number[] = [];
    (client as unknown as {
      waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
    }).waitBeforeMucJoinRetry = async (delayMs) => {
      retryDelays.push(delayMs);
    };
    client.onError((event) => {
      errors.push(event);
      // A listener that synchronously retries a terminal join must NOT
      // observe the rejected single-flight promise.
      staleJoinVisibleAtEmit.push(
        (client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid),
      );
    });
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
      error_text?: string;
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> })
      .ensureJoined(roomJid)
      .catch((error: unknown) => error);
    await Promise.resolve();
    expect((client as unknown as { roomJoinWaiters: Map<string, unknown> }).roomJoinWaiters.size).toBe(1);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
      error_condition: "registration-required",
      error_type: "auth",
      error_text: "Membership required to join managed channel.",
    });

    const rejection = await joined;
    expect(rejection).toBeInstanceOf(MucJoinRejectedError);
    expect(rejection).toMatchObject({
      condition: "registration-required",
      errorType: "auth",
      text: "Membership required to join managed channel.",
    });
    expect(normalizeError(rejection)).toContain("Channel presence was rejected");
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucJoinTokens: Map<string, symbol> }).joinedMucJoinTokens.has(roomJid)).toBe(false);
    expect((client as unknown as { roomJoinWaiters: Map<string, unknown> }).roomJoinWaiters.size).toBe(0);
    expect(xmpp.join_room).toHaveBeenCalledTimes(1);
    expect(retryDelays).toEqual([]);
    expect(errors).toHaveLength(1);
    expect(errors[0].kind).toBe("muc-join");
    expect(errors[0].recoverable).toBe(true);
    expect(errors[0].condition).toBe("registration-required");
    expect(errors[0].errorType).toBe("auth");
    expect(errors[0].errorText).toBe("Membership required to join managed channel.");
    expect(errors[0].roomLocalpart).toBe("c1");
    expect(staleJoinVisibleAtEmit).toEqual([false]);
  });

  test("access-denial conditions remain terminal even with a wait stanza type", async () => {
    for (const condition of [
      "forbidden",
      "not-allowed",
      "not-authorized",
      "registration-required",
    ]) {
      const client = new BrowserXmppClient(session());
      const roomJid = roomBareJidFor(session(), `denied-${condition}`);
      const errors: XmppErrorEvent[] = [];
      const retryDelays: number[] = [];
      client.onError((event) => errors.push(event));
      (client as unknown as {
        waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
      }).waitBeforeMucJoinRetry = async (delayMs) => {
        retryDelays.push(delayMs);
      };
      let onPresence: ((presence: {
        from?: string;
        presence_type: string;
        error_condition?: string;
        error_type?: string;
      }) => void) | null = null;
      const xmpp = {
        join_room: mock(async () => undefined),
        set_on_presence(cb: NonNullable<typeof onPresence>) {
          onPresence = cb;
        },
      };
      (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
      (client as unknown as { connected: boolean }).connected = true;
      (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

      const joined = (client as unknown as {
        ensureJoined: (roomJid: string) => Promise<void>;
      }).ensureJoined(roomJid).catch((error: unknown) => error);
      await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
      onPresence?.({
        from: `${roomJid}/alice`,
        presence_type: "error",
        error_condition: condition,
        error_type: "wait",
      });

      await expect(joined).resolves.toBeInstanceOf(MucJoinRejectedError);
      expect(xmpp.join_room).toHaveBeenCalledTimes(1);
      expect(retryDelays).toEqual([]);
      expect(errors).toHaveLength(1);
      expect(errors[0]).toMatchObject({
        kind: "muc-join",
        recoverable: true,
        condition,
        errorType: "wait",
      });
    }
  });

  test("switching from a ready room never self-pings a denied room before status 110", async () => {
    const client = new BrowserXmppClient(session());
    const readyRoomJid = roomBareJidFor(session(), "c1");
    const deniedRoomJid = roomBareJidFor(session(), "c2");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      send_raw_iq: mock(async () => "<iq type='result'/>"),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const readySwitch = client.switchRoom("w1", "c1");
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    onPresence?.({
      from: `${readyRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    await readySwitch;
    expect((client as unknown as { selfPingTimer: unknown }).selfPingTimer).not.toBeNull();

    const deniedSwitch = client.switchRoom("w1", "c2").catch((error: unknown) => error);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);
    expect((client as unknown as { selfPingTimer: unknown }).selfPingTimer).toBeNull();
    await (client as unknown as { doSelfPing: () => Promise<void> }).doSelfPing();
    expect(xmpp.send_raw_iq).not.toHaveBeenCalled();

    onPresence?.({
      from: `${deniedRoomJid}/alice`,
      presence_type: "error",
      error_condition: "registration-required",
      error_type: "auth",
    });

    await expect(deniedSwitch).resolves.toBeInstanceOf(MucJoinRejectedError);
    await (client as unknown as { doSelfPing: () => Promise<void> }).doSelfPing();
    expect(xmpp.send_raw_iq).not.toHaveBeenCalled();
    expect(
      (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(deniedRoomJid),
    ).toBe(false);
  });

  test("switching rooms detaches a transient join retry without cancelling shared room readiness", async () => {
    const client = new BrowserXmppClient(session());
    const retryingRoomJid = roomBareJidFor(session(), "c1");
    const nextRoomJid = roomBareJidFor(session(), "c2");
    const errors: XmppErrorEvent[] = [];
    client.onError((event) => errors.push(event));
    let markRetryStarted!: () => void;
    let releaseRetry!: () => void;
    const retryStarted = new Promise<void>((resolve) => {
      markRetryStarted = resolve;
    });
    const retryGate = new Promise<void>((resolve) => {
      releaseRetry = resolve;
    });
    (client as unknown as {
      waitBeforeMucJoinRetry: () => Promise<void>;
    }).waitBeforeMucJoinRetry = async () => {
      markRetryStarted();
      await retryGate;
    };
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      send_raw_iq: mock(async () => "<iq type='result'/>"),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const obsoleteSwitch = client.switchRoom("w1", "c1");
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    onPresence?.({
      from: `${retryingRoomJid}/alice`,
      presence_type: "error",
      error_condition: "resource-constraint",
      error_type: "wait",
    });
    await retryStarted;
    const retryingJoin = (client as unknown as {
      joinedMucs: Map<string, Promise<void>>;
    }).joinedMucs.get(retryingRoomJid);
    expect(retryingJoin).toBeDefined();

    const nextSwitch = client.switchRoom("w1", "c2");
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);
    expect(xmpp.join_room.mock.calls[1]?.[0]).toBe(nextRoomJid);
    expect(
      (client as unknown as {
        joinedMucs: Map<string, Promise<void>>;
      }).joinedMucs.get(retryingRoomJid),
    ).toBe(retryingJoin);

    onPresence?.({
      from: `${nextRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    await expect(nextSwitch).resolves.toBeUndefined();
    await expect(obsoleteSwitch).resolves.toBeUndefined();
    expect((client as unknown as { currentRoom: string }).currentRoom).toBe(nextRoomJid);

    // A delayed success from the superseded room may accurately restore that
    // room's background occupancy, but it must not steal focus or the ping.
    onPresence?.({
      from: `${retryingRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    releaseRetry();
    await expect(retryingJoin).resolves.toBeUndefined();
    await (client as unknown as { doSelfPing: () => Promise<void> }).doSelfPing();
    expect(xmpp.send_raw_iq).toHaveBeenCalledTimes(1);
    expect(xmpp.send_raw_iq.mock.calls[0]?.[0]).toContain(`to="${nextRoomJid}/alice"`);
    expect((client as unknown as { currentRoom: string }).currentRoom).toBe(nextRoomJid);
    expect(errors).toEqual([]);
    (client as unknown as { stopSelfPing: () => void }).stopSelfPing();
  });

  test("concurrent switches to the same target keep one join single-flight", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const first = client.switchRoom("w1", "c1");
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    const second = client.switchRoom("w1", "c1");
    await settleReconnectCatchup();
    expect(xmpp.join_room).toHaveBeenCalledTimes(1);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(Promise.all([first, second])).resolves.toEqual([
      undefined,
      undefined,
    ]);
    expect(xmpp.join_room).toHaveBeenCalledTimes(1);
    (client as unknown as { stopSelfPing: () => void }).stopSelfPing();
  });

  test("a superseded A-B-A switch cannot send before A readiness is confirmed", async () => {
    const client = new BrowserXmppClient(session());
    const firstRoomJid = roomBareJidFor(session(), "c1");
    const secondRoomJid = roomBareJidFor(session(), "c2");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      send_reaction: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const staleSend = client
      .sendReaction("w1", "c1", "message-1", ["👍"])
      .then(() => "sent", normalizeError);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);

    // Queue both newer selections in the same task. Their connect()
    // continuations run B then A, before the superseded first A switch
    // resumes its caller.
    const awaySwitch = client.switchRoom("w1", "c2");
    const freshSwitch = client.switchRoom("w1", "c1");
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);

    expect(xmpp.join_room.mock.calls.map((call) => call[0])).toEqual([
      firstRoomJid,
      secondRoomJid,
    ]);
    expect(await staleSend).toBe(`Room is not ready: ${firstRoomJid}`);
    expect(xmpp.send_reaction).not.toHaveBeenCalled();
    await expect(awaySwitch).resolves.toBeUndefined();

    // The detached B membership join remains valid background work too.
    onPresence?.({
      from: `${secondRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    onPresence?.({
      from: `${firstRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(freshSwitch).resolves.toBeUndefined();
    expect(xmpp.send_reaction).not.toHaveBeenCalled();
    (client as unknown as { stopSelfPing: () => void }).stopSelfPing();
  });

  test("a superseded A-B MAM load cannot query the room selected before navigation", async () => {
    const client = new BrowserXmppClient(session());
    const firstRoomJid = roomBareJidFor(session(), "c1");
    const secondRoomJid = roomBareJidFor(session(), "c2");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const fetchRoomHistoryPage = mock(async () => ({
      messages: [],
      is_complete: true,
    }));
    const xmpp = {
      join_room: mock(async () => undefined),
      fetch_room_history_page: fetchRoomHistoryPage,
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const staleMam = client
      .queryMamPage("w1", "c1", 100, { type: "latest" })
      .then(() => "queried", normalizeError);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);

    const awaySwitch = client.switchRoom("w1", "c2");
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);

    expect(await staleMam).toBe(`Room is not ready: ${firstRoomJid}`);
    expect(fetchRoomHistoryPage).not.toHaveBeenCalled();

    // Complete both detached membership joins so no attempt timer survives
    // the test. Neither completion may resurrect the superseded MAM caller.
    onPresence?.({
      from: `${firstRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    onPresence?.({
      from: `${secondRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(awaySwitch).resolves.toBeUndefined();
    await settleReconnectCatchup();
    expect(fetchRoomHistoryPage).not.toHaveBeenCalled();
    (client as unknown as { stopSelfPing: () => void }).stopSelfPing();
  });

  test("MAM joins and queries the same canonical room when topology lands during connect", async () => {
    const client = new BrowserXmppClient(session());
    const canonicalRoomJid = "canonical@rooms.example.com";
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const fetchRoomHistoryPage = mock(async () => ({
      messages: [],
      is_complete: true,
    }));
    const xmpp = {
      join_room: mock(async () => undefined),
      fetch_room_history_page: fetchRoomHistoryPage,
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    const internals = client as unknown as {
      connect: () => Promise<void>;
      xmpp: typeof xmpp;
      connected: boolean;
      wireEvents: (xmpp: typeof xmpp) => void;
    };
    internals.connect = mock(async () => {
      internals.xmpp = xmpp;
      internals.connected = true;
      client.rememberRoomJidForChannel("c1", canonicalRoomJid);
      internals.wireEvents(xmpp);
    });

    const page = client.queryMamPage("w1", "c1", 100, { type: "latest" });
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    onPresence?.({
      from: `${canonicalRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(page).resolves.toEqual({
      messages: [],
      complete: true,
    });
    expect(xmpp.join_room).toHaveBeenCalledWith(canonicalRoomJid, "alice");
    expect(fetchRoomHistoryPage).toHaveBeenCalledWith(
      canonicalRoomJid,
      100,
      { type: "latest" },
    );
    (client as unknown as { stopSelfPing: () => void }).stopSelfPing();
  });

  test("a superseded A-B-A MAM load cannot query before the fresh A join is ready", async () => {
    const client = new BrowserXmppClient(session());
    const firstRoomJid = roomBareJidFor(session(), "c1");
    const secondRoomJid = roomBareJidFor(session(), "c2");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const fetchRoomHistoryPage = mock(async () => ({
      messages: [],
      is_complete: true,
    }));
    const xmpp = {
      join_room: mock(async () => undefined),
      fetch_room_history_page: fetchRoomHistoryPage,
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const staleMam = client
      .queryMamPage("w1", "c1", 100, { type: "latest" })
      .then(() => "queried", normalizeError);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);

    // Supersede the first A selection with B, then immediately select A
    // again. Both background memberships remain valid work, but only the
    // fresh A switch may authorize A's room-scoped MAM IQ.
    const awaySwitch = client.switchRoom("w1", "c2");
    const freshMam = client.queryMamPage("w1", "c1", 100, { type: "latest" });
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);

    expect(xmpp.join_room.mock.calls.map((call) => call[0])).toEqual([
      firstRoomJid,
      secondRoomJid,
    ]);
    expect(await staleMam).toBe(`Room is not ready: ${firstRoomJid}`);
    expect(fetchRoomHistoryPage).not.toHaveBeenCalled();
    await expect(awaySwitch).resolves.toBeUndefined();

    onPresence?.({
      from: `${secondRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    await settleReconnectCatchup();
    expect(fetchRoomHistoryPage).not.toHaveBeenCalled();

    onPresence?.({
      from: `${firstRoomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(freshMam).resolves.toEqual({
      messages: [],
      complete: true,
    });
    expect(fetchRoomHistoryPage).toHaveBeenCalledTimes(1);
    expect(fetchRoomHistoryPage).toHaveBeenCalledWith(
      firstRoomJid,
      100,
      { type: "latest" },
    );
    (client as unknown as { stopSelfPing: () => void }).stopSelfPing();
  });

  test("a handle lost immediately after connect rejects the room switch boundary", async () => {
    const client = new BrowserXmppClient(session());
    const internals = client as unknown as {
      connect: () => Promise<void>;
      xmpp: null;
      connected: boolean;
      roomSwitchPromise: Promise<void> | null;
    };
    internals.connect = mock(async () => {
      internals.xmpp = null;
      internals.connected = false;
    });

    await expect(client.switchRoom("w1", "c1")).rejects.toThrow(
      "XMPP connection changed while switching rooms",
    );
    expect(internals.roomSwitchPromise).toBeNull();
  });

  test("connection replacement rejects a completed room switch boundary and rejoins on the fresh handle", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onOldPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const oldXmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onOldPresence>) {
        onOldPresence = cb;
      },
      get_resume_state: () => null,
      get_resume_state_handle: () => null,
    };
    (client as unknown as { xmpp: typeof oldXmpp; connected: boolean }).xmpp = oldXmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof oldXmpp) => void }).wireEvents(oldXmpp);

    const staleSwitch = client.switchRoom("w1", "c1");
    await settleUntil(() => oldXmpp.join_room.mock.calls.length === 1);
    // Resolve the room boundary, then replace the handle in the same callback
    // turn before the switch continuation can issue MAM/member/pin work.
    onOldPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    (client as unknown as {
      handleDisconnected: (xmpp: typeof oldXmpp) => void;
    }).handleDisconnected(oldXmpp);
    (client as unknown as { clearReconnectTimer: () => void }).clearReconnectTimer();

    await expect(staleSwitch).rejects.toThrow(
      "XMPP disconnected while switching rooms",
    );

    let onFreshPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const freshXmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onFreshPresence>) {
        onFreshPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof freshXmpp; connected: boolean }).xmpp = freshXmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof freshXmpp) => void }).wireEvents(freshXmpp);

    const freshSwitch = client.switchRoom("w1", "c1");
    await settleUntil(() => freshXmpp.join_room.mock.calls.length === 1);
    onFreshPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(freshSwitch).resolves.toBeUndefined();
    expect(oldXmpp.join_room).toHaveBeenCalledTimes(1);
    expect(freshXmpp.join_room).toHaveBeenCalledTimes(1);
    expect((client as unknown as { currentRoom: string }).currentRoom).toBe(roomJid);
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
    (client as unknown as { stopSelfPing: () => void }).stopSelfPing();
  });

  test("disconnect during the post-join queue flush rejects the room switch boundary", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    let markFlushStarted!: () => void;
    let releaseFlush!: () => void;
    const flushStarted = new Promise<void>((resolve) => {
      markFlushStarted = resolve;
    });
    const flushGate = new Promise<void>((resolve) => {
      releaseFlush = resolve;
    });
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
      get_resume_state: () => null,
      get_resume_state_handle: () => null,
    };
    const internals = client as unknown as {
      xmpp: typeof xmpp;
      connected: boolean;
      wireEvents: (xmpp: typeof xmpp) => void;
      flushQueuedRoomMessages: () => Promise<void>;
      handleDisconnected: (xmpp: typeof xmpp) => void;
      clearReconnectTimer: () => void;
    };
    internals.xmpp = xmpp;
    internals.connected = true;
    internals.flushQueuedRoomMessages = mock(async () => {
      markFlushStarted();
      await flushGate;
    });
    internals.wireEvents(xmpp);

    const switched = client.switchRoom("w1", "c1");
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    await flushStarted;

    internals.handleDisconnected(xmpp);
    internals.clearReconnectTimer();
    await expect(switched).rejects.toThrow(
      "XMPP disconnected while switching rooms",
    );
    releaseFlush();
  });

  test("disconnect cancels an already-ready room switch before its flush settles", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let markFlushStarted!: () => void;
    let releaseFlush!: () => void;
    const flushStarted = new Promise<void>((resolve) => {
      markFlushStarted = resolve;
    });
    const flushGate = new Promise<void>((resolve) => {
      releaseFlush = resolve;
    });
    const xmpp = {
      get_resume_state: () => null,
      get_resume_state_handle: () => null,
    };
    const internals = client as unknown as {
      xmpp: typeof xmpp;
      connected: boolean;
      currentRoom: string;
      joinedMucReady: Set<string>;
      flushQueuedRoomMessages: () => Promise<void>;
      handleDisconnected: (xmpp: typeof xmpp) => void;
      clearReconnectTimer: () => void;
    };
    internals.xmpp = xmpp;
    internals.connected = true;
    internals.currentRoom = roomJid;
    internals.joinedMucReady.add(roomJid);
    internals.flushQueuedRoomMessages = mock(async () => {
      markFlushStarted();
      await flushGate;
    });

    const switched = client.switchRoom("w1", "c1");
    await flushStarted;
    internals.handleDisconnected(xmpp);
    internals.clearReconnectTimer();

    await expect(switched).rejects.toThrow(
      "XMPP disconnected while switching rooms",
    );
    releaseFlush();
  });

  test("wait-type MUC join rejection retries and eventually succeeds", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const errors: XmppErrorEvent[] = [];
    const retryDelays: number[] = [];
    client.onError((event) => errors.push(event));
    (client as unknown as {
      waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
    }).waitBeforeMucJoinRetry = async (delayMs) => {
      retryDelays.push(delayMs);
    };
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined(roomJid);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
      error_condition: "resource-constraint",
      error_type: "wait",
    });

    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(joined).resolves.toBeUndefined();
    expect(xmpp.join_room).toHaveBeenCalledTimes(2);
    expect(retryDelays).toEqual([250]);
    expect(errors).toEqual([]);
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("self-presence timeout retries an ambiguous commit without an intermediate Faro error", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const errors: XmppErrorEvent[] = [];
    const retryDelays: number[] = [];
    client.onError((event) => errors.push(event));
    (client as unknown as { roomSelfPresenceTimeoutMs: number })
      .roomSelfPresenceTimeoutMs = 1;

    let markRetryStarted!: () => void;
    let releaseRetry!: () => void;
    const retryStarted = new Promise<void>((resolve) => {
      markRetryStarted = resolve;
    });
    (client as unknown as {
      waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
    }).waitBeforeMucJoinRetry = (delayMs) => {
      retryDelays.push(delayMs);
      markRetryStarted();
      return new Promise<void>((resolve) => {
        releaseRetry = resolve;
      });
    };

    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined(roomJid);
    await retryStarted;
    expect(xmpp.join_room).toHaveBeenCalledTimes(1);
    expect(errors).toEqual([]);

    // Keep the second attempt open long enough to inject the owner-built
    // status-110 response deterministically.
    (client as unknown as { roomSelfPresenceTimeoutMs: number })
      .roomSelfPresenceTimeoutMs = 1_000;
    releaseRetry();
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(joined).resolves.toBeUndefined();
    expect(retryDelays).toEqual([250]);
    expect(errors).toEqual([]);
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("self-presence timeouts exhaust the shared bounded retry budget and emit once", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const errors: XmppErrorEvent[] = [];
    const retryDelays: number[] = [];
    client.onError((event) => errors.push(event));
    (client as unknown as { roomSelfPresenceTimeoutMs: number })
      .roomSelfPresenceTimeoutMs = 1;
    (client as unknown as {
      waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
    }).waitBeforeMucJoinRetry = async (delayMs) => {
      retryDelays.push(delayMs);
    };
    const xmpp = {
      join_room: mock(async () => undefined),
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;

    const rejection = await (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined(roomJid).catch((error: unknown) => error);

    expect(rejection).toBeInstanceOf(MucJoinSelfPresenceTimeoutError);
    expect(xmpp.join_room).toHaveBeenCalledTimes(MUC_JOIN_RETRY_DELAYS_MS.length + 1);
    expect(retryDelays).toEqual([...MUC_JOIN_RETRY_DELAYS_MS]);
    expect(errors).toHaveLength(1);
    expect(errors[0]).toMatchObject({
      kind: "connect-timeout",
      recoverable: true,
      detail: `Timed out waiting for self-presence in ${roomJid}`,
      roomLocalpart: "c1",
    });
    expect(errors[0].cause).toBe(rejection);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucJoinTokens: Map<string, symbol> }).joinedMucJoinTokens.has(roomJid)).toBe(false);
  });

  test("late self-presence during backoff suppresses a duplicate join", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let releaseRetry!: () => void;
    (client as unknown as {
      waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
    }).waitBeforeMucJoinRetry = () => new Promise<void>((resolve) => {
      releaseRetry = resolve;
    });
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined(roomJid);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
      error_condition: "resource-constraint",
      error_type: "wait",
    });
    await settleUntil(() => typeof releaseRetry === "function");
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    releaseRetry();

    await expect(joined).resolves.toBeUndefined();
    expect(xmpp.join_room).toHaveBeenCalledTimes(1);
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("own unavailable during backoff preserves the canonical join single-flight", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let releaseRetry!: () => void;
    let markRetryStarted!: () => void;
    const retryStarted = new Promise<void>((resolve) => {
      markRetryStarted = resolve;
    });
    (client as unknown as {
      waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
    }).waitBeforeMucJoinRetry = () => {
      markRetryStarted();
      return new Promise<void>((resolve) => {
        releaseRetry = resolve;
      });
    };
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const ensureJoined = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined.bind(client);
    const first = ensureJoined(roomJid);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
      error_condition: "resource-constraint",
      error_type: "wait",
    });
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "unavailable",
      muc_status_codes: [110],
    });
    await retryStarted;
    const second = ensureJoined(roomJid);
    await settleReconnectCatchup();
    expect(xmpp.join_room).toHaveBeenCalledTimes(1);

    releaseRetry();
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(Promise.all([first, second])).resolves.toEqual([undefined, undefined]);
    expect(xmpp.join_room).toHaveBeenCalledTimes(2);
  });

  test("a superseded retry cannot borrow a fresh join's in-flight marker", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let releaseRetry!: () => void;
    let markRetryStarted!: () => void;
    const retryStarted = new Promise<void>((resolve) => {
      markRetryStarted = resolve;
    });
    (client as unknown as {
      waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
    }).waitBeforeMucJoinRetry = () => {
      markRetryStarted();
      return new Promise<void>((resolve) => {
        releaseRetry = resolve;
      });
    };
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const ensureJoined = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined.bind(client);
    const first = ensureJoined(roomJid).then(() => "resolved", normalizeError);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
      error_condition: "resource-constraint",
      error_type: "wait",
    });
    await retryStarted;

    // A delayed success immediately followed by own-unavailable revokes the
    // sleeping sequence's token. A fresh caller may now start a new join
    // before the old retry delay expires.
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "unavailable",
      muc_status_codes: [110],
    });
    const second = ensureJoined(roomJid);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);

    releaseRetry();
    await expect(first).resolves.toBe("Room became unavailable while retrying join");
    expect(xmpp.join_room).toHaveBeenCalledTimes(2);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(true);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    await expect(second).resolves.toBeUndefined();
    expect(xmpp.join_room).toHaveBeenCalledTimes(2);
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("back-to-back self-presence and unavailable cannot restore stale room readiness", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const ensureJoined = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined.bind(client);
    const first = ensureJoined(roomJid);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "unavailable",
      muc_status_codes: [110],
    });

    await expect(first).rejects.toThrow("Room became unavailable while joining");
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucJoinTokens: Map<string, symbol> }).joinedMucJoinTokens.has(roomJid)).toBe(false);

    const second = ensureJoined(roomJid);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(second).resolves.toBeUndefined();
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("synchronous self-presence and unavailable cannot resurrect a rejected join tracker", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let joinAttempt = 0;
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => {
        joinAttempt += 1;
        if (joinAttempt !== 1) return;
        // The WASM binding may synchronously drain queued callbacks before
        // returning its Promise. The tracker must already be published when
        // this available -> unavailable transition revokes the join.
        onPresence?.({
          from: `${roomJid}/alice`,
          presence_type: "available",
          muc_status_codes: [110],
        });
        onPresence?.({
          from: `${roomJid}/alice`,
          presence_type: "unavailable",
          muc_status_codes: [110],
        });
      }),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const ensureJoined = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined.bind(client);
    await expect(ensureJoined(roomJid)).rejects.toThrow("Room became unavailable while joining");
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucJoinTokens: Map<string, symbol> }).joinedMucJoinTokens.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(false);

    const second = ensureJoined(roomJid);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(second).resolves.toBeUndefined();
    expect(xmpp.join_room).toHaveBeenCalledTimes(2);
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("wait-type MUC join rejection exhausts its bounded retry budget", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const errors: XmppErrorEvent[] = [];
    const retryDelays: number[] = [];
    client.onError((event) => errors.push(event));
    (client as unknown as {
      waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
    }).waitBeforeMucJoinRetry = async (delayMs) => {
      retryDelays.push(delayMs);
    };
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined(roomJid).catch((error: unknown) => error);
    const totalAttempts = MUC_JOIN_RETRY_DELAYS_MS.length + 1;
    for (let attempt = 1; attempt <= totalAttempts; attempt += 1) {
      await settleUntil(() => xmpp.join_room.mock.calls.length === attempt);
      onPresence?.({
        from: `${roomJid}/alice`,
        presence_type: "error",
        error_condition: "resource-constraint",
        error_type: "wait",
      });
    }

    const rejection = await joined;
    expect(rejection).toBeInstanceOf(MucJoinRejectedError);
    expect(rejection).toMatchObject({
      condition: "resource-constraint",
      errorType: "wait",
    });
    expect(xmpp.join_room).toHaveBeenCalledTimes(totalAttempts);
    expect(retryDelays).toEqual([...MUC_JOIN_RETRY_DELAYS_MS]);
    expect(errors.map((error) => error.recoverable)).toEqual([true]);
    expect(errors.every((error) => error.condition === "resource-constraint")).toBe(true);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucJoinTokens: Map<string, symbol> }).joinedMucJoinTokens.has(roomJid)).toBe(false);
  });

  test("concurrent room join callers share one transient retry sequence", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const retryDelays: number[] = [];
    (client as unknown as {
      waitBeforeMucJoinRetry: (delayMs: number) => Promise<void>;
    }).waitBeforeMucJoinRetry = async (delayMs) => {
      retryDelays.push(delayMs);
    };
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const ensureJoined = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined.bind(client);
    const first = ensureJoined(roomJid);
    const second = ensureJoined(roomJid);
    await settleUntil(() => xmpp.join_room.mock.calls.length === 1);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
      error_condition: "resource-constraint",
      error_type: "wait",
    });
    await settleUntil(() => xmpp.join_room.mock.calls.length === 2);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(Promise.all([first, second])).resolves.toEqual([undefined, undefined]);
    expect(xmpp.join_room).toHaveBeenCalledTimes(2);
    expect(retryDelays).toEqual([250]);
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("room join ignores unrelated room presence errors while waiting for self-presence", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> }).ensureJoined(roomJid);
    const observed = joined.then(() => "resolved" as const, normalizeError);
    await Promise.resolve();

    onPresence?.({
      from: `${roomJid}/not-the-join-nick`,
      presence_type: "error",
    });
    await Promise.resolve();

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(observed).resolves.toBe("resolved");
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("MUC error presence after a completed join does not revoke readiness", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> }).ensureJoined(roomJid);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    await joined;

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
    });

    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(true);
  });

  test("stale room join rejection after reconnect does not reject the fresh join waiter", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let rejectOldJoin!: (error: Error) => void;
    const oldXmpp = {
      join_room: mock(() => new Promise<void>((_resolve, reject) => {
        rejectOldJoin = reject;
      })),
      get_resume_state: () => null,
      get_resume_state_handle: () => null,
    };
    (client as unknown as { xmpp: typeof oldXmpp; connected: boolean }).xmpp = oldXmpp;
    (client as unknown as { connected: boolean }).connected = true;

    const oldJoin = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> })
      .ensureJoined(roomJid)
      .then(() => "resolved" as const)
      .catch((error) => error);
    await Promise.resolve();

    (client as unknown as { handleDisconnected: (xmpp: typeof oldXmpp) => void }).handleDisconnected(oldXmpp);
    (client as unknown as { clearReconnectTimer: () => void }).clearReconnectTimer();

    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const newXmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof newXmpp; connected: boolean }).xmpp = newXmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof newXmpp) => void }).wireEvents(newXmpp);

    const freshJoin = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> }).ensureJoined(roomJid);
    await Promise.resolve();
    rejectOldJoin(new Error("old transport closed"));
    await Promise.resolve();

    expect((client as unknown as { roomJoinWaiters: Map<string, unknown> }).roomJoinWaiters.size).toBe(1);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await expect(freshJoin).resolves.toBeUndefined();
    expect(normalizeError(await oldJoin)).toBe("old transport closed");
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("connection replacement during join backoff cannot send a stale retry", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");

    let onOldPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
    }) => void) | null = null;
    const oldXmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onOldPresence>) {
        onOldPresence = cb;
      },
      get_resume_state: () => null,
      get_resume_state_handle: () => null,
    };
    (client as unknown as { xmpp: typeof oldXmpp; connected: boolean }).xmpp = oldXmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof oldXmpp) => void }).wireEvents(oldXmpp);

    const oldJoin = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined(roomJid).catch((error: unknown) => error);
    await settleUntil(() => oldXmpp.join_room.mock.calls.length === 1);
    onOldPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
      error_condition: "resource-constraint",
      error_type: "wait",
    });
    await settleUntil(() =>
      (client as unknown as {
        mucJoinRetryCancellers: Set<(error: Error) => void>;
      }).mucJoinRetryCancellers.size === 1
    );

    (client as unknown as {
      handleDisconnected: (xmpp: typeof oldXmpp) => void;
    }).handleDisconnected(oldXmpp);
    (client as unknown as { clearReconnectTimer: () => void }).clearReconnectTimer();

    let onFreshPresence: ((presence: {
      from?: string;
      presence_type: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const freshXmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onFreshPresence>) {
        onFreshPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof freshXmpp; connected: boolean }).xmpp = freshXmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof freshXmpp) => void }).wireEvents(freshXmpp);

    const freshJoin = (client as unknown as {
      ensureJoined: (roomJid: string) => Promise<void>;
    }).ensureJoined(roomJid);
    await settleUntil(() => freshXmpp.join_room.mock.calls.length === 1);
    await settleReconnectCatchup();

    expect(normalizeError(await oldJoin)).toBe("XMPP disconnected while retrying room join");
    expect(oldXmpp.join_room).toHaveBeenCalledTimes(1);
    expect(freshXmpp.join_room).toHaveBeenCalledTimes(1);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(true);

    onFreshPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });
    await expect(freshJoin).resolves.toBeUndefined();
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("disconnect clears room join tokens so stale join resolution cannot mark ready", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let resolveOldJoin!: () => void;
    const oldXmpp = {
      join_room: mock(() => new Promise<void>((resolve) => {
        resolveOldJoin = resolve;
      })),
      get_resume_state: () => null,
      get_resume_state_handle: () => null,
    };
    (client as unknown as { xmpp: typeof oldXmpp; connected: boolean }).xmpp = oldXmpp;
    (client as unknown as { connected: boolean }).connected = true;

    const oldJoin = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> }).ensureJoined(roomJid);
    await Promise.resolve();
    expect((client as unknown as { joinedMucJoinTokens: Map<string, symbol> }).joinedMucJoinTokens.has(roomJid)).toBe(true);

    (client as unknown as { handleDisconnected: (xmpp: typeof oldXmpp) => void }).handleDisconnected(oldXmpp);
    (client as unknown as { clearReconnectTimer: () => void }).clearReconnectTimer();
    expect((client as unknown as { joinedMucJoinTokens: Map<string, symbol> }).joinedMucJoinTokens.size).toBe(0);

    resolveOldJoin();
    await expect(oldJoin).rejects.toThrow("XMPP connection changed while joining room");

    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(false);
  });

  test("room join resolves on XEP-0045 status code 110 in an anonymous room (no real JID disclosed)", async () => {
    // Semi-/anonymous rooms don't disclose the occupant's real JID, so
    // self-presence can only be recognised by `<status code='110'/>`
    // (XEP-0045 §7.2.2). The join must still complete.
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> }).ensureJoined(roomJid);
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_status_codes: [110],
    });

    await joined;
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("room join resolves when the room assigns a different nick than requested", async () => {
    // XEP-0045 lockdown can hand the joiner a service-modified nick
    // (status 210). Self-presence (110) identifies us regardless of the
    // echoed occupant nick, so keying the join waiter on the exact
    // requested `room/nick` would wrongly time out.
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    const joined = (client as unknown as { ensureJoined: (roomJid: string) => Promise<void> }).ensureJoined(roomJid);
    onPresence?.({
      from: `${roomJid}/alice-2`,
      presence_type: "available",
      muc_status_codes: [210, 110],
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });

    await joined;
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("own unavailable self-presence revokes room readiness and forces a fresh join", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      send_groupchat_message: mock(async (_room: string, _body: string, opts: { stanza_id?: string }) => opts.stanza_id),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as {
      xmpp: typeof xmpp;
      connected: boolean;
      currentRoom: string | null;
    }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;
    (client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.set(roomJid, Promise.resolve());
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "unavailable",
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });

    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(false);

    const sent = await client.sendGroupMessage("w1", "c1", "after unavailable");
    expect(sent?.state).toBe("queued");
    expect(xmpp.send_groupchat_message).toHaveBeenCalledTimes(0);
    await settleReconnectCatchup();
    expect(xmpp.join_room).toHaveBeenCalledWith(roomJid, "alice");

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });
    await settleUntil(() => xmpp.send_groupchat_message.mock.calls.length === 1);
    expect(xmpp.send_groupchat_message).toHaveBeenCalledWith(
      roomJid,
      "after unavailable",
      expect.any(Object),
    );
  });

  test("own unavailable self-presence recognised via status 110 revokes readiness in an anonymous room", async () => {
    // Anonymous-room leave/kick: no real JID is disclosed, so the only
    // self marker is status 110. Readiness must still be revoked.
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; currentRoom: string | null }).xmpp = xmpp;
    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;
    (client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.set(roomJid, Promise.resolve());
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "unavailable",
      muc_status_codes: [110],
    });

    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(false);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(false);
  });

  test("own nick-change unavailable (status 303+110) does NOT revoke room readiness", async () => {
    // XEP-0045 §7.6: the unavailable presence for the old nick carries
    // our 110 alongside 303. The room stays ready; the available
    // presence for the new nick follows.
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
      muc_status_codes?: number[];
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; currentRoom: string | null }).xmpp = xmpp;
    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;
    (client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.set(roomJid, Promise.resolve());
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "unavailable",
      muc_status_codes: [303, 110],
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });

    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
    expect((client as unknown as { joinedMucs: Map<string, Promise<void>> }).joinedMucs.has(roomJid)).toBe(true);
  });

  test("session-ready room queue flush waits for current-room rejoin", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    enqueueQueuedMessage("alice@example.com", {
      kind: "room",
      id: "queued-room-after-resume",
      createdAt: new Date().toISOString(),
      roomJid,
      body: "queued while reconnecting",
    });
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      send_groupchat_message: mock(async (_room: string, _body: string, opts: { stanza_id?: string }) => opts.stanza_id),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp; currentRoom: string | null }).xmpp = xmpp;
    (client as unknown as { currentRoom: string | null }).currentRoom = roomJid;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    // #1221: retained-room rejoin is a FRESH-reconnect behavior. A
    // `resumed` session-ready must NOT re-send join presence (occupancy
    // survives the SM detach); that path is covered in
    // session-ready-join-lifecycle.test.ts.
    (client as unknown as {
      handleSessionReady: (xmpp: typeof xmpp, lifecycle: { type: "fresh" }) => void;
    }).handleSessionReady(xmpp, { type: "fresh" });
    await settleReconnectCatchup();
    expect(xmpp.send_groupchat_message).toHaveBeenCalledTimes(0);

    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });
    await settleReconnectCatchup();

    expect(xmpp.send_groupchat_message).toHaveBeenCalledWith(
      roomJid,
      "queued while reconnecting",
      expect.objectContaining({ stanza_id: "queued-room-after-resume" }),
    );
  });

  test("session-ready rejoins retained non-current rooms", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = "side@muc.example.com";
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as {
      xmpp: typeof xmpp;
      retainedJoinedRoomJids: Set<string>;
    }).xmpp = xmpp;
    (client as unknown as { retainedJoinedRoomJids: Set<string> }).retainedJoinedRoomJids = new Set([roomJid]);
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    // #1221: retained-room rejoin is a FRESH-reconnect behavior. A
    // `resumed` session-ready must NOT re-send join presence (occupancy
    // survives the SM detach); that path is covered in
    // session-ready-join-lifecycle.test.ts.
    (client as unknown as {
      handleSessionReady: (xmpp: typeof xmpp, lifecycle: { type: "fresh" }) => void;
    }).handleSessionReady(xmpp, { type: "fresh" });
    await settleReconnectCatchup();

    expect(xmpp.join_room).toHaveBeenCalledWith(roomJid, "alice");
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });
    await settleReconnectCatchup();
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("session-ready rejoins retained rooms restored from refresh persistence", async () => {
    const roomJid = "muted@muc.example.com";
    const persistence: ResumePersistence = {
      loadCatchup: () => null,
      saveCatchup: () => undefined,
      clearCatchup: () => undefined,
      loadSm: () => null,
      consumeSm: () => null,
      saveSm: () => undefined,
      clearSm: () => undefined,
      preparePagehideHandoff: () => undefined,
      loadJoinedRooms: () => [roomJid],
      saveJoinedRooms: () => undefined,
      clearJoinedRooms: () => undefined,
    };
    const client = new BrowserXmppClient(session(), persistence);
    let onPresence: ((presence: {
      from?: string;
      presence_type?: string;
      muc_jid?: string;
    }) => void) | null = null;
    const xmpp = {
      join_room: mock(async () => undefined),
      set_on_presence(cb: NonNullable<typeof onPresence>) {
        onPresence = cb;
      },
    };
    (client as unknown as { xmpp: typeof xmpp }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);

    // #1221: retained-room rejoin is a FRESH-reconnect behavior. A
    // `resumed` session-ready must NOT re-send join presence (occupancy
    // survives the SM detach); that path is covered in
    // session-ready-join-lifecycle.test.ts.
    (client as unknown as {
      handleSessionReady: (xmpp: typeof xmpp, lifecycle: { type: "fresh" }) => void;
    }).handleSessionReady(xmpp, { type: "fresh" });
    await settleReconnectCatchup();

    expect(xmpp.join_room).toHaveBeenCalledWith(roomJid, "alice");
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "available",
      muc_jid: (client as unknown as { fullJid: string }).fullJid,
    });
    await settleReconnectCatchup();
    expect((client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.has(roomJid)).toBe(true);
  });

  test("DM send queues when the session is unavailable", async () => {
    const xmpp = { sendMessage: mock(() => undefined) };
    const client = new BrowserXmppClient(session());
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => {
      throw new Error("Reconnection timed out");
    });

    const result = await client.sendDirectMessage("bob@example.com", "hello");

    expect(result?.state).toBe("queued");
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toHaveLength(1);
    await expect(client.sendDmReaction("bob@example.com", "msg-1", ["👍"])).rejects.toThrow(
      "Reconnection timed out",
    );
    await expect(client.sendDmDisplayed("bob@example.com", "msg-1")).rejects.toThrow("Reconnection timed out");
    await expect(client.sendDmChatState("bob@example.com", "composing")).rejects.toThrow(
      "Reconnection timed out",
    );
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(0);
  });

  test("DM send drops deterministic typed WASM failures from the retry queue", async () => {
    const xmpp = { send_chat_message: mock(async () => ({ kind: "invalid-recipient" })) };
    const client = new BrowserXmppClient(session());
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;

    await expect(client.sendDirectMessage("bob@example.com/mobile", "hello", { id: "dm-send-invalid" })).rejects.toThrow("XMPP send failed: invalid-recipient");
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toEqual([]);
  });

  test("DM sends are durable until XEP-0198 ack confirms server handling", async () => {
    const xmpp = { send_chat_message: mock(async (_peer: string, _body: string, opts: { stanza_id?: string }) => opts.stanza_id) };
    const client = new BrowserXmppClient(session());
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;

    const result = await client.sendDirectMessage("bob@example.com", "hello", { id: "dm-live-1" });

    expect(result).toEqual({ id: "dm-live-1", state: "sending" });
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account").map((message) => message.id)).toEqual([
      "dm-live-1",
    ]);

    (client as unknown as { handleMessageAck: (id: string) => void }).handleMessageAck("dm-live-1");
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toEqual([]);
  });

  test("custom-service MUC-PM sends preserve the occupant resource before room discovery", async () => {
    const sentTargets: string[] = [];
    const captureTarget = async (target: string) => { sentTargets.push(target); };
    const xmpp = {
      send_chat_message: mock(async (target: string) => { sentTargets.push(target); return "muc-pm-send"; }),
      send_chat_state: mock(captureTarget),
      send_displayed: mock(captureTarget),
      send_retraction: mock(captureTarget),
      send_correction: mock(async (target: string) => { sentTargets.push(target); return "muc-pm-correction"; }),
      send_reaction: mock(captureTarget),
    };
    const client = new BrowserXmppClient(session());
    const occupant = "room@rooms.waddle.example/alice";
    (client as unknown as {
      xmpp: typeof xmpp;
      connected: boolean;
      connect: ReturnType<typeof mock>;
    }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => undefined);
    client.catchup.recordDmSeen(
      " Room@Rooms.Waddle.Example/alice ",
      "2026-07-11T17:00:00.000Z",
      undefined,
      undefined,
      "muc-occupant",
    );

    await client.sendDirectMessage(occupant, "hello", { id: "muc-pm-send" });
    await client.sendDmChatState(occupant, "composing");
    await client.sendDmDisplayed(occupant, "message-1");
    await client.sendDmRetraction(occupant, "message-1");
    await client.sendDmCorrection(occupant, "edited", "message-1");
    await client.sendDmReaction(occupant, "message-1", ["👍"]);

    expect(sentTargets).toEqual(Array(6).fill(occupant));
    expect((xmpp.send_chat_message.mock.calls[0]?.[2] as { muc_pm?: boolean }).muc_pm).toBe(true);
    expect(
      (client as unknown as { directMessageAddress: (peerJid: string) => string })
        .directMessageAddress("user@accounts.example/phone"),
    ).toBe("user@accounts.example");
  });

  test("explicit restored scope preserves every MUC-PM outbound target before discovery", async () => {
    const sentTargets: string[] = [];
    const captureTarget = async (target: string) => { sentTargets.push(target); };
    const xmpp = {
      send_chat_message: mock(async (target: string) => { sentTargets.push(target); return "muc-pm-send"; }),
      send_chat_state: mock(captureTarget),
      send_displayed: mock(captureTarget),
      send_retraction: mock(captureTarget),
      send_correction: mock(async (target: string) => { sentTargets.push(target); return "muc-pm-correction"; }),
      send_reaction: mock(captureTarget),
    };
    const client = new BrowserXmppClient(session());
    const occupant = "room@rooms.custom.example/alice";
    (client as unknown as { xmpp: typeof xmpp }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;

    expect(client.isMucPmPeer(occupant)).toBe(false);
    await client.sendDirectMessage(occupant, "hello", { id: "muc-pm-send", mucPm: true });
    await client.sendDmChatState(occupant, "composing", undefined, "muc-occupant");
    await client.sendDmDisplayed(occupant, "message-1", undefined, "muc-occupant");
    await client.sendDmRetraction(occupant, "message-1", undefined, "muc-occupant");
    await client.sendDmCorrection(
      occupant,
      "edited",
      "message-1",
      undefined,
      undefined,
      undefined,
      undefined,
      "muc-occupant",
    );
    await client.sendDmReaction(occupant, "message-1", ["👍"], undefined, "muc-occupant");

    expect(sentTargets).toEqual(Array(6).fill(occupant));
    expect((xmpp.send_chat_message.mock.calls[0]?.[2] as { muc_pm?: boolean }).muc_pm).toBe(true);
  });

  test("restored conversation scope reaches every DM composer and mutation send", async () => {
    const occupant = "room@rooms.custom.example/alice";
    const sendDirectMessage = mock(async () => ({ id: "outbound-1", state: "sending" as const }));
    const sendDmChatState = mock(async () => undefined);
    const sendDmCorrection = mock(async () => "correction-1");
    const sendDmReaction = mock(async () => undefined);
    const sendDmRetraction = mock(async () => undefined);
    const messaging = useDirectMessages(
      ref(session()),
      ref({
        sendDirectMessage,
        sendDmChatState,
        sendDmCorrection,
        sendDmReaction,
        sendDmRetraction,
        isMucPmPeer: () => false,
      } as never),
      ref(occupant),
      normalizeError,
      ref(""),
      () => undefined,
      ref<DmConversationScope | null>("muc-occupant"),
    );

    await messaging.sendMessage("private hello", []);
    await messaging.editMessage("outbound-1", "edited");
    await messaging.toggleReaction("outbound-1", "👍");
    await messaging.retractMessage("outbound-1");
    messaging.notifyComposing();
    await Promise.resolve();

    expect(sendDirectMessage.mock.calls[0]?.[2]).toMatchObject({ mucPm: true });
    expect(sendDmCorrection.mock.calls[0]?.[7]).toBe("muc-occupant");
    expect(sendDmReaction.mock.calls[0]?.[4]).toBe("muc-occupant");
    expect(sendDmRetraction.mock.calls[0]?.[3]).toBe("muc-occupant");
    expect(sendDmChatState.mock.calls.some((call) => call[3] === "muc-occupant")).toBe(true);
    messaging.disconnect();
  });

  test("native failed-resume fallback owns resend for live unacked DM sends", async () => {
    const xmpp = {
      send_chat_message: mock(async (_peer: string, _body: string, opts: { stanza_id?: string }) => opts.stanza_id),
      get_resume_state: () => ({
        previd: "live-sm-id",
        inboundH: 4,
        outboundH: 9,
        hasUnackedOutbound: true,
        unhandledOutboundStanzas: ["<message xmlns='jabber:client' id='dm-live-1'/>"],
      }),
    };
    const client = new BrowserXmppClient(session());
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { connected: boolean }).connected = true;

    await client.sendDirectMessage("bob@example.com", "hello", { id: "dm-live-1" });
    (client as unknown as { handleDisconnected: (xmpp: typeof xmpp) => void }).handleDisconnected(xmpp);
    (client as unknown as { handleMessageFailed: (id: string) => void }).handleMessageFailed("dm-live-1");
    (client as unknown as { clearReconnectTimer: () => void }).clearReconnectTimer();

    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toEqual([]);
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

    // #1221: session-ready runs once per handle, so a genuine SECOND
    // fresh session uses a NEW handle (a reconnect constructs a fresh
    // XmppClientInstance). The catch-up cursor store is client-level, so
    // the first session arms it (nothing to fetch) and the second pages.
    const session1 = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = session1;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(session1);

    session1.emit("session:started");
    await settleReconnectCatchup();
    expect(fetchDmHistoryPage).toHaveBeenCalledTimes(0);

    const session2 = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = session2;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(session2);

    session2.emit("session:started");
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
      { kind: "dm", scope: "account", key: "bob@example.com", after: "mam-newer", since: "2024-01-01T00:00:01.000Z", seenIds: ["dm-newer"] },
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
    expect(dmHandler.mock.calls.map(([message]) => (message as LiveDmMessage).id)).toEqual([
      "dm-same-time-missed",
      "dm-newer-1",
      "dm-newer-2",
    ]);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "dm-newer-2" }));
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "dm-newer-1" }));
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "dm-same-time-missed" }));
  });

  test("timestamp fallback replays DM call events oldest-to-newest across pages", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z");
    catchup.onSessionStarted();
    const fetchDmHistoryPage = mock(async (_peer: string, _max: number, pageParam: { type: string; before?: string }) => {
      if (pageParam.type === "latest") {
        return {
          messages: [{
            mam_id: "mam-proceed",
            from: "alice@example.com/desktop",
            to: "bob@example.com/phone",
            message_type: "chat",
            timestamp: new Date(Date.now() - 60_000).toISOString(),
            reaction_emojis: [],
            shared_files: [],
            call_event: {
              kind: "proceed",
              from: "alice@example.com/desktop",
              to: "bob@example.com/phone",
              sid: "call-replay",
            },
          }],
          first_id: "mam-proceed",
          last_id: "mam-proceed",
          is_complete: false,
        };
      }
      return {
        messages: [{
          mam_id: "mam-propose",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          timestamp: new Date(Date.now() - 120_000).toISOString(),
          reaction_emojis: [],
          shared_files: [],
          call_event: {
            kind: "propose",
            from: "bob@example.com/phone",
            sid: "call-replay",
            media: { audio: true, video: true },
          },
        }],
        first_id: "mam-propose",
        last_id: "mam-propose",
        is_complete: true,
      };
    });

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchDmHistoryPage).toHaveBeenNthCalledWith(2, "bob@example.com", 100, {
      type: "before",
      before: "mam-proceed",
    });
    expect(readDmCallActivity("bob@example.com")).toMatchObject({
      sid: "call-replay",
      state: "accepted",
      direction: "incoming",
      media: { audio: true, video: true },
    });
  });

  test("drops DM timestamp catch-up pages after the XMPP handle disconnects while awaiting MAM", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z");
    catchup.onSessionStarted();
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    let resolvePage: ((page: unknown) => void) | null = null;
    const fetchDmHistoryPage = mock(async () => new Promise((resolve) => {
      resolvePage = resolve;
    }));

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await Promise.resolve();
    await Promise.resolve();
    expect(fetchDmHistoryPage).toHaveBeenCalledTimes(1);

    xmpp.emit("disconnected");
    resolvePage?.({
      messages: [{
        mam_id: "mam-stale",
        id: "dm-stale",
        from: "bob@example.com/phone",
        to: "alice@example.com/desktop",
        message_type: "chat",
        body: "stale after disconnect",
        timestamp: new Date().toISOString(),
        reaction_emojis: [],
        shared_files: [],
        call_event: {
          kind: "propose",
          from: "bob@example.com/phone",
          sid: "stale-after-disconnect",
          media: { audio: true, video: true },
        },
      }],
      first_id: "mam-stale",
      last_id: "mam-stale",
      is_complete: true,
    });
    await settleReconnectCatchup();

    expect(dmHandler).toHaveBeenCalledTimes(0);
    expect(readDmCallActivity("bob@example.com")).toBeNull();
  });

  test("resumed timestamp catch-up stops at the reconnect page budget (#1221)", async () => {
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
    const newestIndex = 51;
    const fetchDmHistoryPage = mock(async (_peer: string, _max: number, pageParam: { type: string; before?: string }) => {
      const index = pageParam.type === "latest"
        ? newestIndex
        : Number(pageParam.before?.replace("mam-", "") ?? "1") - 1;
      return {
        messages: [dmWasmMessage({
          mam_id: `mam-${index}`,
          id: `dm-${index}`,
          body: index === 0 ? "older than boundary" : `missed page ${index}`,
          timestamp: index === 0
            ? "2023-12-31T23:59:59.000Z"
            : new Date(Date.UTC(2024, 0, 1, 0, 0, index)).toISOString(),
        })],
        first_id: `mam-${index}`,
        last_id: `mam-${index}`,
        is_complete: false,
      };
    });

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup(newestIndex + 20);

    // #1221: capped at 5 pages instead of walking the whole 52-page
    // archive. The backward loop applies the 5 pages it fetched before
    // failing over (partial recovery of the most-recent window), rather
    // than discarding them.
    expect(fetchDmHistoryPage).toHaveBeenCalledTimes(5);
    expect(dmHandler).toHaveBeenCalledTimes(5);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "dm-51" }));
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

  test("tracked DM catch-up rejects non-advancing after pages before delivery", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z", "mam-1", ["dm-1"]);
    catchup.onSessionStarted();
    const dmHandler = mock(() => undefined);
    const errorHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    client.onError(errorHandler);
    const fetchDmHistoryPage = mock(async () => ({
      messages: [dmWasmMessage({
        mam_id: "mam-1",
        id: "dm-duplicate-cursor",
        body: "duplicate cursor row",
        timestamp: "2024-01-01T00:00:00.000Z",
      })],
      last_id: "mam-1",
      is_complete: false,
    }));

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchDmHistoryPage).toHaveBeenCalledTimes(1);
    expect(fetchDmHistoryPage).toHaveBeenCalledWith("bob@example.com", 100, {
      type: "after",
      after: "mam-1",
    });
    expect(dmHandler).not.toHaveBeenCalled();
    expect(errorHandler).toHaveBeenCalledWith(expect.objectContaining({
      kind: "history",
      recoverable: true,
      detail: "Reconnect catch-up failed for bob@example.com",
    }));
    expect(normalizeError(errorHandler.mock.calls[0]?.[0]?.cause)).toContain("non-advancing archive cursor");
  });

  test("tracked DM catch-up falls back to timestamp paging when after cursor is gone", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z", "mam-stale", ["dm-seen"]);
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:01.500Z", undefined, ["dm-live-already-seen-1"]);
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:02.000Z", undefined, ["dm-live-already-seen-2"]);
    catchup.onSessionStarted();
    const dmHandler = mock(() => undefined);
    const errorHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    client.onError(errorHandler);
    const fetchDmHistoryPage = mock(async (_peer: string, _max: number, pageParam: { type: string }) => {
      if (pageParam.type === "after") throw new Error("stanza error: item-not-found");
      return {
        messages: [
          dmWasmMessage({
            mam_id: "mam-seen-sibling",
            id: "dm-seen",
            body: "already seen at stale archive timestamp",
            timestamp: "2024-01-01T00:00:00.000Z",
          }),
          dmWasmMessage({
            mam_id: "mam-recovered",
            id: "dm-recovered",
            body: "recovered between stale cursor and live watermark",
            timestamp: "2024-01-01T00:00:01.000Z",
          }),
          dmWasmMessage({
            mam_id: "mam-live-already-seen-1",
            id: "dm-live-already-seen-1",
            body: "already delivered live before latest watermark",
            timestamp: "2024-01-01T00:00:01.500Z",
          }),
          dmWasmMessage({
            mam_id: "mam-live-already-seen-2",
            id: "dm-live-already-seen-2",
            body: "already delivered live at latest watermark",
            timestamp: "2024-01-01T00:00:02.000Z",
          }),
        ],
        first_id: "mam-seen-sibling",
        last_id: "mam-live-already-seen-2",
        is_complete: true,
      };
    });

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchDmHistoryPage).toHaveBeenNthCalledWith(1, "bob@example.com", 100, {
      type: "after",
      after: "mam-stale",
    });
    expect(fetchDmHistoryPage).toHaveBeenNthCalledWith(2, "bob@example.com", 100, { type: "latest" });
    expect(errorHandler).not.toHaveBeenCalled();
    expect(dmHandler).toHaveBeenCalledTimes(1);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "dm-recovered",
      body: "recovered between stale cursor and live watermark",
    }));
  });

  test("resumed forward catch-up stops at the reconnect page budget (#1221)", async () => {
    const client = new BrowserXmppClient(session());
    const catchup = (client as unknown as {
      catchup: {
        recordDmSeen: (peer: string, ts: string, archiveId?: string) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordDmSeen("bob@example.com", "2024-01-01T00:00:00.000Z", "mam-0");
    catchup.onSessionStarted();
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const finalPage = 51;
    const fetchDmHistoryPage = mock(async (_peer: string, _max: number, pageParam: { type: string; after?: string }) => {
      const index = Number(pageParam.after?.replace("mam-", "") ?? "0") + 1;
      return {
        messages: [dmWasmMessage({
          mam_id: `mam-${index}`,
          id: `dm-${index}`,
          body: `missed page ${index}`,
          timestamp: new Date(Date.UTC(2024, 0, 1, 0, 0, index)).toISOString(),
        })],
        last_id: `mam-${index}`,
        is_complete: index === finalPage,
      };
    });

    const xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup(finalPage + 20);

    // #1221: capped at 5 forward pages; the forward loop applies each
    // page as it goes, so 5 messages arrive before the budget throw.
    expect(fetchDmHistoryPage).toHaveBeenCalledTimes(5);
    expect(fetchDmHistoryPage).toHaveBeenLastCalledWith("bob@example.com", 100, {
      type: "after",
      after: "mam-4",
    });
    expect(dmHandler).toHaveBeenCalledTimes(5);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "dm-5" }));
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
      { kind: "dm", scope: "account", key: "bob@example.com", after: "mam-loaded-1", since: "2024-01-01T00:00:01.000Z", seenIds: ["dm-loaded-1"] },
    ]);
  });

  test("queryPersonalMamPage returns archived DM call outcome cards", async () => {
    const client = new BrowserXmppClient(session());
    (client as unknown as { connect: () => Promise<void> }).connect = async () => undefined;
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup.onSessionStarted();
    const proposedAt = new Date(Date.now() - 60_000).toISOString();
    const retractedAt = new Date(Date.now() - 30_000).toISOString();
    const fetchDmHistoryPage = mock(async () => ({
      messages: [
        {
          mam_id: "mam-propose-video",
          id: "stanza-propose-video",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          timestamp: proposedAt,
          reaction_emojis: [],
          shared_files: [],
          call_event: {
            kind: "propose",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            sid: "call-archive-video",
            media: { audio: true, video: true },
          },
        },
        {
          mam_id: "mam-retract-video",
          id: "stanza-retract-video",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          timestamp: retractedAt,
          reaction_emojis: [],
          shared_files: [],
          call_event: {
            kind: "retract",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            sid: "call-archive-video",
          },
        },
      ],
      last_id: "mam-retract-video",
      is_complete: true,
    }));
    (client as unknown as { xmpp: Agent }).xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;

    const page = await client.queryPersonalMamPage("bob@example.com", 100, { type: "latest" });

    expect(page.messages).toHaveLength(1);
    expect(page.messages[0]).toMatchObject({
      id: "dmcall:call-archive-video:missed",
      archiveId: "mam-retract-video",
      wireIds: ["stanza-retract-video"],
      peerJid: "bob@example.com",
      threadId: "call-archive-video",
      callThread: {
        kind: "dm",
        sid: "call-archive-video",
        media: ["audio", "video"],
        outcome: "missed",
      },
    });
    expect($dmCallOutcomeAnchor.get()).toBeNull();
    expect((client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup.onSessionStarted()).toEqual([
      {
        kind: "dm",
        scope: "account",
        key: "bob@example.com",
        after: "mam-retract-video",
        since: retractedAt,
        seenIds: ["dmcall:call-archive-video:missed", "stanza-retract-video"],
      },
    ]);
  });

  test("queryPersonalMamPage returns archived DM call outcome cards on older pages", async () => {
    const client = new BrowserXmppClient(session());
    (client as unknown as { connect: () => Promise<void> }).connect = async () => undefined;
    (client as unknown as { connected: boolean }).connected = true;
    const proposedAt = new Date(Date.now() - 60_000).toISOString();
    const retractedAt = new Date(Date.now() - 30_000).toISOString();
    const fetchDmHistoryPage = mock(async () => ({
      messages: [
        {
          mam_id: "mam-before-propose-video",
          id: "stanza-before-propose-video",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          timestamp: proposedAt,
          reaction_emojis: [],
          shared_files: [],
          call_event: {
            kind: "propose",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            sid: "call-before-video",
            media: { audio: true, video: true },
          },
        },
        {
          mam_id: "mam-before-retract-video",
          id: "stanza-before-retract-video",
          from: "bob@example.com/phone",
          to: "alice@example.com/desktop",
          message_type: "chat",
          timestamp: retractedAt,
          reaction_emojis: [],
          shared_files: [],
          call_event: {
            kind: "retract",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            sid: "call-before-video",
          },
        },
      ],
      first_id: "mam-before-propose-video",
      last_id: "mam-before-retract-video",
      is_complete: false,
    }));
    (client as unknown as { xmpp: Agent }).xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_page: fetchDmHistoryPage,
    }) as unknown as Agent;

    const page = await client.queryPersonalMamPage("bob@example.com", 100, { type: "before", before: "mam-newer" });

    expect(page.messages).toHaveLength(1);
    expect(page.messages[0]).toMatchObject({
      id: "dmcall:call-before-video:missed",
      archiveId: "mam-before-retract-video",
      wireIds: ["stanza-before-retract-video"],
      callThread: {
        sid: "call-before-video",
        media: ["audio", "video"],
        outcome: "missed",
      },
    });
  });

  test("searchDmMessages ignores call events without mutating call activity", async () => {
    const client = new BrowserXmppClient(session());
    (client as unknown as { connected: boolean }).connected = true;
    applyDmCallEvent({
      selfBareJid: "alice@example.com",
      selfFullJid: "alice@example.com/desktop",
      timestamp: new Date().toISOString(),
      event: {
        kind: "propose",
        from: "bob@example.com/phone",
        to: "alice@example.com/desktop",
        sid: "call-existing-search",
        media: { audio: true, video: false },
      },
    });
    $dmCallOutcomeAnchor.set({
      peerBareJid: "bob@example.com",
      sid: "call-existing-outcome",
      media: { audio: true, video: false },
      outcome: "missed",
      initiator: "bob@example.com/phone",
      ended: "2024-01-01T00:00:00.000Z",
    });
    const existingActivity = readDmCallActivity("bob@example.com");
    const existingOutcome = $dmCallOutcomeAnchor.get();
    const searchDmHistory = mock(async () => ({
      messages: [
        dmWasmMessage({
          mam_id: "mam-search-propose",
          id: "search-propose",
          body: undefined,
          call_event: {
            kind: "propose",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            sid: "call-search",
            media: { audio: true, video: true },
          },
        }),
        dmWasmMessage({
          mam_id: "mam-search-body",
          id: "search-body",
          body: "matched text",
          timestamp: "2024-01-01T00:00:02.000Z",
        }),
      ],
      is_complete: true,
    }));
    (client as unknown as { xmpp: Agent }).xmpp = Object.assign(new EventEmitter(), {
      search_dm_history: searchDmHistory,
    }) as unknown as Agent;

    const results = await client.searchDmMessages("bob@example.com", "matched", 20);

    expect(results).toEqual([
      {
        id: "search-body",
        archiveId: "mam-search-body",
        nick: "bob",
        body: "matched text",
        createdAt: "2024-01-01T00:00:02.000Z",
        peerJid: "bob@example.com",
      },
    ]);
    expect(readDmCallActivity("bob@example.com")).toEqual(existingActivity);
    expect($dmCallOutcomeAnchor.get()).toEqual(existingOutcome);
  });

  test("queryMamThreadPage seeds room reconnect catch-up from XEP-0313 archive ids", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    (client as unknown as { connect: () => Promise<void> }).connect = async () => undefined;
    (client as unknown as { switchRoom: () => Promise<void> }).switchRoom = async () => undefined;
    (client as unknown as { connected: boolean }).connected = true;
    markReadyTestRoom(client, roomJid);
    (client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup.onSessionStarted();
    const fetchRoomHistoryByThread = mock(async () => ({
      messages: [roomWasmMessage({
        mam_id: "mam-thread-1",
        id: "room-thread-1",
        from: `${roomJid}/bob`,
        body: "thread history",
        timestamp: "2024-01-01T00:00:01.000Z",
        thread: "thread-1",
      })],
      last_id: "mam-thread-1",
      is_complete: true,
    }));
    (client as unknown as { xmpp: Agent }).xmpp = Object.assign(new EventEmitter(), {
      fetch_room_history_by_thread: fetchRoomHistoryByThread,
    }) as unknown as Agent;

    await client.queryMamThreadPage("w1", "c1", "thread-1", 100, { type: "latest" });

    expect(fetchRoomHistoryByThread).toHaveBeenCalledWith(roomJid, "thread-1", 100, null);
    expect((client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup.onSessionStarted()).toEqual([
      { kind: "room", key: roomJid, after: "mam-thread-1", since: "2024-01-01T00:00:01.000Z", seenIds: ["mam-thread-1", "room-thread-1"] },
    ]);
  });

  test("queryPersonalMamThreadPage ignores call events without mutating call activity", async () => {
    const client = new BrowserXmppClient(session());
    (client as unknown as { connected: boolean }).connected = true;
    applyDmCallEvent({
      selfBareJid: "alice@example.com",
      selfFullJid: "alice@example.com/desktop",
      timestamp: new Date().toISOString(),
      event: {
        kind: "propose",
        from: "bob@example.com/phone",
        to: "alice@example.com/desktop",
        sid: "call-existing-thread",
        media: { audio: true, video: false },
      },
    });
    $dmCallOutcomeAnchor.set({
      peerBareJid: "bob@example.com",
      sid: "call-existing-thread-outcome",
      media: { audio: true, video: false },
      outcome: "missed",
      initiator: "bob@example.com/phone",
      ended: "2024-01-01T00:00:00.000Z",
    });
    const existingActivity = readDmCallActivity("bob@example.com");
    const existingOutcome = $dmCallOutcomeAnchor.get();
    const fetchDmHistoryByThread = mock(async () => ({
      messages: [
        dmWasmMessage({
          mam_id: "mam-thread-call",
          id: "thread-call",
          body: undefined,
          thread: "call-thread",
          call_event: {
            kind: "propose",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            sid: "call-thread",
            media: { audio: true, video: true },
          },
        }),
      ],
      last_id: "mam-thread-call",
      is_complete: true,
    }));
    (client as unknown as { xmpp: Agent }).xmpp = Object.assign(new EventEmitter(), {
      fetch_dm_history_by_thread: fetchDmHistoryByThread,
    }) as unknown as Agent;

    const page = await client.queryPersonalMamThreadPage("bob@example.com", "call-thread", 100, { type: "latest" });

    expect(page.messages).toEqual([]);
    expect(fetchDmHistoryByThread).toHaveBeenCalledWith("bob@example.com", "call-thread", 100, null);
    expect(readDmCallActivity("bob@example.com")).toEqual(existingActivity);
    expect($dmCallOutcomeAnchor.get()).toEqual(existingOutcome);
  });

  test("queryMamByThread seeds room reconnect catch-up from XEP-0313 archive ids", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    (client as unknown as { connect: () => Promise<void> }).connect = async () => undefined;
    (client as unknown as { switchRoom: () => Promise<void> }).switchRoom = async () => undefined;
    (client as unknown as { connected: boolean }).connected = true;
    markReadyTestRoom(client, roomJid);
    (client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup.onSessionStarted();
    const fetchRoomHistoryByThread = mock(async () => ({
      messages: [roomWasmMessage({
        mam_id: "mam-thread-list-1",
        id: "room-thread-list-1",
        from: `${roomJid}/bob`,
        body: "thread list history",
        timestamp: "2024-01-01T00:00:01.000Z",
        thread: "thread-1",
      })],
      last_id: "mam-thread-list-1",
      is_complete: true,
    }));
    (client as unknown as { xmpp: Agent }).xmpp = Object.assign(new EventEmitter(), {
      fetch_room_history_by_thread: fetchRoomHistoryByThread,
    }) as unknown as Agent;

    await client.queryMamByThread("w1", "c1", "thread-1", 100);

    expect(fetchRoomHistoryByThread).toHaveBeenCalledWith(roomJid, "thread-1", 100, null);
    expect((client as unknown as { catchup: { onSessionStarted: () => unknown[] } }).catchup.onSessionStarted()).toEqual([
      { kind: "room", key: roomJid, after: "mam-thread-list-1", since: "2024-01-01T00:00:01.000Z", seenIds: ["mam-thread-list-1", "room-thread-list-1"] },
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
      { kind: "room", key: roomJid, after: "mam-room-newer", since: "2024-01-01T00:00:01.000Z", seenIds: ["mam-room-newer", "room-newer"] },
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
    expect(roomHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "mam-room-newer-2" }));
    expect(roomHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "mam-room-newer-1" }));
    expect(roomHandler).toHaveBeenCalledWith(expect.objectContaining({ id: "mam-room-same-time-missed" }));
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
    markReadyTestRoom(client, roomJid);
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
      { kind: "room", key: roomJid, after: "mam-room-1", since: "2024-01-01T00:00:00.000Z", seenIds: ["mam-room-1", "room-1"] },
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
      id: "mam-room-2",
      body: "first missed room page",
      roomJid,
    }));
    expect(roomHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "mam-room-3",
      body: "second missed room page",
      roomJid,
    }));
  });

  test("tracked room catch-up rejects non-advancing after pages before activity delivery", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const catchup = (client as unknown as {
      catchup: {
        recordRoomSeen: (room: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordRoomSeen(roomJid, "2024-01-01T00:00:00.000Z", "mam-room-1", ["room-1"]);
    catchup.onSessionStarted();
    const activityHandler = mock(() => undefined);
    const errorHandler = mock(() => undefined);
    client.setActivityHandler(activityHandler);
    client.onError(errorHandler);
    const fetchRoomHistoryPage = mock(async () => ({
      messages: [roomWasmMessage({
        mam_id: "mam-room-1",
        id: "room-duplicate-cursor",
        from: `${roomJid}/bob`,
        body: "duplicate room cursor row",
        timestamp: "2024-01-01T00:00:00.000Z",
      })],
      last_id: "mam-room-1",
      is_complete: false,
    }));
    const xmpp = Object.assign(new EventEmitter(), {
      fetch_room_history_page: fetchRoomHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchRoomHistoryPage).toHaveBeenCalledTimes(1);
    expect(fetchRoomHistoryPage).toHaveBeenCalledWith(roomJid, 100, {
      type: "after",
      after: "mam-room-1",
    });
    expect(activityHandler).not.toHaveBeenCalled();
    expect(errorHandler).toHaveBeenCalledWith(expect.objectContaining({
      kind: "history",
      recoverable: true,
      detail: `Reconnect catch-up failed for ${roomJid}`,
    }));
    expect(normalizeError(errorHandler.mock.calls[0]?.[0]?.cause)).toContain("non-advancing archive cursor");
  });

  test("tracked room catch-up falls back to timestamp paging when after cursor is gone", async () => {
    const client = new BrowserXmppClient(session());
    const roomJid = roomBareJidFor(session(), "c1");
    const catchup = (client as unknown as {
      catchup: {
        recordRoomSeen: (room: string, ts: string, archiveId?: string, seenIds?: string[]) => void;
        onSessionStarted: () => unknown[];
      };
    }).catchup;
    catchup.recordRoomSeen(roomJid, "2024-01-01T00:00:00.000Z", "mam-room-stale", ["room-seen"]);
    catchup.recordRoomSeen(roomJid, "2024-01-01T00:00:01.500Z", undefined, ["room-live-already-seen-1"]);
    catchup.recordRoomSeen(roomJid, "2024-01-01T00:00:02.000Z", undefined, ["room-live-already-seen-2"]);
    catchup.onSessionStarted();
    const activityHandler = mock(() => undefined);
    const errorHandler = mock(() => undefined);
    client.setActivityHandler(activityHandler);
    client.onError(errorHandler);
    const fetchRoomHistoryPage = mock(async (_room: string, _max: number, pageParam: { type: string }) => {
      if (pageParam.type === "after") throw new Error("stanza error: item-not-found");
      return {
        messages: [
          roomWasmMessage({
            mam_id: "mam-room-seen-sibling",
            id: "room-seen",
            from: `${roomJid}/bob`,
            body: "already seen at stale archive timestamp",
            timestamp: "2024-01-01T00:00:00.000Z",
          }),
          roomWasmMessage({
            mam_id: "mam-room-recovered",
            id: "room-recovered",
            from: `${roomJid}/bob`,
            body: "recovered room activity",
            timestamp: "2024-01-01T00:00:01.000Z",
          }),
          roomWasmMessage({
            mam_id: "mam-room-live-already-seen-1",
            id: "room-live-already-seen-1",
            from: `${roomJid}/bob`,
            body: "already delivered live before latest watermark",
            timestamp: "2024-01-01T00:00:01.500Z",
          }),
          roomWasmMessage({
            mam_id: "mam-room-live-already-seen-2",
            id: "room-live-already-seen-2",
            from: `${roomJid}/bob`,
            body: "already delivered live at latest watermark",
            timestamp: "2024-01-01T00:00:02.000Z",
          }),
        ],
        first_id: "mam-room-seen-sibling",
        last_id: "mam-room-live-already-seen-2",
        is_complete: true,
      };
    });
    const xmpp = Object.assign(new EventEmitter(), {
      fetch_room_history_page: fetchRoomHistoryPage,
    }) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("stream:management:resumed");
    await settleReconnectCatchup();

    expect(fetchRoomHistoryPage).toHaveBeenNthCalledWith(1, roomJid, 100, {
      type: "after",
      after: "mam-room-stale",
    });
    expect(fetchRoomHistoryPage).toHaveBeenNthCalledWith(2, roomJid, 100, { type: "latest" });
    expect(errorHandler).not.toHaveBeenCalled();
    expect(activityHandler).toHaveBeenCalledTimes(1);
    expect(activityHandler).toHaveBeenCalledWith(expect.objectContaining({
      roomJid,
      body: "recovered room activity",
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
        // Catch-up re-emissions are flagged so unread/mention/notification
        // accounting stays idempotent (the server inbox accounts
        // genuinely-missed messages authoritatively).
        fromArchive: true,
      },
    ]);
  });
});

describe("carbon forwarding", () => {
  test("delivers carbon-marked received messages to the DM handler (#1243)", () => {
    // The WASM core unwraps the XEP-0280 envelope (after the §11 own-
    // bare-JID check) and surfaces the INNER message with a `carbon`
    // marker — it must render like any live DM, keyed by the peer.
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("message", {
      id: "c-recv-render",
      type: "chat",
      from: "bob@example.com/phone",
      to: "alice@example.com/tablet",
      body: "carbon copy renders live",
      carbon: { sent: false, received: true },
    });

    expect(dmHandler).toHaveBeenCalledTimes(1);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      peerJid: "bob@example.com",
      body: "carbon copy renders live",
    }));
  });

  test("applies carbon-wrapped call activity on generic message events", () => {
    const client = new BrowserXmppClient(session());
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("message", {
      id: "call-carbon-1",
      type: "chat",
      from: "alice@example.com/phone",
      to: "bob@example.com/desktop",
      timestamp: new Date().toISOString(),
      carbon: { sent: true },
      call_event: {
        kind: "propose",
        from: "alice@example.com/phone",
        to: "bob@example.com/desktop",
        sid: "call-carbon-1",
        media: { audio: true, video: true },
      },
    });

    expect(readDmCallActivity("bob@example.com")).toMatchObject({
      sid: "call-carbon-1",
      direction: "outgoing",
      media: { audio: true, video: true },
    });
  });

  test("applies call activity from carbon-marked sent messages", () => {
    const client = new BrowserXmppClient(session());
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    // #1243: the WASM core unwraps the carbon envelope and delivers the
    // inner message on the normal path with a `carbon` direction marker.
    xmpp.emit("message", {
      id: "call-carbon-sent",
      type: "chat",
      from: "alice@example.com/phone",
      to: "bob@example.com/desktop",
      timestamp: new Date().toISOString(),
      carbon: { sent: true, received: false },
      call_event: {
        kind: "propose",
        from: "alice@example.com/phone",
        to: "bob@example.com/desktop",
        sid: "call-carbon-sent",
        media: { audio: true, video: true },
      },
    });

    expect(readDmCallActivity("bob@example.com")).toMatchObject({
      sid: "call-carbon-sent",
      direction: "outgoing",
      media: { audio: true, video: true },
    });
  });

  test("deduplicates carbon-marked call activity against a duplicate direct delivery", () => {
    const client = new BrowserXmppClient(session());
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    const forwarded = {
      id: "call-carbon-received",
      type: "chat",
      from: "bob@example.com/phone",
      to: "alice@example.com/desktop",
      timestamp: new Date().toISOString(),
      call_event: {
        kind: "propose",
        from: "bob@example.com/phone",
        sid: "call-carbon-received",
        media: { audio: true, video: false },
      },
    };
    xmpp.emit("message", { ...forwarded, carbon: { sent: false, received: true } });
    xmpp.emit("message", {
      ...forwarded,
      call_event: {
        kind: "finish",
        from: "bob@example.com/phone",
        sid: "call-carbon-received",
        reason: "success",
      },
    });

    expect(readDmCallActivity("bob@example.com")).toMatchObject({
      sid: "call-carbon-received",
      direction: "incoming",
      media: { audio: true, video: false },
    });
  });

  test("forwards carbon-marked sent messages to the DM handler", () => {
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("message", {
      id: "c-sent-1",
      type: "chat",
      from: "alice@example.com/phone",
      to: "bob@example.com/desktop",
      body: "hello from sibling sender",
      carbon: { sent: true, received: false },
    });

    expect(dmHandler).toHaveBeenCalledTimes(1);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      id: "c-sent-1",
      peerJid: "bob@example.com",
      body: "hello from sibling sender",
    }));
  });

  test("an SM-replayed carbon copy dispatches only once", () => {
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    const carbonCopy = {
      id: "c-replay-1",
      type: "chat",
      from: "bob@example.com/phone",
      to: "alice@example.com/tablet",
      body: "replayed after resume",
      carbon: { sent: false, received: true },
    };
    xmpp.emit("message", carbonCopy);
    // XEP-0198 resume replays the unacked carbon verbatim.
    xmpp.emit("message", carbonCopy);

    expect(dmHandler).toHaveBeenCalledTimes(1);
  });

  test("carbon dedupe is sender-scoped: same id from another sender still delivers", () => {
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("message", {
      id: "1",
      type: "chat",
      from: "bob@example.com/phone",
      to: "alice@example.com/tablet",
      body: "carbon from bob",
      carbon: { sent: false, received: true },
    });
    // Stanza ids are only unique per sender — Carol's unrelated message
    // sharing the id must NOT be swallowed by Bob's carbon entry.
    xmpp.emit("message", {
      id: "1",
      type: "chat",
      from: "carol@example.com/desktop",
      to: "alice@example.com/tablet",
      body: "unrelated from carol",
    });

    expect(dmHandler).toHaveBeenCalledTimes(2);
  });

  test("collapses the pair when the direct delivery arrives BEFORE the carbon", () => {
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    const direct = {
      id: "d-first-1",
      type: "chat",
      from: "bob@example.com/phone",
      to: "alice@example.com/tablet",
      body: "direct beats carbon",
    };
    xmpp.emit("message", direct);
    xmpp.emit("message", { ...direct, carbon: { sent: false, received: true } });

    expect(dmHandler).toHaveBeenCalledTimes(1);
  });

  test("a delay-carrying carbon after a direct delivery passes through to re-stamp the row", () => {
    // #1267 item 6 / Greptile P1: the direct copy rendered with a live
    // fallback timestamp; the carbon's forwarded <delay/> is the only
    // authoritative stamp, so it must reach the merge layer (which
    // collapses by wire id) instead of being dropped by the dedupe.
    const client = new BrowserXmppClient(session());
    const received: Array<{ createdAtSource?: string }> = [];
    client.setDirectMessageHandler((msg) => received.push(msg));
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    const direct = {
      id: "d-delay-1",
      type: "chat",
      from: "bob@example.com/phone",
      to: "alice@example.com/tablet",
      body: "needs a real stamp",
    };
    xmpp.emit("message", direct);
    xmpp.emit("message", {
      ...direct,
      timestamp: "2026-07-01T10:00:00.000Z",
      carbon: { sent: false, received: true },
    });
    // A replayed copy of that carbon still drops.
    xmpp.emit("message", {
      ...direct,
      timestamp: "2026-07-01T10:00:00.000Z",
      carbon: { sent: false, received: true },
    });

    expect(received).toHaveLength(2);
    expect(received[0]?.createdAtSource).toBe("fallback");
    expect(received[1]?.createdAtSource).toBe("delay");
    // The pass-through is restamp-only: unread accounting and
    // notifications must skip it (the direct copy already counted).
    expect((received[0] as { timestampRefreshOnly?: boolean }).timestampRefreshOnly).toBeUndefined();
    expect((received[1] as { timestampRefreshOnly?: boolean }).timestampRefreshOnly).toBe(true);
  });

  test("carbon dedupe keys MUC-PM senders by the full occupant JID (#1256)", () => {
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    (client as unknown as { retainedJoinedRoomJids: Set<string> }).retainedJoinedRoomJids =
      new Set(["room@muc.example.com"]);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    // Two occupants of the SAME room pick the same sender-chosen stanza
    // id; a bare-folded dedupe key would let juliet's PM swallow iago's.
    xmpp.emit("message", {
      id: "pm-id",
      type: "chat",
      from: "room@muc.example.com/juliet",
      to: "alice@example.com/desktop",
      body: "from juliet",
    });
    xmpp.emit("message", {
      id: "pm-id",
      type: "chat",
      from: "room@muc.example.com/iago",
      to: "alice@example.com/desktop",
      body: "from iago",
    });

    expect(dmHandler).toHaveBeenCalledTimes(2);
  });

  test("a replayed carbon after its direct duplicate was dropped still dedupes", () => {
    // Qodo: deleting the dedupe entry when dropping the paired direct
    // delivery let an SM-replayed carbon look unseen and dispatch again.
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    const carbonCopy = {
      id: "c-replay-2",
      type: "chat",
      from: "bob@example.com/phone",
      to: "alice@example.com/tablet",
      body: "carbon → direct dup → carbon replay",
      carbon: { sent: false, received: true },
    };
    xmpp.emit("message", carbonCopy);
    xmpp.emit("message", { ...carbonCopy, carbon: undefined }); // duplicate direct delivery
    xmpp.emit("message", carbonCopy); // XEP-0198 replay of the carbon

    expect(dmHandler).toHaveBeenCalledTimes(1);
  });

  test("drops carbon-sent chat states and displayed markers (own activity elsewhere)", () => {
    const client = new BrowserXmppClient(session());
    const chatStateHandler = mock(() => undefined);
    const displayedHandler = mock(() => undefined);
    client.setDmChatStateHandler(chatStateHandler);
    client.setDmDisplayedHandler(displayedHandler);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    // Our own typing indicator on another device must not surface as
    // peer state.
    xmpp.emit("message", {
      type: "chat",
      from: "alice@example.com/phone",
      to: "bob@example.com",
      chat_state: "composing",
      carbon: { sent: true, received: false },
    });
    xmpp.emit("message", {
      id: "marker-1",
      type: "chat",
      from: "alice@example.com/phone",
      to: "bob@example.com",
      displayed_marker_id: "some-message",
      carbon: { sent: true, received: false },
    });
    expect(chatStateHandler).not.toHaveBeenCalled();
    expect(displayedHandler).not.toHaveBeenCalled();

    // A carbon-RECEIVED chat state is the peer typing to another of our
    // devices — same conversation, so it does surface.
    xmpp.emit("message", {
      type: "chat",
      from: "bob@example.com/desktop",
      to: "alice@example.com/phone",
      chat_state: "composing",
      carbon: { sent: false, received: true },
    });
    expect(chatStateHandler).toHaveBeenCalledWith(expect.objectContaining({
      peerJid: "bob@example.com",
      state: "composing",
    }));
  });

  test("files MUC private messages under the full occupant JID (#1256)", () => {
    const client = new BrowserXmppClient(session());
    const dmHandler = mock(() => undefined);
    client.setDirectMessageHandler(dmHandler);
    (client as unknown as { retainedJoinedRoomJids: Set<string> }).retainedJoinedRoomJids =
      new Set(["room@muc.example.com"]);
    const xmpp = Object.assign(new EventEmitter(), {}) as unknown as Agent;
    (client as unknown as { xmpp: Agent }).xmpp = xmpp;
    (client as unknown as { wireEvents: (xmpp: Agent) => void }).wireEvents(xmpp);

    xmpp.emit("message", {
      id: "muc-pm-1",
      type: "chat",
      from: "room@muc.example.com/juliet",
      to: "alice@example.com/desktop",
      body: "psst — occupant to occupant",
    });

    expect(dmHandler).toHaveBeenCalledTimes(1);
    expect(dmHandler).toHaveBeenCalledWith(expect.objectContaining({
      // XEP-0045 §7.5: conversation identity = occupant JID, so replies
      // address room@service/nick, never the room bare JID (broadcast).
      peerJid: "room@muc.example.com/juliet",
      mucPm: true,
      nick: "juliet",
      body: "psst — occupant to occupant",
    }));
  });

  test("does not double-process a carbon copy plus a duplicate direct delivery", () => {
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
    // The unwrapped carbon copy and a direct delivery of the same
    // stanza (bare-JID fan-out race) must collapse to one dispatch.
    xmpp.emit("message", { ...forwarded, carbon: { sent: false, received: true } });
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

  test("accepts snake_case inbox pushes from the WASM message callback", () => {
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
      inbox_push: {
        partner: "space_channel@muc.example.com",
        kind: "muc",
        last_stanza_id: "sid-live",
        last_updated: 1_700_001,
        unread: 1,
        preview: "live unread",
      },
    });

    expect(inboxEntries).toEqual([
      {
        partner: "space_channel@muc.example.com",
        kind: "muc",
        lastStanzaId: "sid-live",
        lastUpdated: 1_700_001,
        unread: 1,
        preview: "live unread",
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
