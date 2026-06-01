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
import { withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
    ...partial,
  } as WaddleSession;
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

describe("offline outbound queue replay", () => {
  test("does not persist decodable link preview tokens for queued sends", async () => {
    const client = new BrowserXmppClient(session());
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => {
      throw new Error("Reconnection timed out");
    });

    await client.sendDirectMessage("bob@example.com", "read https://example.com", {
      id: "dm-preview-queued",
      linkPreviewToken: "plaintext-token",
      linkPreviewExpiresAt: "2026-06-01T12:05:00.000Z",
    });

    const raw = localStorage.getItem("waddle.chat.outbound-queue.alice@example.com");
    expect(raw).toContain("dm-preview-queued");
    expect(raw).not.toContain("plaintext-token");
    expect(raw).not.toContain("linkPreviewToken");
    expect(raw).not.toContain("linkPreviewExpiresAt");
  });

  test("reacquires a fresh link preview token when replaying queued URL sends", async () => {
    const client = new BrowserXmppClient(session());
    enqueueQueuedMessage("alice@example.com", {
      kind: "dm",
      id: "dm-preview-replay",
      createdAt: new Date().toISOString(),
      peerJid: "bob@example.com",
      body: "read https://example.com/article",
    });

    const xmpp = {
      send_raw_iq: mock(async () =>
        `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="fresh-token" original-url="https://example.com/article" normalized-url="https://example.com/article" expires-at="2026-06-01T12:05:00.000Z"><title>Example</title></preview></lookup></iq>`),
      send_chat_message: mock(async (_peer: string, _body: string, opts: { stanza_id?: string }) => opts.stanza_id),
      on() {},
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).connected = true;

    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        await (client as unknown as { flushQueuedDirectMessages: () => Promise<void> }).flushQueuedDirectMessages();
      });
    });

    expect(xmpp.send_raw_iq).toHaveBeenCalled();
    expect((xmpp.send_chat_message.mock.calls[0]?.[2] as { link_preview_token?: string }).link_preview_token)
      .toBe("fresh-token");
  });

  test("replays queued DM messages in order when the session returns", async () => {
    const client = new BrowserXmppClient(session());
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => {
      throw new Error("Reconnection timed out");
    });

    await client.sendDirectMessage("bob@example.com", "first", { id: "dm-1" });
    await client.sendDirectMessage("bob@example.com", "second", { id: "dm-2" });
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com").map((message) => message.id)).toEqual([
      "dm-1",
      "dm-2",
    ]);

    // The XMPP client emits ack events; the BrowserXmppClient
    // wires "message:acked" in `wireEvents`, and that's what drives
    // removal from the persisted queue post-fix. For the flush test we
    // drive the Rust send method directly and simulate the ack afterwards.
    const handlers = new Map<string, Array<(payload: unknown) => void>>();
    const xmpp = {
      send_chat_message: mock(async (_peer: string, _body: string, opts: { stanza_id?: string }) => opts.stanza_id),
      on(event: string, handler: (payload: unknown) => void) {
        const list = handlers.get(event) ?? [];
        list.push(handler);
        handlers.set(event, list);
      },
      emit(event: string, payload: unknown) {
        for (const handler of handlers.get(event) ?? []) {
          handler(payload);
        }
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).connected = true;
    // Wire the ack handler the same way wireEvents would.
    (client as unknown as { wireEvents: (x: typeof xmpp) => void }).wireEvents(xmpp);

    const statuses: Array<{ id: string; status: string }> = [];
    client.setQueuedMessageStatusHandler((id, status) => {
      statuses.push({ id, status });
    });

    await (client as unknown as { flushQueuedDirectMessages: () => Promise<void> }).flushQueuedDirectMessages();

    expect(statuses).toEqual([
      { id: "dm-1", status: "sending" },
      { id: "dm-2", status: "sending" },
    ]);
    expect(xmpp.send_chat_message.mock.calls.map((call) => (call[2] as { stanza_id?: string }).stanza_id)).toEqual([
      "dm-1",
      "dm-2",
    ]);
    // Post-fix: persisted queue entries linger until XEP-0198
    // `message:acked` confirms the server received them.
    expect(
      listQueuedDmMessages("alice@example.com", "bob@example.com").map((message) => message.id),
    ).toEqual(["dm-1", "dm-2"]);

    // A second flush in the same session must NOT re-send: both entries
    // are already inflight (handed to the XMPP client, ack pending).
    xmpp.send_chat_message.mockClear();
    await (client as unknown as { flushQueuedDirectMessages: () => Promise<void> }).flushQueuedDirectMessages();
    expect(xmpp.send_chat_message.mock.calls).toEqual([]);

    // Now simulate the server acking dm-1 — that entry drops out of the
    // persisted queue; dm-2 is still pending.
    xmpp.emit("message:acked", { id: "dm-1" });
    expect(
      listQueuedDmMessages("alice@example.com", "bob@example.com").map((message) => message.id),
    ).toEqual(["dm-2"]);

    xmpp.emit("message:acked", { id: "dm-2" });
    expect(listQueuedDmMessages("alice@example.com", "bob@example.com")).toEqual([]);
  });

  test("replays queued room messages in order after the room rejoins", async () => {
    const client = new BrowserXmppClient(session());
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

    const handlers = new Map<string, Array<(payload: unknown) => void>>();
    const xmpp = {
      send_raw_iq: mock(async (xml: string) => {
        expect(xml).toContain("<url>https://example.com/room</url>");
        expect(xml).toContain(`<scope>${roomJid}</scope>`);
        return `<iq type="result" id="lookup-room-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="fresh-room-token" original-url="https://example.com/room" normalized-url="https://example.com/room" expires-at="2026-06-01T12:05:00.000Z"><title>Example</title></preview></lookup></iq>`;
      }),
      send_groupchat_message: mock(async (_room: string, _body: string, opts: { stanza_id?: string }) => opts.stanza_id),
      on(event: string, handler: (payload: unknown) => void) {
        const list = handlers.get(event) ?? [];
        list.push(handler);
        handlers.set(event, list);
      },
      emit(event: string, payload: unknown) {
        for (const handler of handlers.get(event) ?? []) {
          handler(payload);
        }
      },
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean; currentRoom: string | null }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean; currentRoom: string | null }).connected = true;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean; currentRoom: string | null }).currentRoom =
      roomJid;
    (client as unknown as { joinedMucReady: Set<string> }).joinedMucReady.add(roomJid);
    (client as unknown as { wireEvents: (x: typeof xmpp) => void }).wireEvents(xmpp);

    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        await (client as unknown as { flushQueuedRoomMessages: (roomJid: string) => Promise<void> }).flushQueuedRoomMessages(roomJid);
      });
    });

    expect(xmpp.send_raw_iq).toHaveBeenCalledTimes(1);
    expect(xmpp.send_groupchat_message.mock.calls.map((call) => (call[2] as { stanza_id?: string }).stanza_id)).toEqual([
      "room-1",
      "room-2",
    ]);
    expect((xmpp.send_groupchat_message.mock.calls[0]?.[2] as { link_preview_token?: string }).link_preview_token)
      .toBe("fresh-room-token");
    expect((xmpp.send_groupchat_message.mock.calls[1]?.[2] as { link_preview_token?: string }).link_preview_token)
      .toBeUndefined();
    // Persisted entries stay until ack, same as the DM path.
    expect(
      listQueuedRoomMessages("alice@example.com", roomJid).map((message) => message.id),
    ).toEqual(["room-1", "room-2"]);

    xmpp.emit("message:acked", { id: "room-1" });
    xmpp.emit("message:acked", { id: "room-2" });
    expect(listQueuedRoomMessages("alice@example.com", roomJid)).toEqual([]);
  });
});

describe("offline outbound queue hydration", () => {
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
      peerJid: "bob@example.com",
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
});
