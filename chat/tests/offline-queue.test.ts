import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
import { useDirectMessages } from "../src/dms/messages";
import { useChannelMessages } from "../src/channels/messages";
import { BrowserXmppClient, roomBareJidFor } from "../src/lib/xmpp-client";
import {
  enqueueQueuedMessage,
  listQueuedDmMessages,
  listQueuedRoomMessages,
} from "../src/lib/outbound-queue-store";
import {
  committedOrThrow,
  MemoryDurableOutboundStore,
  type OutboundOwnerContext,
  type OutboundTerminalIntent,
} from "../src/lib/xmpp-runtime-durable-store";
import { WasmClientCallbackDouble } from "./helpers/wasm-client-callbacks";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
    ...partial,
  } as WaddleSession;
}

function wasmSent(stanzaId: string | undefined) {
  if (!stanzaId) throw new Error("test send is missing stanza_id");
  return { kind: "sent" as const, stanza_id: stanzaId };
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
    key(index: number) {
      return [...values.keys()][index] ?? null;
    },
    get length() {
      return values.size;
    },
  };
}

async function waitForOutboundHydration(client: BrowserXmppClient): Promise<void> {
  if (!createdClients.includes(client)) createdClients.push(client);
  await (client as unknown as { outboundQueueHydration: Promise<void> }).outboundQueueHydration;
}

async function bindReplayGeneration(
  client: BrowserXmppClient,
  generation = 1,
): Promise<void> {
  await waitForOutboundHydration(client);
  const state = client as unknown as {
    connectEpoch: number;
    outboundQueue: {
      beginConnectionGeneration(generation: number): number;
    };
  };
  state.connectEpoch = generation;
  expect(state.outboundQueue.beginConnectionGeneration(generation)).toBe(generation);
}

type TerminalExecution = {
  executor: OutboundOwnerContext;
  intent: OutboundTerminalIntent;
};

function observeTerminalExecutions(
  store: MemoryDurableOutboundStore,
): TerminalExecution[] {
  const executions: TerminalExecution[] = [];
  const applyTerminal = store.applyTerminal.bind(store);
  store.applyTerminal = async (executor, intent) => {
    executions.push({
      executor: structuredClone(executor),
      intent: structuredClone(intent),
    });
    return applyTerminal(executor, intent);
  };
  return executions;
}

function expectExactTerminalExecution(
  execution: TerminalExecution | undefined,
  messageId: string,
  connectionGeneration: number,
): void {
  if (!execution) throw new Error(`missing terminal execution for ${messageId}`);
  expect(execution.intent.identity.messageId).toBe(messageId);
  expect(execution.intent.expected.connectionGeneration).toBe(connectionGeneration);
  expect(execution.executor).toEqual({
    accountKey: execution.intent.expected.accountKey,
    ownerId: execution.intent.expected.ownerId,
    ownerInstanceId: execution.intent.expected.ownerInstanceId,
    ownerGeneration: execution.intent.expected.ownerGeneration,
    authorityEpoch: execution.intent.expected.authorityEpoch,
  });
}

function nextMessageAck(client: BrowserXmppClient, stanzaId: string): Promise<void> {
  return new Promise((resolve) => {
    client.setMessageAckHandler((ackedId) => {
      if (ackedId === stanzaId) resolve();
    });
  });
}

const originalWindow = globalThis.window;
const originalLocalStorage = globalThis.localStorage;
let createdClients: BrowserXmppClient[] = [];

beforeEach(() => {
  createdClients = [];
  const storage = createStorageMock();
  (globalThis as typeof globalThis & { localStorage: typeof storage }).localStorage = storage;
  (globalThis as typeof globalThis & { window: Window & { localStorage: typeof storage } }).window = {
    ...(originalWindow ?? {}),
    localStorage: storage,
  } as Window & { localStorage: typeof storage };
  localStorage.clear();
});

