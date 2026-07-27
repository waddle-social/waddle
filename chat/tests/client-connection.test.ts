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
import type { ResumePersistence } from "../src/lib/xmpp/resume-persistence";
import type { XmppStatusSnapshot } from "../src/lib/xmpp/types";

const SCOPE = "alice@example.com";

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

type QueueOverrides = {
  canUseConnectedSession?: () => boolean;
  roomIsReady?: (roomJid: string) => boolean;
  sendDirect?: (peerJid: string, body: string, opts: { id: string }) => Promise<string | null>;
  sendRoom?: (roomJid: string, body: string, opts: { id: string }) => Promise<string | null>;
  emitStatus?: (snapshot: XmppStatusSnapshot) => void;
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

    queue.queueDirectMessage("bob@example.com", "first", { id: "dm-1" });
    queue.queueDirectMessage("bob@example.com", "second", { id: "dm-2" });
    queue.queueDirectMessage("bob@example.com", "third", { id: "dm-3" });

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

    queue.queueDirectMessage("room@muc.example.com/juliet", "psst", { id: "pm-1", mucPm: true });
    queue.queueDirectMessage("bob@example.com/desktop", "hi", { id: "dm-1" });

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

    queue.queueDirectMessage("bob@example.com", "first", { id: "dm-1" });
    queue.queueDirectMessage("bob@example.com", "second", { id: "dm-2" });
    queue.queueDirectMessage("bob@example.com", "third", { id: "dm-3" });
    queue.markInflight("dm-1");

    await queue.flushDirect();

    // dm-1 already in flight, dm-3 not reached after the drop mid-drain.
    expect(sent).toEqual(["second"]);
  });

  test("synchronous XEP-0198 ack cannot strand a persisted queued send", async () => {
    let queue: OfflineSendQueue;
    const created = createQueue({
      sendDirect: async (_peer, _body, opts) => {
        // A host may dispatch the acknowledgement while resolving its send
        // promise. The queue must have claimed the stable stanza id already.
        queue.handleAck(opts.id);
        return opts.id;
      },
    });
    queue = created.queue;
    queue.queueDirectMessage("bob@example.com", "hello", { id: "dm-sync-ack" });

    await queue.flushDirect();

    expect(listQueuedMessages(SCOPE)).toEqual([]);
  });

  test("null, throwing, and mismatched send outcomes release the attempt for a later retry", async () => {
    const outcomes: Array<"null" | "throw" | "mismatch"> = ["null", "throw", "mismatch"];
    for (const outcome of outcomes) {
      const { queue } = createQueue({
        sendDirect: async (_peer, _body, opts) => {
          if (outcome === "throw") throw new Error("transport closed");
          return outcome === "mismatch" ? `${opts.id}-wrong` : null;
        },
      });
      const id = `dm-${outcome}`;
      queue.queueDirectMessage("bob@example.com", outcome, { id });
      await queue.flushDirect().catch(() => undefined);

      // The persisted identity remains available to a later generation;
      // it was not left falsely in-flight by the failed host attempt.
      expect(listQueuedMessages(SCOPE).some((entry) => entry.id === id)).toBe(true);
      await queue.flushDirect().catch(() => undefined);
    }
  });

  test("a stale direct non-retryable rejection after dispose cannot delete the persisted entry", async () => {
    let continueSend: (() => void) | undefined;
    const { queue } = createQueue({
      sendDirect: async () => {
        await new Promise<void>((resolve) => { continueSend = resolve; });
        return compatWasmSendResult({ kind: "invalid-recipient" });
      },
    });
    queue.queueDirectMessage("bob@example.com", "hello", { id: "dm-stale-dispose" });
    const flush = queue.flushDirect();
    queue.dispose();
    continueSend?.();
    await flush;

    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toContain("dm-stale-dispose");
  });

  test("a stale room non-retryable rejection after dispose cannot delete the persisted entry", async () => {
    let continueSend: (() => void) | undefined;
    const { queue } = createQueue({
      sendRoom: async () => {
        await new Promise<void>((resolve) => { continueSend = resolve; });
        return compatWasmSendResult({ kind: "invalid-recipient" });
      },
    });
    queue.queueRoomMessage("general@muc.example.com", "hello", { id: "room-stale-dispose" });
    const flush = queue.flushRoom("general@muc.example.com");
    queue.dispose();
    continueSend?.();
    await flush;

    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toContain("room-stale-dispose");
  });

  test("ack removes the persisted copy and reports queue depth + latency", async () => {
    const { queue, events } = createQueue();
    const depths: Array<{ kind: "room" | "dm"; persisted: number; inflight: number }> = [];
    const acked: Array<{ id: string; kind: "room" | "dm" }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("messageAcked", (id, meta) => acked.push({ id, kind: meta.kind }));

    queue.queueDirectMessage("bob@example.com", "hello", { id: "dm-1" });
    await queue.flushDirect();
    queue.handleAck("dm-1");

    expect(listQueuedMessages(SCOPE)).toHaveLength(0);
    expect(acked).toEqual([{ id: "dm-1", kind: "dm" }]);
    expect(depths.slice(-2)).toEqual([
      { kind: "dm", persisted: 0, inflight: 0 },
      { kind: "room", persisted: 0, inflight: 0 },
    ]);
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

    queue.queueDirectMessage("bob@example.com", "bad recipient", { id: "dm-bad" });
    queue.queueDirectMessage("bob@example.com", "fine", { id: "dm-ok" });

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

    queue.queueRoomMessage("general@muc.example.com", "to general", { id: "room-1" });
    queue.queueRoomMessage("random@muc.example.com", "to random", { id: "room-2" });

    await queue.flushRoom("general@muc.example.com");
    await queue.flushRoom("random@muc.example.com");

    expect(sends).toEqual([{ roomJid: "general@muc.example.com", body: "to general" }]);
  });

  test("queueing while connected (room not joined) does not emit a reconnecting status (#1164)", () => {
    const { queue, statuses } = createQueue({
      canUseConnectedSession: () => true,
      roomIsReady: () => false,
    });

    queue.queueRoomMessage("general@muc.example.com", "hello", { id: "room-q1" });

    // The message still queues, but the global banner must stay online.
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["room-q1"]);
    expect(statuses).toHaveLength(0);
  });

  test("queueing while the session is unusable still reports the reconnecting status", () => {
    const { queue, statuses } = createQueue({
      canUseConnectedSession: () => false,
    });

    queue.queueDirectMessage("bob@example.com", "hello", { id: "dm-q1" });

    expect(statuses).toEqual([
      { state: "reconnecting", detail: "Message queued until the connection returns" },
    ]);
  });

  test("seedFromResumeState tracks XEP-0198 replayed stanza ids so acks clear the store", () => {
    const { queue, events } = createQueue();
    const depths: Array<{ kind: "room" | "dm"; persisted: number; inflight: number }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));

    queue.queueDirectMessage("bob@example.com", "native replay", { id: "dm-native" });
    queue.seedFromResumeState({
      previd: "p",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [{
        xml: '<message id="dm-native" to="bob@example.com"><body>native replay</body></message>',
        sentAt: "2026-07-26T12:34:56.789Z",
      }],
    });

    queue.handleAck("dm-native");

    expect(listQueuedMessages(SCOPE)).toHaveLength(0);
    expect(depths.slice(-2)).toEqual([
      { kind: "dm", persisted: 0, inflight: 0 },
      { kind: "room", persisted: 0, inflight: 0 },
    ]);
  });

  test("fresh fallback leaves the native resume tail owned until its core retry is acked", async () => {
    const sent: string[] = [];
    const { queue } = createQueue({
      sendDirect: async (_peer, _body, opts) => {
        sent.push(opts.id);
        return opts.id;
      },
    });
    queue.queueDirectMessage("bob@example.com", "native fallback", { id: "dm-native" });
    queue.queueDirectMessage("bob@example.com", "ordinary reconnect", { id: "dm-ordinary" });
    queue.seedFromResumeState({
      previd: "old-sm",
      inboundH: 0,
      outboundH: 1,
      unhandledOutboundEntries: [{
        xml: '<message id="dm-native"><body>native fallback</body></message>',
        sentAt: "2026-07-26T12:34:56.789Z",
      }],
    });
    queue.markInflight("dm-ordinary");

    // `<failed h='0'/>` leads to a fresh session-ready before the Rust
    // runtime receives the fresh `<enabled/>`. Only the ordinary write may
    // re-enter the JS flush; the resume tail must wait for core fallback.
    queue.clearOrdinaryInflight();
    await queue.flushDirect();
    expect(sent).toEqual(["dm-ordinary"]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id))
      .toEqual(["dm-native", "dm-ordinary"]);

    // The Rust fallback emits the one retained native stanza after
    // `<enabled/>`; its acknowledgement is the only normal release of that
    // queue ownership.
    queue.handleAck("dm-native");
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-ordinary"]);
  });

  test("transient native replay failure survives teardown and a fallback ack releases once", async () => {
    const initial = createQueue();
    const failures: string[] = [];
    initial.events.on("messageDeliveryFailure", (id) => failures.push(id));
    initial.queue.queueDirectMessage("bob@example.com", "native replay", { id: "dm-replay-failed" });
    initial.queue.seedFromResumeState({
      previd: "p",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [{
        xml: '<message id="dm-replay-failed"><body>native replay</body></message>',
        sentAt: "2026-07-26T12:34:56.789Z",
      }],
    });

    initial.queue.handleFailed("dm-replay-failed");
    expect(failures).toEqual([]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-replay-failed"]);

    // The replay still owns the id until teardown, so this generation cannot
    // double-send it. A fresh fallback generation can retry the durable row.
    await initial.queue.flushDirect();
    initial.queue.dispose();

    const fallbackSends: string[] = [];
    const fallback = createQueue({
      sendDirect: async (_peer, _body, opts) => {
        fallbackSends.push(opts.id);
        return opts.id;
      },
    });
    await fallback.queue.flushDirect();
    expect(fallbackSends).toEqual(["dm-replay-failed"]);

    fallback.queue.handleAck("dm-replay-failed");
    fallback.queue.handleAck("dm-replay-failed");
    expect(listQueuedMessages(SCOPE)).toEqual([]);
  });
});

