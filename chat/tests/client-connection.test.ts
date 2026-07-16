/**
 * Unit tests for the connection-support modules extracted from
 * `BrowserXmppClient` (`src/lib/xmpp/client-connection.ts`):
 * offline-queue drain ordering, non-retryable discard, ack/failure
 * bookkeeping, reconnect backoff scheduling, and resume-state
 * persistence — all exercised without constructing the full client.
 */
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { TypedEventBus, type ClientEvents } from "../src/lib/xmpp/client-events";
import {
  OfflineSendQueue,
  ReconnectScheduler,
  ResumeStateStore,
  compatWasmSendResult,
  type XmppResumeState,
} from "../src/lib/xmpp/client-connection";
import { listQueuedMessages } from "../src/lib/outbound-queue-store";
import {
  MemoryDurableOutboundStore,
  OUTBOUND_CLAIM_LEASE_MS,
  type DurableOutboundStore,
} from "../src/lib/outbound-durable-store";
import type {
  ResumePersistence,
  XmppResumeEntry,
} from "../src/lib/xmpp/resume-persistence";
import type { XmppStatusSnapshot } from "../src/lib/xmpp/types";

const SCOPE = "alice@example.com";

function messageResumeEntry(id: string): XmppResumeEntry {
  return {
    stanza: {
      stanzaKind: "message",
      tokens: [
        {
          kind: "start",
          name: { namespace: "jabber:client", localName: "message" },
          attributes: [{ name: { namespace: "", localName: "id" }, value: id }],
        },
        { kind: "end" },
      ],
    },
    sentAtEpochMs: Date.parse("2026-07-16T08:09:10.123Z"),
  };
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

const originalWindow = globalThis.window;
const originalLocalStorage = globalThis.localStorage;
let durableStore: MemoryDurableOutboundStore;

beforeEach(() => {
  durableStore = new MemoryDurableOutboundStore();
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

type QueueOverrides = {
  canUseConnectedSession?: () => boolean;
  roomIsReady?: (roomJid: string) => boolean;
  sendDirect?: (peerJid: string, body: string, opts: { id: string }) => Promise<string | null>;
  sendRoom?: (roomJid: string, body: string, opts: { id: string }) => Promise<string | null>;
  emitStatus?: (snapshot: XmppStatusSnapshot) => void;
  durableStore?: DurableOutboundStore;
};

function createQueue(overrides: QueueOverrides = {}) {
  const events = new TypedEventBus<ClientEvents>();
  const statuses: XmppStatusSnapshot[] = [];
  const queue = new OfflineSendQueue({
    queueScope: () => SCOPE,
    events,
    canUseConnectedSession: overrides.canUseConnectedSession ?? (() => true),
    roomIsReady: overrides.roomIsReady ?? (() => true),
    enqueueReason: () => "reconnecting",
    emitStatus: overrides.emitStatus ?? ((snapshot) => statuses.push(snapshot)),
    roomMemberJids: () => ({}),
    sendDirect: overrides.sendDirect ?? (async (_peer, _body, opts) => opts.id),
    sendRoom: overrides.sendRoom ?? (async (_room, _body, opts) => opts.id),
    durableStore: overrides.durableStore ?? durableStore,
  });
  return { queue, events, statuses };
}

describe("OfflineSendQueue drain ordering", () => {
  test("flushDirect replays queued DMs in enqueue order and marks them in flight", async () => {
    const sent: string[] = [];
    const { queue, events } = createQueue({
      sendDirect: async (_peer, body, opts) => {
        sent.push(body);
        return opts.id;
      },
    });
    const statusEvents: Array<{ id: string; status: "queued" | "sending" }> = [];
    events.on("queuedMessageStatus", (id, status) => statusEvents.push({ id, status }));

    await queue.queueDirectMessage("bob@example.com", "first", { id: "dm-1" });
    await queue.queueDirectMessage("bob@example.com", "second", { id: "dm-2" });
    await queue.queueDirectMessage("bob@example.com", "third", { id: "dm-3" });

    await queue.flushDirect();

    expect(sent).toEqual(["first", "second", "third"]);
    expect(statusEvents.filter((event) => event.status === "sending").map((event) => event.id))
      .toEqual(["dm-1", "dm-2", "dm-3"]);
    // Replayed entries stay persisted until the server acks them.
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-1", "dm-2", "dm-3"]);
  });

  test("MUC-PM drains to the full occupant JID, never the room bare JID (#1256)", async () => {
    const sent: Array<{ peer: string; mucPm?: boolean }> = [];
    const { queue } = createQueue({
      sendDirect: async (peer, _body, opts) => {
        sent.push({ peer, ...(opts.mucPm ? { mucPm: true } : {}) });
        return opts.id;
      },
    });

    await queue.queueDirectMessage("room@muc.example.com/juliet", "psst", { id: "pm-1", mucPm: true });
    await queue.queueDirectMessage("bob@example.com/desktop", "hi", { id: "dm-1" });

    await queue.flushDirect();

    expect(sent.sort((a, b) => a.peer.localeCompare(b.peer))).toEqual([
      // Normal DM sends stay bare-folded.
      { peer: "bob@example.com" },
      // Occupant address preserved verbatim + the muc#user marker option.
      { peer: "room@muc.example.com/juliet", mucPm: true },
    ]);
  });

  test("flushDirect skips entries already in flight and stops when the session drops", async () => {
    let connected = true;
    const sent: string[] = [];
    const { queue } = createQueue({
      canUseConnectedSession: () => connected,
      sendDirect: async (_peer, body, opts) => {
        sent.push(body);
        if (body === "second") connected = false;
        return opts.id;
      },
    });

    await queue.queueDirectMessage("bob@example.com", "first", { id: "dm-1" });
    await queue.queueDirectMessage("bob@example.com", "second", { id: "dm-2" });
    await queue.queueDirectMessage("bob@example.com", "third", { id: "dm-3" });
    queue.beginAttempt("dm-1", "dm");

    await queue.flushDirect();

    // dm-1 already in flight, dm-3 not reached after the drop mid-drain.
    expect(sent).toEqual(["second"]);
  });

  test("ack removes the persisted copy and reports queue depth + latency", async () => {
    const { queue, events } = createQueue();
    const depths: Array<{ kind: "room" | "dm"; persisted: number; inflight: number }> = [];
    const acked: Array<{ id: string; kind: "room" | "dm" }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("messageAcked", (id, meta) => acked.push({ id, kind: meta.kind }));

    await queue.queueDirectMessage("bob@example.com", "hello", { id: "dm-1" });
    await queue.flushDirect();
    await queue.handleAck("dm-1");

    expect(listQueuedMessages(SCOPE)).toHaveLength(0);
    expect(acked).toEqual([{ id: "dm-1", kind: "dm" }]);
    expect(depths.slice(-2)).toEqual([
      { kind: "dm", persisted: 0, inflight: 0 },
      { kind: "room", persisted: 0, inflight: 0 },
    ]);
  });

  test("a synchronous ack inside send clears persistence before the promise resolves", async () => {
    let queue!: OfflineSendQueue;
    const created = createQueue({
      sendDirect: async (_peer, _body, opts) => {
        await queue.handleAck(opts.id);
        return opts.id;
      },
    });
    queue = created.queue;
    const acked: string[] = [];
    created.events.on("messageAcked", (id) => acked.push(id));

    await queue.queueDirectMessage("bob@example.com", "fast ack", { id: "dm-fast" });
    await queue.flushDirect();

    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(acked).toEqual(["dm-fast"]);
  });

  test("a synchronous room ack inside send clears persistence before the promise resolves", async () => {
    let queue!: OfflineSendQueue;
    const created = createQueue({
      sendRoom: async (_room, _body, opts) => {
        await queue.handleAck(opts.id);
        return opts.id;
      },
    });
    queue = created.queue;

    await queue.queueRoomMessage("general@muc.example.com", "fast ack", { id: "room-fast" });
    await queue.flushRoom("general@muc.example.com");

    expect(listQueuedMessages(SCOPE)).toEqual([]);
  });

  test("a rejected attempt rolls back so the persisted message can retry", async () => {
    let attempt = 0;
    const { queue, events } = createQueue({
      sendDirect: async (_peer, _body, opts) => {
        attempt += 1;
        if (attempt === 1) throw new Error("socket unavailable");
        return opts.id;
      },
    });
    const depths: Array<{ persisted: number; inflight: number }> = [];
    const statuses: Array<{ id: string; status: "queued" | "sending" }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("queuedMessageStatus", (id, status) => statuses.push({ id, status }));
    await queue.queueDirectMessage("bob@example.com", "retry me", { id: "dm-retry" });

    await expect(queue.flushDirect()).rejects.toThrow("socket unavailable");
    expect(listQueuedMessages(SCOPE).map((message) => message.id)).toEqual(["dm-retry"]);
    expect(depths.at(-1)?.inflight).toBe(0);
    expect(statuses.at(-1)).toEqual({ id: "dm-retry", status: "queued" });

    await queue.flushDirect();
    await queue.handleAck("dm-retry");
    expect(attempt).toBe(2);
    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(depths.at(-1)).toEqual({ persisted: 0, inflight: 0 });
  });

  test("a confirmed WASM send stays inflight when its control follow-up disconnects", async () => {
    let connected = true;
    let attempts = 0;
    const { queue, events } = createQueue({
      canUseConnectedSession: () => connected,
      sendDirect: async (_peer, _body, opts) => {
        attempts += 1;
        const id = compatWasmSendResult({ kind: "sent", stanza_id: opts.id });
        connected = false;
        return id;
      },
    });
    const depths: Array<{ persisted: number; inflight: number }> = [];
    const failures: string[] = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("messageDeliveryFailure", (id) => failures.push(id));
    await queue.queueDirectMessage("bob@example.com", "confirmed before control failure", {
      id: "dm-confirmed-control-failure",
    });

    await queue.flushDirect();
    await queue.flushDirect();

    expect(attempts).toBe(1);
    expect(failures).toEqual([]);
    expect(listQueuedMessages(SCOPE).map((message) => message.id))
      .toEqual(["dm-confirmed-control-failure"]);
    expect(depths.at(-1)).toEqual({
      persisted: 1,
      inflight: 1,
      oldestAgeMs: 0,
    });
  });

  test("null and mismatched queued DM attempts roll back before a later acked retry", async () => {
    let attempt = 0;
    const { queue, events } = createQueue({
      sendDirect: async (_peer, _body, opts) => {
        attempt += 1;
        if (attempt === 1) return null;
        if (attempt === 2) return "wrong-dm-id";
        return opts.id;
      },
    });
    const depths: Array<{ persisted: number; inflight: number }> = [];
    const statuses: Array<{ id: string; status: "queued" | "sending" }> = [];
    const queuedIds = () => listQueuedMessages(SCOPE).map((message) => message.id);
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("queuedMessageStatus", (id, status) => statuses.push({ id, status }));
    await queue.queueDirectMessage("bob@example.com", "retry DM", { id: "dm-null-mismatch" });

    await queue.flushDirect();
    expect(queuedIds()).toEqual(["dm-null-mismatch"]);
    expect(depths.at(-1)?.inflight).toBe(0);
    expect(statuses.at(-1)).toEqual({ id: "dm-null-mismatch", status: "queued" });

    await expect(queue.flushDirect())
      .rejects.toThrow("XMPP send returned a different stanza id");
    expect(queuedIds()).toEqual(["dm-null-mismatch"]);
    expect(depths.at(-1)?.inflight).toBe(0);
    expect(statuses.at(-1)).toEqual({ id: "dm-null-mismatch", status: "queued" });

    await queue.flushDirect();
    await queue.handleAck("dm-null-mismatch");
    expect(attempt).toBe(3);
    expect(queuedIds()).toEqual([]);
    expect(depths.at(-1)).toEqual({ persisted: 0, inflight: 0 });
  });

  test("a rejected queued room attempt stays retryable before a later acked retry", async () => {
    let attempt = 0;
    const { queue, events } = createQueue({
      sendRoom: async (_room, _body, opts) => {
        attempt += 1;
        if (attempt === 1) throw new Error("room socket unavailable");
        return opts.id;
      },
    });
    const depths: Array<{ persisted: number; inflight: number }> = [];
    const statuses: Array<{ id: string; status: "queued" | "sending" }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("queuedMessageStatus", (id, status) => statuses.push({ id, status }));
    await queue.queueRoomMessage("general@muc.example.com", "retry room", { id: "room-rejected" });

    await expect(queue.flushRoom("general@muc.example.com"))
      .rejects.toThrow("room socket unavailable");
    expect(listQueuedMessages(SCOPE).map((message) => message.id)).toEqual(["room-rejected"]);
    expect(depths.at(-1)?.inflight).toBe(0);
    expect(statuses.at(-1)).toEqual({ id: "room-rejected", status: "queued" });

    await queue.flushRoom("general@muc.example.com");
    await queue.handleAck("room-rejected");
    expect(attempt).toBe(2);
    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(depths.at(-1)).toEqual({ persisted: 0, inflight: 0 });
  });

  test("null and mismatched queued room attempts roll back and the coalescer later retries", async () => {
    let attempt = 0;
    const { queue, events } = createQueue({
      sendRoom: async (_room, _body, opts) => {
        attempt += 1;
        if (attempt === 1) return null;
        if (attempt === 2) return "wrong-room-id";
        return opts.id;
      },
    });
    const depths: Array<{ persisted: number; inflight: number }> = [];
    const statuses: Array<{ id: string; status: "queued" | "sending" }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("queuedMessageStatus", (id, status) => statuses.push({ id, status }));
    await queue.queueRoomMessage("general@muc.example.com", "retry room", { id: "room-retry" });

    await queue.flushRoom("general@muc.example.com");
    expect(listQueuedMessages(SCOPE).map((message) => message.id)).toEqual(["room-retry"]);
    expect(depths.at(-1)?.inflight).toBe(0);
    expect(statuses.at(-1)).toEqual({ id: "room-retry", status: "queued" });

    await expect(queue.flushRoom("general@muc.example.com"))
      .rejects.toThrow("XMPP send returned a different stanza id");
    expect(listQueuedMessages(SCOPE).map((message) => message.id)).toEqual(["room-retry"]);
    expect(depths.at(-1)?.inflight).toBe(0);
    expect(statuses.at(-1)).toEqual({ id: "room-retry", status: "queued" });

    await queue.flushRoom("general@muc.example.com");
    await queue.handleAck("room-retry");
    expect(attempt).toBe(3);
    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(depths.at(-1)).toEqual({ persisted: 0, inflight: 0 });
  });

  test("an ack after reload deletes persistence without ephemeral inflight state", async () => {
    const { queue } = createQueue();
    await queue.queueDirectMessage("bob@example.com", "already handled", { id: "dm-reloaded" });

    await queue.handleAck("dm-reloaded");

    expect(listQueuedMessages(SCOPE)).toEqual([]);
  });

  test("projection loss cannot suppress durable resend or fabricate an acknowledgement", async () => {
    const storage = window.localStorage;
    const originalSetItem = storage.setItem.bind(storage);
    storage.setItem = ((key: string, value: string) => {
      if (key.startsWith("waddle.chat.outbound-queue.v2.")) {
        throw new DOMException("projection quota", "QuotaExceededError");
      }
      originalSetItem(key, value);
    }) as Storage["setItem"];

    const sent: string[] = [];
    try {
      const { queue } = createQueue({
        sendDirect: async (_peer, _body, opts) => {
          sent.push(opts.id);
          return opts.id;
        },
      });
      await queue.queueDirectMessage("bob@example.com", "durable only", { id: "dm-no-projection" });
      expect(listQueuedMessages(SCOPE)).toEqual([]);

      await queue.flushDirect();
      expect(sent).toEqual(["dm-no-projection"]);
      await queue.handleAck("dm-no-projection");

      const reconstructed = createQueue({
        sendDirect: async (_peer, _body, opts) => {
          sent.push(opts.id);
          return opts.id;
        },
      }).queue;
      await reconstructed.flushDirect();
      expect(sent).toEqual(["dm-no-projection"]);
    } finally {
      storage.setItem = originalSetItem;
    }
  });

  test("durable enqueue failure emits no optimistic queue state and never reaches the wire", async () => {
    const failingStore = new MemoryDurableOutboundStore();
    failingStore.persistReady = async () => ({
      kind: "failed",
      reason: "quota",
      cause: new DOMException("durable quota", "QuotaExceededError"),
    });
    const sent: string[] = [];
    const { queue, events } = createQueue({
      durableStore: failingStore,
      sendDirect: async (_peer, _body, opts) => {
        sent.push(opts.id);
        return opts.id;
      },
    });
    const statuses: string[] = [];
    events.on("queuedMessageStatus", (id, status) => statuses.push(`${id}:${status}`));

    await expect(queue.queueDirectMessage("bob@example.com", "must persist", { id: "dm-quota" }))
      .rejects.toThrow("Outbound persistence enqueue-direct failed: quota");
    await queue.flushDirect();

    expect(sent).toEqual([]);
    expect(statuses).toEqual([]);
    expect(listQueuedMessages(SCOPE)).toEqual([]);
  });

  test("durable ack delete failure retains ownership and emits no false acknowledgement", async () => {
    const store = new MemoryDurableOutboundStore();
    const { queue, events } = createQueue({ durableStore: store });
    const acknowledgements: string[] = [];
    events.on("messageAck", (id) => acknowledgements.push(id));
    await queue.queueDirectMessage("bob@example.com", "retain until commit", { id: "dm-delete-fail" });
    await queue.flushDirect();
    store.delete = async () => ({
      kind: "failed",
      reason: "aborted",
      cause: new DOMException("delete aborted", "AbortError"),
    });

    await expect(queue.handleAck("dm-delete-fail"))
      .rejects.toThrow("Outbound persistence ack-delete failed: aborted");

    expect(acknowledgements).toEqual([]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-delete-fail"]);
  });

  test("two same-account tabs cannot claim and send the same durable row", async () => {
    const store = new MemoryDurableOutboundStore();
    const sends: string[] = [];
    const sendDirect = async (_peer: string, _body: string, opts: { id: string }) => {
      sends.push(opts.id);
      return opts.id;
    };
    const first = createQueue({ durableStore: store, sendDirect }).queue;
    await first.queueDirectMessage("bob@example.com", "once", { id: "dm-cross-tab" });
    const second = createQueue({ durableStore: store, sendDirect }).queue;

    await Promise.all([first.flushDirect(), second.flushDirect()]);

    expect(sends).toEqual(["dm-cross-tab"]);
  });

  test("non-retryable send failures are discarded instead of retried forever", async () => {
    const attempts: string[] = [];
    const { queue, events } = createQueue({
      sendDirect: async (_peer, body, _opts) => {
        attempts.push(body);
        if (body === "bad recipient") return compatWasmSendResult({ kind: "invalid-recipient" });
        return _opts.id;
      },
    });
    const failures: string[] = [];
    events.on("messageDeliveryFailure", (id) => failures.push(id));

    await queue.queueDirectMessage("bob@example.com", "bad recipient", { id: "dm-bad" });
    await queue.queueDirectMessage("bob@example.com", "fine", { id: "dm-ok" });

    await queue.flushDirect();

    expect(attempts).toEqual(["bad recipient", "fine"]);
    expect(failures).toEqual(["dm-bad"]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-ok"]);
  });

  test("flushRoom only drains the ready room and merges its member mentions", async () => {
    const sends: Array<{ roomJid: string; body: string }> = [];
    const { queue } = createQueue({
      roomIsReady: (roomJid) => roomJid === "general@muc.example.com",
      sendRoom: async (roomJid, body, opts) => {
        sends.push({ roomJid, body });
        return opts.id;
      },
    });

    await queue.queueRoomMessage("general@muc.example.com", "to general", { id: "room-1" });
    await queue.queueRoomMessage("random@muc.example.com", "to random", { id: "room-2" });

    await queue.flushRoom("general@muc.example.com");
    await queue.flushRoom("random@muc.example.com");

    expect(sends).toEqual([{ roomJid: "general@muc.example.com", body: "to general" }]);
  });

  test("queueing while connected (room not joined) does not emit a reconnecting status (#1164)", async () => {
    const { queue, statuses } = createQueue({
      canUseConnectedSession: () => true,
      roomIsReady: () => false,
    });

    await queue.queueRoomMessage("general@muc.example.com", "hello", { id: "room-q1" });

    // The message still queues, but the global banner must stay online.
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["room-q1"]);
    expect(statuses).toHaveLength(0);
  });

  test("queueing while the session is unusable still reports the reconnecting status", async () => {
    const { queue, statuses } = createQueue({
      canUseConnectedSession: () => false,
    });

    await queue.queueDirectMessage("bob@example.com", "hello", { id: "dm-q1" });

    expect(statuses).toEqual([
      { state: "reconnecting", detail: "Message queued until the connection returns" },
    ]);
  });

  test("seedFromResumeState tracks XEP-0198 replayed stanza ids so acks clear the store", async () => {
    const { queue, events } = createQueue();
    const depths: Array<{ kind: "room" | "dm"; persisted: number; inflight: number }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));

    await queue.queueDirectMessage("bob@example.com", "native replay", { id: "dm-native" });
    await queue.seedFromResumeState({
      previd: "p",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [messageResumeEntry("dm-native")],
    });

    await queue.handleAck("dm-native");

    expect(listQueuedMessages(SCOPE)).toHaveLength(0);
    expect(depths.slice(-2)).toEqual([
      { kind: "dm", persisted: 0, inflight: 0 },
      { kind: "room", persisted: 0, inflight: 0 },
    ]);
  });

  test("failed resume transfers retry ownership without deleting or browser-flushing the durable row", async () => {
    const sent: string[] = [];
    const { queue, events } = createQueue({
      sendDirect: async (_peer, _body, opts) => {
        sent.push(opts.id);
        return opts.id;
      },
    });
    const failures: string[] = [];
    const depths: Array<{ persisted: number; inflight: number }> = [];
    events.on("messageDeliveryFailure", (id) => failures.push(id));
    events.on("queueDepthChange", (depth) => depths.push(depth));

    await queue.queueDirectMessage("bob@example.com", "native fallback", { id: "dm-native-fallback" });
    await queue.seedFromResumeState({
      previd: "p",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [messageResumeEntry("dm-native-fallback")],
    });

    await queue.handleFailed("dm-native-fallback");
    await queue.clearInflight();
    await queue.flushDirect();

    expect(sent).toEqual([]);
    expect(failures).toEqual([]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-native-fallback"]);
    expect(depths.at(-1)).toMatchObject({ persisted: 1, inflight: 1 });

    await queue.handleAck("dm-native-fallback");
    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(depths.at(-1)).toEqual({ persisted: 0, inflight: 0 });
  });

  test("a plain fresh bind releases an unattempted resume replay claim to the durable browser queue", async () => {
    const sent: string[] = [];
    const { queue } = createQueue({
      sendDirect: async (_peer, _body, opts) => {
        sent.push(opts.id);
        return opts.id;
      },
    });
    await queue.queueDirectMessage("bob@example.com", "fresh without SM", { id: "dm-no-resume" });
    await queue.seedFromResumeState({
      previd: "p",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [messageResumeEntry("dm-no-resume")],
    });

    await queue.clearInflight();
    await queue.flushDirect();

    expect(sent).toEqual(["dm-no-resume"]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-no-resume"]);
  });

  test("a reconstructed queue reclaims a retained failed-resume row after an immediate or mid-write crash", async () => {
    const original = createQueue().queue;
    await original.queueDirectMessage("bob@example.com", "survive fallback crash", { id: "dm-crash-fallback" });
    await original.seedFromResumeState({
      previd: "p",
      inboundH: 3,
      outboundH: 4,
      unhandledOutboundEntries: [messageResumeEntry("dm-crash-fallback")],
    });
    await original.handleFailed("dm-crash-fallback");

    const reclaimed: string[] = [];
    const reconstructed = createQueue({
      sendDirect: async (_peer, _body, opts) => {
        reclaimed.push(opts.id);
        return opts.id;
      },
    }).queue;

    const actualNow = Date.now;
    Date.now = () => actualNow() + OUTBOUND_CLAIM_LEASE_MS + 1;
    try {
      await reconstructed.flushDirect();
    } finally {
      Date.now = actualNow;
    }

    expect(reclaimed).toEqual(["dm-crash-fallback"]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-crash-fallback"]);
    await reconstructed.handleAck("dm-crash-fallback");
    expect(listQueuedMessages(SCOPE)).toEqual([]);
  });
});

describe("ReconnectScheduler", () => {
  test("backs off exponentially and coalesces while a timer is pending", async () => {
    const scheduled: Array<{ attempt: number; delayMs: number }> = [];
    const scheduler = new ReconnectScheduler({
      isDestroying: () => false,
      connect: async () => undefined,
      onScheduled: (info) => scheduled.push(info),
      onExhausted: () => undefined,
    });

    scheduler.schedule();
    scheduler.schedule(); // timer pending — must not double-schedule
    scheduler.clearTimer();
    scheduler.schedule();
    scheduler.clearTimer();

    expect(scheduled).toEqual([
      { attempt: 1, delayMs: 2000 },
      { attempt: 2, delayMs: 4000 },
    ]);

    scheduler.resetAttempts();
    scheduler.schedule();
    scheduler.clearTimer();
    expect(scheduled.at(-1)).toEqual({ attempt: 1, delayMs: 2000 });
  });

  test("caps attempts at 10 and reports exhaustion instead of scheduling forever (#1164)", async () => {
    const scheduled: Array<{ attempt: number; delayMs: number }> = [];
    let exhausted = 0;
    const scheduler = new ReconnectScheduler({
      isDestroying: () => false,
      connect: async () => undefined,
      onScheduled: (info) => scheduled.push(info),
      onExhausted: () => { exhausted += 1; },
    });

    for (let i = 0; i < 12; i += 1) {
      scheduler.schedule();
      scheduler.clearTimer();
    }

    expect(scheduled).toHaveLength(10);
    expect(scheduled.at(-1)).toEqual({ attempt: 10, delayMs: 60000 });
    expect(exhausted).toBe(2);

    // A successful session-ready resets the budget.
    scheduler.resetAttempts();
    scheduler.schedule();
    scheduler.clearTimer();
    expect(scheduled.at(-1)).toEqual({ attempt: 1, delayMs: 2000 });
  });

  test("does not schedule while destroying and reports reconnect duration on completion", async () => {
    const scheduled: Array<{ attempt: number; delayMs: number }> = [];
    const scheduler = new ReconnectScheduler({
      isDestroying: () => true,
      connect: async () => undefined,
      onScheduled: (info) => scheduled.push(info),
      onExhausted: () => undefined,
    });
    scheduler.schedule();
    expect(scheduled).toHaveLength(0);

    expect(scheduler.noteStatus({ state: "online", detail: "" })).toEqual({});
    expect(scheduler.noteStatus({ state: "reconnecting", detail: "" })).toEqual({});
    const meta = scheduler.noteStatus({ state: "online", detail: "" });
    expect(meta.reconnectDurationMs).toBeGreaterThanOrEqual(0);
    expect(scheduler.noteStatus({ state: "online", detail: "" })).toEqual({});
  });
});

function createRecordingPersistence() {
  let saved: XmppResumeState | null = null;
  const calls: string[] = [];
  const persistence: ResumePersistence = {
    loadCatchup: () => null,
    saveCatchup: () => undefined,
    clearCatchup: () => undefined,
    loadSm: async () => ({ kind: "committed", value: saved }),
    consumeSm: async () => {
      calls.push("consumeSm");
      return { kind: "committed", value: saved };
    },
    saveSm: async (state) => {
      calls.push("saveSm");
      saved = state;
      return { kind: "committed", value: undefined };
    },
    clearSm: async () => {
      calls.push("clearSm");
      const removed = saved !== null;
      saved = null;
      return { kind: "committed", value: removed };
    },
    preparePagehideHandoff: () => calls.push("preparePagehideHandoff"),
    reclaimPagehideOwnership: () => calls.push("reclaimPagehideOwnership"),
    loadJoinedRooms: () => [],
    saveJoinedRooms: () => undefined,
    clearJoinedRooms: () => calls.push("clearJoinedRooms"),
  };
  return { persistence, calls, getSaved: () => saved };
}

describe("ResumeStateStore", () => {
  test("persistForPageHide snapshots the live state with the session resource", async () => {
    const { persistence, calls, getSaved } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    let persistedRooms = 0;

    store.persistForPageHide(
      { previd: "p1", inboundH: 3, outboundH: 4 },
      "web-abc",
      () => { persistedRooms += 1; },
    );
    await Promise.resolve();

    expect(calls).toEqual(["preparePagehideHandoff", "saveSm"]);
    expect(getSaved()).toEqual({ previd: "p1", inboundH: 3, outboundH: 4, resource: "web-abc" });
    expect(persistedRooms).toBe(1);
  });

  test("persistForPageHide drops unacked-outbound state it cannot replay", async () => {
    const { persistence, calls } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    let persistedRooms = 0;

    store.persistForPageHide(
      { previd: "p1", inboundH: 3, outboundH: 4, hasUnackedOutbound: true },
      "web-abc",
      () => { persistedRooms += 1; },
    );
    await Promise.resolve();

    expect(calls).toEqual(["preparePagehideHandoff", "clearSm"]);
    expect(store.state).toBeNull();
    expect(persistedRooms).toBe(1);
  });

  test("captureFromDisconnect stamps the resource and keeps state in memory only", async () => {
    const { persistence, calls } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);

    const captured = store.captureFromDisconnect(
      { get_resume_state: () => ({ previd: "p2", inboundH: 1, outboundH: 2 }) },
      "web-xyz",
    );

    expect(captured).toEqual({ previd: "p2", inboundH: 1, outboundH: 2, resource: "web-xyz" });
    expect(store.state).toEqual(captured);
    // In-memory only: ordinary disconnects never touch the persisted slot.
    expect(calls).toEqual([]);
  });

  test("discardState clears the persisted slot; clearAll also drops joined rooms + handle", async () => {
    const { persistence, calls } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    store.captureFromDisconnect(
      { get_resume_state: () => ({ previd: "p3", inboundH: 0, outboundH: 0 }) },
      "web-r",
    );

    await store.discardState();
    expect(store.state).toBeNull();
    expect(calls).toEqual(["clearSm"]);

    let freed = 0;
    store.setHandle({ free: () => { freed += 1; } } as never);
    await store.clearAll();
    expect(freed).toBe(1);
    expect(store.handle).toBeNull();
  });
});