afterEach(async () => {
  for (const client of createdClients) {
    const state = client as unknown as {
      xmpp: null;
      connected: boolean;
    };
    state.xmpp = null;
    state.connected = false;
  }
  await Promise.all(createdClients.map((client) => client.dispose()));
  createdClients = [];
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

describe("offline outbound queue replay", () => {
  test("does not persist decodable link preview tokens for queued sends", async () => {
    const durableStore = new MemoryDurableOutboundStore();
    const client = new BrowserXmppClient(session(), {
      durableRuntimeStore: durableStore,
    });
    await bindReplayGeneration(client);
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => {
      throw new Error("Reconnection timed out");
    });

    await client.sendDirectMessage("bob@example.com", "read https://example.com", {
      id: "dm-preview-queued",
      linkPreviewToken: "plaintext-token",
      linkPreviewExpiresAt: "2999-01-01T00:00:00.000Z",
    });

    const raw = JSON.stringify(committedOrThrow(
      "test-list-preview-queue",
      await durableStore.list("alice@example.com"),
    ));
    expect(raw).toContain("dm-preview-queued");
    expect(raw).not.toContain("plaintext-token");
    expect(raw).not.toContain("linkPreviewToken");
    expect(raw).not.toContain("linkPreviewExpiresAt");
  });

  test("does not reacquire link preview metadata when replaying queued URL sends", async () => {
    const durableStore = new MemoryDurableOutboundStore();
    committedOrThrow("test-seed-preview-replay", await durableStore.persistReady("alice@example.com", {
      kind: "dm",
      id: "dm-preview-replay",
      createdAt: new Date().toISOString(),
      peerJid: " Bob@Example.COM/desktop ",
      body: "read https://example.com/article",
    }));
    const client = new BrowserXmppClient(session(), {
      durableRuntimeStore: durableStore,
    });
    await bindReplayGeneration(client);

    const xmpp = {
      send_raw_iq: mock(async () => {
        throw new Error("queued replay must not perform composer preview lookup");
      }),
      send_chat_message: mock(async (_peer: string, _body: string, opts: { stanza_id?: string }) => wasmSent(opts.stanza_id)),
      on() {},
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).connected = true;

    await (client as unknown as { flushQueuedDirectMessages: () => Promise<void> }).flushQueuedDirectMessages();

    expect(xmpp.send_raw_iq).not.toHaveBeenCalled();
    expect((xmpp.send_chat_message.mock.calls[0]?.[2] as { link_preview_token?: string }).link_preview_token)
      .toBeUndefined();
  });

  test("replays queued DM messages in order when the session returns", async () => {
    const durableStore = new MemoryDurableOutboundStore();
    const client = new BrowserXmppClient(session(), {
      durableRuntimeStore: durableStore,
    });
    await bindReplayGeneration(client);
    const terminalExecutions = observeTerminalExecutions(durableStore);
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => {
      throw new Error("Reconnection timed out");
    });

    await client.sendDirectMessage("bob@example.com", "first", { id: "dm-1" });
    await client.sendDirectMessage("bob@example.com", "second", { id: "dm-2" });
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account").map((message) => message.id)).toEqual([
      "dm-1",
      "dm-2",
    ]);

    // The generated XMPP callback carries delivery acks into `wireEvents`,
    // which drives removal from the persisted queue. The test double keeps
    // Closed typed callback controls mirror the generated binding exactly.
    const xmpp = Object.assign(new WasmClientCallbackDouble(), {
      send_chat_message: mock(async (_peer: string, _body: string, opts: { stanza_id?: string }) => wasmSent(opts.stanza_id)),
    });
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).connected = true;
    // Wire the ack handler the same way wireEvents would.
    (client as unknown as { wireEvents: (x: typeof xmpp) => void }).wireEvents(xmpp);

    const statuses: Array<{ id: string; status: string }> = [];
    client.setQueuedMessageStatusHandler((id, status) => {
      statuses.push({ id, status });
    });

    await (client as unknown as { flushQueuedDirectMessages: () => Promise<void> }).flushQueuedDirectMessages();

    expect(statuses).toEqual([{ id: "dm-1", status: "sending" }]);
    expect(xmpp.send_chat_message.mock.calls.map((call) => (call[2] as { stanza_id?: string }).stanza_id)).toEqual([
      "dm-1",
    ]);
    // Post-fix: persisted queue entries linger until XEP-0198
    // `message:acked` confirms the server received them.
    expect(
      listQueuedDmMessages("alice@example.com", "bob@example.com", "account").map((message) => message.id),
    ).toEqual(["dm-1", "dm-2"]);

    // A second flush in the same session must NOT re-send: both entries
    // are already inflight (handed to the XMPP client, ack pending).
    xmpp.send_chat_message.mockClear();
    await (client as unknown as { flushQueuedDirectMessages: () => Promise<void> }).flushQueuedDirectMessages();
    expect(xmpp.send_chat_message.mock.calls).toEqual([]);

    // Now simulate the server acking dm-1. The exact durable terminal commit
    // advances the one-head direct lane and makes dm-2 eligible.
    const firstAck = nextMessageAck(client, "dm-1");
    xmpp.emitMessageDeliveryAcked("dm-1");
    await firstAck;
    expectExactTerminalExecution(terminalExecutions[0], "dm-1", 1);
    await (client as unknown as { flushQueuedDirectMessages: () => Promise<void> }).flushQueuedDirectMessages();
    expect(
      listQueuedDmMessages("alice@example.com", "bob@example.com", "account").map((message) => message.id),
    ).toEqual(["dm-2"]);
    expect(xmpp.send_chat_message.mock.calls.map((call) => (call[2] as { stanza_id?: string }).stanza_id))
      .toEqual(["dm-2"]);
    expect(statuses).toEqual([
      { id: "dm-1", status: "sending" },
      { id: "dm-2", status: "sending" },
    ]);

    const secondAck = nextMessageAck(client, "dm-2");
    xmpp.emitMessageDeliveryAcked("dm-2");
    await secondAck;
    expectExactTerminalExecution(terminalExecutions[1], "dm-2", 1);
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com", "account")).toEqual([]);
  });

  test("replays queued room messages in order after the room rejoins", async () => {
    const durableStore = new MemoryDurableOutboundStore();
    const client = new BrowserXmppClient(session(), {
      durableRuntimeStore: durableStore,
    });
    await bindReplayGeneration(client);
    const terminalExecutions = observeTerminalExecutions(durableStore);
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => {
      throw new Error("Reconnection timed out");
    });
    (client as unknown as { switchRoom: ReturnType<typeof mock> }).switchRoom = mock(async () => {
      throw new Error("Reconnection timed out");
    });

    await client.sendGroupMessage("w1", "c1", "first https://example.com/room", { id: "room-1" });
    await client.sendGroupMessage("w1", "c1", "second", { id: "room-2" });

    const roomJid = roomBareJidFor(session(), "c1");
    expect(listQueuedRoomMessages("alice@example.com", roomJid).map((message) => message.id)).toEqual([
      "room-1",
      "room-2",
    ]);

    const xmpp = Object.assign(new WasmClientCallbackDouble(), {
      send_raw_iq: mock(async () => {
        throw new Error("queued replay must not perform composer preview lookup");
      }),
      send_groupchat_message: mock(async (_room: string, _body: string, opts: { stanza_id?: string }) => wasmSent(opts.stanza_id)),
    });
    (client as unknown as { xmpp: typeof xmpp; connected: boolean; currentRoom: string | null }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean; currentRoom: string | null }).connected = true;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean; currentRoom: string | null }).currentRoom =
      roomJid;
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);
    (client as unknown as { wireEvents: (x: typeof xmpp) => void }).wireEvents(xmpp);

    await (client as unknown as { flushQueuedRoomMessages: (roomJid: string) => Promise<void> }).flushQueuedRoomMessages(roomJid);

    expect(xmpp.send_raw_iq).not.toHaveBeenCalled();
    expect(xmpp.send_groupchat_message.mock.calls.map((call) => (call[2] as { stanza_id?: string }).stanza_id)).toEqual([
      "room-1",
    ]);
    expect((xmpp.send_groupchat_message.mock.calls[0]?.[2] as { link_preview_token?: string }).link_preview_token)
      .toBeUndefined();
    // Persisted entries stay until ack, same as the DM path.
    expect(
      listQueuedRoomMessages("alice@example.com", roomJid).map((message) => message.id),
    ).toEqual(["room-1", "room-2"]);

    const firstAck = nextMessageAck(client, "room-1");
    xmpp.emitMessageDeliveryAcked("room-1");
    await firstAck;
    expectExactTerminalExecution(terminalExecutions[0], "room-1", 1);
    await (client as unknown as { flushQueuedRoomMessages: (roomJid: string) => Promise<void> })
      .flushQueuedRoomMessages(roomJid);
    expect(xmpp.send_groupchat_message.mock.calls.map((call) => (call[2] as { stanza_id?: string }).stanza_id))
      .toEqual(["room-1", "room-2"]);
    expect((xmpp.send_groupchat_message.mock.calls[1]?.[2] as { link_preview_token?: string }).link_preview_token)
      .toBeUndefined();
    const secondAck = nextMessageAck(client, "room-2");
    xmpp.emitMessageDeliveryAcked("room-2");
    await secondAck;
    expectExactTerminalExecution(terminalExecutions[1], "room-2", 1);
    expect(listQueuedRoomMessages("alice@example.com", roomJid)).toEqual([]);
  });
});