describe("ReconnectScheduler", () => {
  test("backs off exponentially and coalesces while a timer is pending", () => {
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

  test("caps attempts at 10 and reports exhaustion instead of scheduling forever (#1164)", () => {
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

  test("does not schedule while destroying and reports reconnect duration on completion", () => {
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
    loadSm: () => saved,
    consumeSm: () => {
      calls.push("consumeSm");
      return saved;
    },
    saveSm: (state) => {
      calls.push("saveSm");
      saved = state;
    },
    clearSm: () => {
      calls.push("clearSm");
      saved = null;
    },
    preparePagehideHandoff: () => calls.push("preparePagehideHandoff"),
    loadJoinedRooms: () => [],
    saveJoinedRooms: () => undefined,
    clearJoinedRooms: () => calls.push("clearJoinedRooms"),
    clearAutoJoinBlocks: () => calls.push("clearAutoJoinBlocks"),
  };
  return { persistence, calls, getSaved: () => saved };
}

describe("ResumeStateStore", () => {
  test("persistForPageHide snapshots the live state with the session resource", () => {
    const { persistence, calls, getSaved } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    let persistedRooms = 0;

    store.persistForPageHide(
      { previd: "p1", inboundH: 3, outboundH: 4 },
      "web-abc",
      () => { persistedRooms += 1; },
    );

    expect(calls).toEqual(["preparePagehideHandoff", "saveSm"]);
    expect(getSaved()).toEqual({ previd: "p1", inboundH: 3, outboundH: 4, resource: "web-abc" });
    expect(persistedRooms).toBe(1);
  });

  test("persistForPageHide drops unacked-outbound state it cannot replay", () => {
    const { persistence, calls } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    let persistedRooms = 0;

    store.persistForPageHide(
      { previd: "p1", inboundH: 3, outboundH: 4, hasUnackedOutbound: true },
      "web-abc",
      () => { persistedRooms += 1; },
    );

    expect(calls).toEqual(["preparePagehideHandoff", "clearSm"]);
    expect(store.state).toBeNull();
    expect(persistedRooms).toBe(1);
  });

  test("captureFromDisconnect stamps the resource and keeps state in memory only", () => {
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

  test("discardState clears the persisted slot; clearAll also drops joined rooms + handle", () => {
    const { persistence, calls } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    store.captureFromDisconnect(
      { get_resume_state: () => ({ previd: "p3", inboundH: 0, outboundH: 0 }) },
      "web-r",
    );

    store.discardState();
    expect(store.state).toBeNull();
    expect(calls).toEqual(["clearSm"]);

    let freed = 0;
    store.setHandle({ free: () => { freed += 1; } } as never);
    store.clearAll();
    expect(freed).toBe(1);
    expect(store.handle).toBeNull();
    expect(calls).toEqual([
      "clearSm",
      "clearSm",
      "clearJoinedRooms",
      "clearAutoJoinBlocks",
    ]);
  });
});