describe("offline outbound queue hydration", () => {
  test("ambiguous resource-bearing legacy rows fail closed for every DM scope", () => {
    localStorage.setItem(
      "waddle.chat.outbound-queue.alice@example.com",
      JSON.stringify([
        {
          kind: "dm",
          id: "legacy-unscoped-occupant",
          createdAt: new Date().toISOString(),
          peerJid: "room@conference.example/alice",
          body: "ambiguous",
        },
      ]),
    );

    expect(
      listQueuedDmMessages("alice@example.com", "room@conference.example", "account"),
    ).toEqual([]);
    expect(
      listQueuedDmMessages(
        "alice@example.com",
        "room@conference.example/alice",
        "muc-occupant",
      ),
    ).toEqual([]);
  });

  test("room timelines restore queued messages from localStorage", async () => {
    const roomJid = roomBareJidFor(session(), "c1");
    enqueueQueuedMessage("alice@example.com", {
      kind: "room",
      id: "queued-room-1",
      // Recent stamp — the queue store now prunes entries older
      // than 7 days on read (PR4), so a fixed 2024 date would be
      // dropped by the time these tests run on a wall clock past
      // that window.
      createdAt: new Date().toISOString(),
      roomJid,
      body: "hello from storage",
    });

    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
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

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0]).toMatchObject({
      id: "queued-room-1",
      body: "hello from storage",
      deliveryStatus: "queued",
    });
  });

  test("DM timelines restore queued messages from localStorage", async () => {
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "queued-dm-1",
      // See `room timelines restore queued messages from localStorage` —
      // the queue store now prunes entries older than 7 days.
      createdAt: new Date().toISOString(),
      peerJid: " Bob@Example.COM/desktop ",
      body: "hello from storage",
    });

    const actionError = ref("");
    const messaging = useDirectMessages(
      ref(session()),
      ref(null),
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("bob@example.com");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0]).toMatchObject({
      id: "queued-dm-1",
      body: "hello from storage",
      deliveryStatus: "queued",
    });
  });

  test("MUC-PM timelines restore only the full occupant's queued messages", async () => {
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "queued-muc-pm-alice",
      createdAt: new Date().toISOString(),
      peerJid: " Room@Conference.Example/alice ",
      mucPm: true,
      body: "private hello",
    });
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "queued-muc-pm-bob",
      createdAt: new Date().toISOString(),
      peerJid: "room@conference.example/bob",
      mucPm: true,
      body: "other occupant",
    });

    const actionError = ref("");
    const messaging = useDirectMessages(
      ref(session()),
      ref(null),
      ref("room@conference.example/alice"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
      ref("muc-occupant"),
    );

    await messaging.loadMessages("room@conference.example/alice");

    expect(messaging.messages.value.map((message) => message.id)).toEqual([
      "queued-muc-pm-alice",
    ]);
  });
});
