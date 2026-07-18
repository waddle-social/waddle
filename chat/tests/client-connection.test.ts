/**
 * Unit tests for the connection-support modules extracted from
 * `BrowserXmppClient` (`src/lib/xmpp/client-connection.ts`):
 * offline-queue drain ordering, non-retryable discard, ack/failure
 * bookkeeping, reconnect backoff scheduling, and resume-state
 * persistence — all exercised without constructing the full client.
 */
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  TypedEventBus,
  type ClientEvents,
  type QueueDepthTelemetry,
} from "../src/lib/xmpp/client-events";
import {
  OfflineSendQueue,
  ReconnectScheduler,
  ResumeStateTeardownError,
  ResumeStateStore,
  wasmSendMessageId,
  type XmppResumeState,
} from "../src/lib/xmpp/client-connection";
import { listQueuedMessages } from "../src/lib/outbound-queue-store";
import {
  committedOrThrow,
  OUTBOUND_CLAIM_LEASE_MS,
  OutboundPersistenceError,
  type DurableOutboundStore,
  type OutboundOwnerActivation,
  type OutboundOwnerContext,
  type OutboundOwnerHint,
} from "../src/lib/xmpp-runtime/durable-contract";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";
import type {
  ResumePersistence,
  XmppResumeEntry,
} from "../src/lib/xmpp/resume-persistence";
import { XmppLifecycleId } from "../src/lib/xmpp/resume-persistence";
import type { XmppStatusSnapshot } from "../src/lib/xmpp/types";

const SCOPE = "alice@example.com";

function latestQueueDepths(
  depths: readonly QueueDepthTelemetry[],
): Partial<Record<QueueDepthTelemetry["kind"], QueueDepthTelemetry>> {
  return Object.fromEntries(
    depths.slice(-2).map((depth) => [depth.kind, depth]),
  );
}

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
let createdQueues: OfflineSendQueue[] = [];

beforeEach(() => {
  createdQueues = [];
  durableStore = new MemoryDurableOutboundStore();
  const storage = createStorageMock();
  (globalThis as typeof globalThis & { localStorage: typeof storage }).localStorage = storage;
  (globalThis as typeof globalThis & { window: Window & { localStorage: typeof storage } }).window = {
    ...(originalWindow ?? {}),
    localStorage: storage,
  } as Window & { localStorage: typeof storage };
  localStorage.clear();
});

afterEach(async () => {
  await Promise.all(createdQueues.map((queue) => queue.dispose()));
  createdQueues = [];
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
  outboundOwnerHint?: () => OutboundOwnerHint;
  acceptOutboundOwner?: (activation: OutboundOwnerActivation) => void;
};

function createQueue(overrides: QueueOverrides = {}) {
  const events = new TypedEventBus<ClientEvents>();
  const statuses: XmppStatusSnapshot[] = [];
  const lifecycleId = XmppLifecycleId.create();
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
    lifecycleId,
    outboundOwnerHint: overrides.outboundOwnerHint ?? (() => ({
      ownerId: `queue-owner-${lifecycleId.value}`,
      ownerInstanceId: lifecycleId.value,
    })),
    acceptOutboundOwner: overrides.acceptOutboundOwner ?? (() => undefined),
    onAuthorityLost: () => undefined,
  });
  createdQueues.push(queue);
  return { queue, events, statuses };
}

async function createActiveQueue(overrides: QueueOverrides = {}) {
  const created = createQueue(overrides);
  await created.queue.ready();
  created.queue.beginConnectionGeneration(1);
  return created;
}

describe("OfflineSendQueue drain ordering", () => {
  test("a successor generation cannot bind before predecessor reconciliation settles", async () => {
    let blockNext = false;
    let releaseBlocked: (() => void) | null = null;
    let markBlocked!: () => void;
    const blocked = new Promise<void>((resolve) => {
      markBlocked = resolve;
    });
    const store = new MemoryDurableOutboundStore(
      undefined,
      async () => {
        if (!blockNext) return;
        blockNext = false;
        markBlocked();
        await new Promise<void>((resolve) => {
          releaseBlocked = resolve;
        });
      },
    );
    const { queue } = createQueue({ durableStore: store });
    await queue.ready();
    queue.beginConnectionGeneration(1);

    blockNext = true;
    const predecessor = queue.reconcileNativeSnapshot(1, null);
    expect(() => queue.beginConnectionGeneration(2)).toThrow(
      "A predecessor native snapshot is still reconciling",
    );
    await blocked;
    releaseBlocked?.();
    await predecessor;
    await queue.whenQuiescent();

    expect(queue.beginConnectionGeneration(2)).toBe(2);
  });

  test("final disposal never reclaims a consumed handoff or fences its successor", async () => {
    const store = new MemoryDurableOutboundStore();
    const activations: OutboundOwnerActivation[] = [];
    const { queue } = await createActiveQueue({
      durableStore: store,
      outboundOwnerHint: () => ({
        ownerId: "dispose-handoff-owner",
        ownerInstanceId: "dispose-instance-a",
      }),
      acceptOutboundOwner: (activation) => {
        activations.push(activation);
      },
    });
    const predecessor = activations[0];
    if (!predecessor) throw new Error("predecessor activation was not installed");

    committedOrThrow(
      "prepare-dispose-handoff",
      await store.preparePagehideHandoff(
        predecessor.fence,
        null,
        "dispose-handoff-token",
        null,
      ),
    );
    const successor = committedOrThrow(
      "claim-dispose-successor",
      await store.claimOwner(SCOPE, {
        ownerId: predecessor.fence.ownerId,
        ownerInstanceId: "dispose-instance-b",
        handoffToken: "dispose-handoff-token",
      }),
    );

    const claimOwner = store.claimOwner.bind(store);
    const renewOwner = store.renewOwner.bind(store);
    let claimsAfterBoundary = 0;
    let renewalsAfterBoundary = 0;
    store.claimOwner = async (...arguments_) => {
      claimsAfterBoundary += 1;
      return claimOwner(...arguments_);
    };
    store.renewOwner = async (...arguments_) => {
      renewalsAfterBoundary += 1;
      return renewOwner(...arguments_);
    };

    queue.beginFinalDisposal();
    await expect(queue.reconcileFinalNativeSnapshot(1, null)).rejects.toThrow(
      "Outbound owner fenced during native reconciliation",
    );
    await expect(queue.whenQuiescent()).rejects.toThrow(
      "Outbound queue quiescence failed",
    );
    await queue.dispose();

    expect(claimsAfterBoundary).toBe(0);
    expect(renewalsAfterBoundary).toBe(0);
    expect(committedOrThrow(
      "renew-dispose-successor",
      await renewOwner(successor.fence),
    )).toBe(true);
  });

  test("queue quiescence drains real rejected terminal waves and reports each failure once", async () => {
    const store = new MemoryDurableOutboundStore();
    const { queue } = await createActiveQueue({ durableStore: store });
    const firstError = new Error("first queue wave failed");
    const secondError = new Error("second queue wave failed");
    let markFirstStarted!: () => void;
    let markSecondStarted!: () => void;
    let releaseFirst!: () => void;
    let releaseSecond!: () => void;
    const firstStarted = new Promise<void>((resolve) => {
      markFirstStarted = resolve;
    });
    const secondStarted = new Promise<void>((resolve) => {
      markSecondStarted = resolve;
    });
    const firstGate = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const secondGate = new Promise<void>((resolve) => {
      releaseSecond = resolve;
    });
    const recordTerminal = store.recordTerminal.bind(store);
    let call = 0;
    store.recordTerminal = async (...arguments_) => {
      call += 1;
      if (call === 1) {
        markFirstStarted();
        await firstGate;
        throw firstError;
      }
      if (call === 2) {
        markSecondStarted();
        await secondGate;
        throw secondError;
      }
      return recordTerminal(...arguments_);
    };

    await queue.queueDirectMessage("bob@example.com", "terminal failure", {
      id: "dm-terminal-failure",
    });
    await queue.flushDirect();

    const first = queue.handleAck("dm-terminal-failure");
    await firstStarted;
    let settled = false;
    const barrier = queue.whenQuiescent().finally(() => {
      settled = true;
    });
    const second = queue.handleFailed("dm-terminal-failure");

    releaseFirst();
    await secondStarted;
    expect(settled).toBe(false);

    releaseSecond();
    const [firstOutcome, secondOutcome, barrierOutcome] = await Promise.allSettled([
      first,
      second,
      barrier,
    ]);
    expect(firstOutcome).toEqual({ status: "rejected", reason: firstError });
    expect(secondOutcome).toEqual({ status: "rejected", reason: secondError });
    expect(barrierOutcome.status).toBe("rejected");
    if (barrierOutcome.status !== "rejected") {
      throw new Error("barrier unexpectedly resolved");
    }
    expect(barrierOutcome.reason).toBeInstanceOf(AggregateError);
    const failures = (barrierOutcome.reason as AggregateError).errors;
    expect(failures.filter((error) => error === firstError)).toHaveLength(1);
    expect(failures.filter((error) => error === secondError)).toHaveLength(1);

    await expect(queue.whenQuiescent()).resolves.toBeUndefined();
  });

  test("flushDirect advances the direct lane only after each exact acknowledgement", async () => {
    const sent: string[] = [];
    const { queue, events } = await createActiveQueue({
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

    expect(sent).toEqual(["first"]);
    await queue.handleAck("dm-1");
    await queue.flushDirect();
    expect(sent).toEqual(["first", "second"]);
    await queue.handleAck("dm-2");
    await queue.flushDirect();
    expect(sent).toEqual(["first", "second", "third"]);
    expect(statusEvents.filter((event) => event.status === "sending").map((event) => event.id))
      .toEqual(["dm-1", "dm-2", "dm-3"]);
    // The current head stays persisted until the server acks it; earlier
    // heads were removed only by their exact durable acknowledgement.
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-3"]);
  });

  test("MUC-PM drains to the full occupant JID, never the room bare JID (#1256)", async () => {
    const sent: Array<{ id: string; peer: string; mucPm?: boolean }> = [];
    const { queue } = await createActiveQueue({
      sendDirect: async (peer, _body, opts) => {
        sent.push({ id: opts.id, peer, ...(opts.mucPm ? { mucPm: true } : {}) });
        return opts.id;
      },
    });

    await queue.queueDirectMessage("room@muc.example.com/juliet", "psst", { id: "pm-1", mucPm: true });
    await queue.queueDirectMessage("bob@example.com/desktop", "hi", { id: "dm-1" });

    await queue.flushDirect();
    expect(sent).toHaveLength(1);
    await queue.handleAck(sent[0]!.id);
    await queue.flushDirect();

    expect(sent.sort((a, b) => a.peer.localeCompare(b.peer))).toEqual([
      // Normal DM sends stay bare-folded.
      { id: "dm-1", peer: "bob@example.com" },
      // Occupant address preserved verbatim + the muc#user marker option.
      { id: "pm-1", peer: "room@muc.example.com/juliet", mucPm: true },
    ]);
  });

  test("flushDirect waits for a claimed head and stops when the session drops", async () => {
    let connected = true;
    const sent: string[] = [];
    const { queue } = await createActiveQueue({
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
    await queue.reconcileNativeSnapshot(1, {
      previd: "p",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [messageResumeEntry("dm-1")],
    }, "resume-replay");

    await queue.flushDirect();
    expect(sent).toEqual([]);
    await queue.handleAck("dm-1");
    await queue.flushDirect();

    // dm-1 fenced the lane until its exact ack. dm-3 is not reached after
    // sending dm-2 drops the connected session.
    expect(sent).toEqual(["second"]);
  });

  test("ack removes the persisted copy and reports queue depth + latency", async () => {
    const { queue, events } = await createActiveQueue();
    const depths: QueueDepthTelemetry[] = [];
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
    const created = await createActiveQueue({
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
    const created = await createActiveQueue({
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
    const { queue, events } = await createActiveQueue({
      sendDirect: async (_peer, _body, opts) => {
        attempt += 1;
        if (attempt === 1) throw new Error("socket unavailable");
        return opts.id;
      },
    });
    const depths: QueueDepthTelemetry[] = [];
    const statuses: Array<{ id: string; status: "queued" | "sending" }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("queuedMessageStatus", (id, status) => statuses.push({ id, status }));
    await queue.queueDirectMessage("bob@example.com", "retry me", { id: "dm-retry" });

    await expect(queue.flushDirect()).rejects.toThrow("socket unavailable");
    expect(listQueuedMessages(SCOPE).map((message) => message.id)).toEqual(["dm-retry"]);
    expect(latestQueueDepths(depths)).toMatchObject({
      dm: { kind: "dm", persisted: 1, inflight: 0 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });
    expect(statuses.at(-1)).toEqual({ id: "dm-retry", status: "queued" });

    await queue.flushDirect();
    await queue.handleAck("dm-retry");
    expect(attempt).toBe(2);
    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(latestQueueDepths(depths)).toEqual({
      dm: { kind: "dm", persisted: 0, inflight: 0 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });
  });

  test("a confirmed WASM send stays inflight when its control follow-up disconnects", async () => {
    let connected = true;
    let attempts = 0;
    const { queue, events } = await createActiveQueue({
      canUseConnectedSession: () => connected,
      sendDirect: async (_peer, _body, opts) => {
        attempts += 1;
        const id = wasmSendMessageId({ kind: "sent", stanza_id: opts.id });
        connected = false;
        return id;
      },
    });
    const depths: QueueDepthTelemetry[] = [];
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
    const latestDepths = latestQueueDepths(depths);
    expect(latestDepths).toMatchObject({
      dm: { kind: "dm", persisted: 1, inflight: 1 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });
    expect(latestDepths.dm?.oldestAgeMs).toBeGreaterThanOrEqual(0);
  });

  test("null and mismatched queued DM attempts roll back before a later acked retry", async () => {
    let attempt = 0;
    const { queue, events } = await createActiveQueue({
      sendDirect: async (_peer, _body, opts) => {
        attempt += 1;
        if (attempt === 1) return null;
        if (attempt === 2) return "wrong-dm-id";
        return opts.id;
      },
    });
    const depths: QueueDepthTelemetry[] = [];
    const statuses: Array<{ id: string; status: "queued" | "sending" }> = [];
    const queuedIds = () => listQueuedMessages(SCOPE).map((message) => message.id);
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("queuedMessageStatus", (id, status) => statuses.push({ id, status }));
    await queue.queueDirectMessage("bob@example.com", "retry DM", { id: "dm-null-mismatch" });

    await queue.flushDirect();
    expect(queuedIds()).toEqual(["dm-null-mismatch"]);
    expect(latestQueueDepths(depths)).toMatchObject({
      dm: { kind: "dm", persisted: 1, inflight: 0 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });
    expect(statuses.at(-1)).toEqual({ id: "dm-null-mismatch", status: "queued" });

    await expect(queue.flushDirect())
      .rejects.toThrow("XMPP send returned a different stanza id");
    expect(queuedIds()).toEqual(["dm-null-mismatch"]);
    expect(latestQueueDepths(depths)).toMatchObject({
      dm: { kind: "dm", persisted: 1, inflight: 0 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });
    expect(statuses.at(-1)).toEqual({ id: "dm-null-mismatch", status: "queued" });

    await queue.flushDirect();
    await queue.handleAck("dm-null-mismatch");
    expect(attempt).toBe(3);
    expect(queuedIds()).toEqual([]);
    expect(latestQueueDepths(depths)).toEqual({
      dm: { kind: "dm", persisted: 0, inflight: 0 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });
  });

  test("a rejected queued room attempt stays retryable before a later acked retry", async () => {
    let attempt = 0;
    const { queue, events } = await createActiveQueue({
      sendRoom: async (_room, _body, opts) => {
        attempt += 1;
        if (attempt === 1) throw new Error("room socket unavailable");
        return opts.id;
      },
    });
    const depths: QueueDepthTelemetry[] = [];
    const statuses: Array<{ id: string; status: "queued" | "sending" }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("queuedMessageStatus", (id, status) => statuses.push({ id, status }));
    await queue.queueRoomMessage("general@muc.example.com", "retry room", { id: "room-rejected" });

    await expect(queue.flushRoom("general@muc.example.com"))
      .rejects.toThrow("room socket unavailable");
    expect(listQueuedMessages(SCOPE).map((message) => message.id)).toEqual(["room-rejected"]);
    expect(latestQueueDepths(depths)).toMatchObject({
      dm: { kind: "dm", persisted: 0, inflight: 0 },
      room: { kind: "room", persisted: 1, inflight: 0 },
    });
    expect(statuses.at(-1)).toEqual({ id: "room-rejected", status: "queued" });

    await queue.flushRoom("general@muc.example.com");
    await queue.handleAck("room-rejected");
    expect(attempt).toBe(2);
    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(latestQueueDepths(depths)).toEqual({
      dm: { kind: "dm", persisted: 0, inflight: 0 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });
  });

  test("null and mismatched queued room attempts roll back and the coalescer later retries", async () => {
    let attempt = 0;
    const { queue, events } = await createActiveQueue({
      sendRoom: async (_room, _body, opts) => {
        attempt += 1;
        if (attempt === 1) return null;
        if (attempt === 2) return "wrong-room-id";
        return opts.id;
      },
    });
    const depths: QueueDepthTelemetry[] = [];
    const statuses: Array<{ id: string; status: "queued" | "sending" }> = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));
    events.on("queuedMessageStatus", (id, status) => statuses.push({ id, status }));
    await queue.queueRoomMessage("general@muc.example.com", "retry room", { id: "room-retry" });

    await queue.flushRoom("general@muc.example.com");
    expect(listQueuedMessages(SCOPE).map((message) => message.id)).toEqual(["room-retry"]);
    expect(latestQueueDepths(depths)).toMatchObject({
      dm: { kind: "dm", persisted: 0, inflight: 0 },
      room: { kind: "room", persisted: 1, inflight: 0 },
    });
    expect(statuses.at(-1)).toEqual({ id: "room-retry", status: "queued" });

    await expect(queue.flushRoom("general@muc.example.com"))
      .rejects.toThrow("XMPP send returned a different stanza id");
    expect(listQueuedMessages(SCOPE).map((message) => message.id)).toEqual(["room-retry"]);
    expect(latestQueueDepths(depths)).toMatchObject({
      dm: { kind: "dm", persisted: 0, inflight: 0 },
      room: { kind: "room", persisted: 1, inflight: 0 },
    });
    expect(statuses.at(-1)).toEqual({ id: "room-retry", status: "queued" });

    await queue.flushRoom("general@muc.example.com");
    await queue.handleAck("room-retry");
    expect(attempt).toBe(3);
    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(latestQueueDepths(depths)).toEqual({
      dm: { kind: "dm", persisted: 0, inflight: 0 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });
  });

  test("an ack without an exact generation claim cannot delete a reloaded row", async () => {
    const { queue, events } = await createActiveQueue();
    const acknowledgements: string[] = [];
    events.on("messageAck", (id) => acknowledgements.push(id));
    await queue.queueDirectMessage("bob@example.com", "already handled", { id: "dm-reloaded" });

    await queue.handleAck("dm-reloaded");

    expect(acknowledgements).toEqual([]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-reloaded"]);
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
      const { queue } = await createActiveQueue({
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

      const reconstructed = (await createActiveQueue({
        sendDirect: async (_peer, _body, opts) => {
          sent.push(opts.id);
          return opts.id;
        },
      })).queue;
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
    const { queue, events } = await createActiveQueue({
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

  test("a transient terminal-apply failure emits no acknowledgement before durable commit", async () => {
    const store = new MemoryDurableOutboundStore();
    const { queue, events } = await createActiveQueue({ durableStore: store });
    const acknowledgements: string[] = [];
    events.on("messageAck", (id) => acknowledgements.push(id));
    await queue.queueDirectMessage("bob@example.com", "retain until commit", { id: "dm-delete-fail" });
    await queue.flushDirect();
    const applyTerminal = store.applyTerminal.bind(store);
    let attempts = 0;
    const executors: OutboundOwnerContext[] = [];
    store.applyTerminal = async (executor, intent) => {
      attempts += 1;
      executors.push({ ...executor });
      if (attempts === 1) {
        return {
          kind: "failed",
          reason: "aborted",
          cause: new DOMException("terminal apply aborted", "AbortError"),
        };
      }
      return applyTerminal(executor, intent);
    };

    const acknowledgement = queue.handleAck("dm-delete-fail");
    await Promise.resolve();
    expect(acknowledgements).toEqual([]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-delete-fail"]);
    await acknowledgement;

    expect(attempts).toBe(2);
    expect(executors).toHaveLength(2);
    expect(executors[1]).toEqual(executors[0]);
    expect(acknowledgements).toEqual(["dm-delete-fail"]);
    expect(listQueuedMessages(SCOPE)).toEqual([]);
  });

  test("two same-account tabs cannot claim and send the same durable row", async () => {
    const store = new MemoryDurableOutboundStore();
    const sends: string[] = [];
    const sendDirect = async (_peer: string, _body: string, opts: { id: string }) => {
      sends.push(opts.id);
      return opts.id;
    };
    const first = (await createActiveQueue({ durableStore: store, sendDirect })).queue;
    await first.queueDirectMessage("bob@example.com", "once", { id: "dm-cross-tab" });
    const second = (await createActiveQueue({ durableStore: store, sendDirect })).queue;

    await Promise.all([first.flushDirect(), second.flushDirect()]);

    expect(sends).toEqual(["dm-cross-tab"]);
  });

  test("non-retryable send failures are discarded instead of retried forever", async () => {
    const attempts: string[] = [];
    const { queue, events } = await createActiveQueue({
      sendDirect: async (_peer, body, _opts) => {
        attempts.push(body);
        if (body === "bad recipient") return wasmSendMessageId({ kind: "invalid-recipient" });
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
    const { queue } = await createActiveQueue({
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

  test("noncanonical room ingress coalesces canonical flushes without orphaning rows", async () => {
    const canonicalRoom = "general@muc.example";
    const readyChecks: string[] = [];
    const sends: Array<{ roomJid: string; id: string }> = [];
    let releaseFirst!: () => void;
    let markFirstStarted!: () => void;
    let markSecondStarted!: () => void;
    const firstReleased = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const firstStarted = new Promise<void>((resolve) => {
      markFirstStarted = resolve;
    });
    const secondStarted = new Promise<void>((resolve) => {
      markSecondStarted = resolve;
    });
    expect(committedOrThrow(
      "seed-noncanonical-first",
      await durableStore.persistReady(SCOPE, {
        kind: "room",
        id: "canonical-first",
        createdAt: "2026-07-18T00:00:00.000Z",
        roomJid: " General@MUC.Example/First ",
        body: "first",
      }),
    ).kind).toBe("inserted");
    expect(committedOrThrow(
      "seed-noncanonical-second",
      await durableStore.persistReady(SCOPE, {
        kind: "room",
        id: "canonical-second",
        createdAt: "2026-07-18T00:00:00.001Z",
        roomJid: "GENERAL@muc.example/Second",
        body: "second",
      }),
    ).kind).toBe("inserted");
    const { queue } = await createActiveQueue({
      roomIsReady: (roomJid) => {
        readyChecks.push(roomJid);
        return roomJid === canonicalRoom;
      },
      sendRoom: async (roomJid, _body, opts) => {
        sends.push({ roomJid, id: opts.id });
        if (opts.id === "canonical-first") {
          markFirstStarted();
          await firstReleased;
        } else {
          markSecondStarted();
        }
        return opts.id;
      },
    });

    expect(listQueuedMessages(SCOPE).map((message) => ({
      id: message.id,
      roomJid: message.kind === "room" ? message.roomJid : null,
    }))).toEqual([
      { id: "canonical-first", roomJid: canonicalRoom },
      { id: "canonical-second", roomJid: canonicalRoom },
    ]);

    const firstFlush = queue.flushRoom(" GENERAL@MUC.Example/Caller ");
    const coalescedFlush = queue.flushRoom(canonicalRoom);
    await firstStarted;
    expect(sends).toEqual([
      { roomJid: canonicalRoom, id: "canonical-first" },
    ]);
    releaseFirst();
    await Promise.all([firstFlush, coalescedFlush]);
    expect(sends).toHaveLength(1);

    await queue.handleAck("canonical-first");
    await secondStarted;
    await queue.whenQuiescent();
    expect(sends).toEqual([
      { roomJid: canonicalRoom, id: "canonical-first" },
      { roomJid: canonicalRoom, id: "canonical-second" },
    ]);

    await queue.handleAck("canonical-second");
    await queue.whenQuiescent();
    expect(committedOrThrow(
      "canonical-room-list",
      await durableStore.list(SCOPE),
    )).toEqual([]);
    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(readyChecks.length).toBeGreaterThan(0);
    expect(readyChecks.every((roomJid) => roomJid === canonicalRoom)).toBe(true);
  });

  test("queueing while connected (room not joined) does not emit a reconnecting status (#1164)", async () => {
    const { queue, statuses } = await createActiveQueue({
      canUseConnectedSession: () => true,
      roomIsReady: () => false,
    });

    await queue.queueRoomMessage("general@muc.example.com", "hello", { id: "room-q1" });

    // The message still queues, but the global banner must stay online.
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["room-q1"]);
    expect(statuses).toHaveLength(0);
  });

  test("queueing while the session is unusable still reports the reconnecting status", async () => {
    const { queue, statuses } = await createActiveQueue({
      canUseConnectedSession: () => false,
    });

    await queue.queueDirectMessage("bob@example.com", "hello", { id: "dm-q1" });

    expect(statuses).toEqual([
      { state: "reconnecting", detail: "Message queued until the connection returns" },
    ]);
  });

  test("resume reconciliation tracks XEP-0198 replayed stanza ids so acks clear the store", async () => {
    const { queue, events } = await createActiveQueue();
    const depths: QueueDepthTelemetry[] = [];
    events.on("queueDepthChange", (depth) => depths.push(depth));

    await queue.queueDirectMessage("bob@example.com", "native replay", { id: "dm-native" });
    await queue.reconcileNativeSnapshot(1, {
      previd: "p",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [messageResumeEntry("dm-native")],
    }, "resume-replay");

    await queue.handleAck("dm-native");

    expect(listQueuedMessages(SCOPE)).toHaveLength(0);
    expect(depths.slice(-2)).toEqual([
      { kind: "dm", persisted: 0, inflight: 0 },
      { kind: "room", persisted: 0, inflight: 0 },
    ]);
  });

  test("failed resume retains fallback ownership until a fresh bind releases it", async () => {
    const sent: string[] = [];
    const { queue, events } = await createActiveQueue({
      sendDirect: async (_peer, _body, opts) => {
        sent.push(opts.id);
        return opts.id;
      },
    });
    const failures: string[] = [];
    const depths: QueueDepthTelemetry[] = [];
    events.on("messageDeliveryFailure", (id) => failures.push(id));
    events.on("queueDepthChange", (depth) => depths.push(depth));

    await queue.queueDirectMessage("bob@example.com", "native fallback", { id: "dm-native-fallback" });
    await queue.reconcileNativeSnapshot(1, {
      previd: "p",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [messageResumeEntry("dm-native-fallback")],
    }, "resume-replay");

    await queue.handleFailed("dm-native-fallback");
    await queue.flushDirect();
    expect(sent).toEqual([]);
    expect(failures).toEqual([]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-native-fallback"]);
    expect(latestQueueDepths(depths)).toMatchObject({
      dm: { kind: "dm", persisted: 1, inflight: 1 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });

    await queue.reconcileFreshSession(1, null);
    await queue.flushDirect();

    expect(sent).toEqual(["dm-native-fallback"]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-native-fallback"]);
    expect(latestQueueDepths(depths)).toMatchObject({
      dm: { kind: "dm", persisted: 1, inflight: 1 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });

    await queue.handleAck("dm-native-fallback");
    expect(listQueuedMessages(SCOPE)).toEqual([]);
    expect(latestQueueDepths(depths)).toEqual({
      dm: { kind: "dm", persisted: 0, inflight: 0 },
      room: { kind: "room", persisted: 0, inflight: 0 },
    });
  });

  test("a plain fresh bind releases an unattempted resume replay claim to the durable browser queue", async () => {
    const sent: string[] = [];
    const { queue } = await createActiveQueue({
      sendDirect: async (_peer, _body, opts) => {
        sent.push(opts.id);
        return opts.id;
      },
    });
    await queue.queueDirectMessage("bob@example.com", "fresh without SM", { id: "dm-no-resume" });
    await queue.reconcileNativeSnapshot(1, {
      previd: "p",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [messageResumeEntry("dm-no-resume")],
    }, "resume-replay");

    await queue.reconcileFreshSession(1, null);
    await queue.flushDirect();

    expect(sent).toEqual(["dm-no-resume"]);
    expect(listQueuedMessages(SCOPE).map((entry) => entry.id)).toEqual(["dm-no-resume"]);
  });

  test("a reconstructed queue reclaims a retained failed-resume row after an immediate or mid-write crash", async () => {
    let authorityNow = Date.now();
    const store = new MemoryDurableOutboundStore({
      now: () => authorityNow,
    });
    const original = (await createActiveQueue({ durableStore: store })).queue;
    await original.queueDirectMessage("bob@example.com", "survive fallback crash", { id: "dm-crash-fallback" });
    await original.reconcileNativeSnapshot(1, {
      previd: "p",
      inboundH: 3,
      outboundH: 4,
      unhandledOutboundEntries: [messageResumeEntry("dm-crash-fallback")],
    }, "resume-replay");
    await original.handleFailed("dm-crash-fallback");
    // Simulate the crashed runtime becoming inert without releasing or
    // renewing its durable owner/row claims.
    original.beginFinalDisposal();
    authorityNow += OUTBOUND_CLAIM_LEASE_MS + 1;

    const reclaimed: string[] = [];
    const reconstructed = (await createActiveQueue({
      durableStore: store,
      sendDirect: async (_peer, _body, opts) => {
        reclaimed.push(opts.id);
        return opts.id;
      },
    })).queue;

    await reconstructed.flushDirect();

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
  const lifecycleId = XmppLifecycleId.create();
  const persistence: ResumePersistence = {
    lifecycleId,
    durableRuntimeStore: new MemoryDurableOutboundStore(),
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
    outboundOwnerHint: () => ({
      ownerId: "recording-owner",
      ownerInstanceId: lifecycleId.value,
    }),
    acceptOutboundOwner: () => undefined,
    preparePagehideHandoff: async (state) => {
      calls.push("preparePagehideHandoff");
      saved = state;
      return {
        kind: "committed",
        value: {
          handoff: {
            token: "recording-handoff",
            expiresAt: Date.now() + 30_000,
            authorityEpoch: 1,
            ownerGeneration: 1,
          },
          smVersion: 1,
        },
      };
    },
    publishPagehideHandoff: () => calls.push("publishPagehideHandoff"),
    reclaimPagehideOwnership: async () => {
      calls.push("reclaimPagehideOwnership");
      return { kind: "committed", value: undefined };
    },
    loadJoinedRooms: () => [],
    saveJoinedRooms: () => undefined,
    clearJoinedRooms: () => calls.push("clearJoinedRooms"),
  };
  return { persistence, calls, getSaved: () => saved };
}

describe("ResumeStateStore", () => {
  test("page lifecycle quiescence drains a rejected wave before reporting each failure once", async () => {
    const { persistence } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    const state = store as unknown as {
      enqueuePageLifecycle: (operation: () => Promise<void>) => void;
    };
    const firstError = new Error("first page lifecycle wave failed");
    const secondError = new Error("second page lifecycle wave failed");
    let releaseFirst!: () => void;
    let releaseSecond!: () => void;
    let markSecondStarted!: () => void;
    const firstGate = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const secondGate = new Promise<void>((resolve) => {
      releaseSecond = resolve;
    });
    const secondStarted = new Promise<void>((resolve) => {
      markSecondStarted = resolve;
    });
    state.enqueuePageLifecycle(async () => {
      await firstGate;
      state.enqueuePageLifecycle(async () => {
        await secondGate;
        throw secondError;
      });
      markSecondStarted();
      throw firstError;
    });

    let settled = false;
    const barrier = store.whenPageLifecycleQuiescent().finally(() => {
      settled = true;
    });
    releaseFirst();
    await secondStarted;
    expect(settled).toBe(false);

    releaseSecond();
    const [outcome] = await Promise.allSettled([barrier]);
    expect(outcome?.status).toBe("rejected");
    if (outcome?.status !== "rejected") throw new Error("barrier unexpectedly resolved");
    expect(outcome.reason).toBeInstanceOf(AggregateError);
    const failures = (outcome.reason as AggregateError).errors;
    expect(failures.filter((error) => error === firstError)).toHaveLength(1);
    expect(failures.filter((error) => error === secondError)).toHaveLength(1);
  });

  test("persistForPageHide snapshots the live state with the session resource", async () => {
    const { persistence, calls, getSaved } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    let persistedRooms = 0;

    store.persistForPageHide(
      { previd: "p1", inboundH: 3, outboundH: 4, unhandledOutboundEntries: [] },
      "web-abc",
      () => { persistedRooms += 1; },
    );
    await store.whenPageLifecycleQuiescent();

    expect(calls).toEqual([
      "preparePagehideHandoff",
      "publishPagehideHandoff",
    ]);
    expect(getSaved()).toEqual({
      previd: "p1",
      inboundH: 3,
      outboundH: 4,
      unhandledOutboundEntries: [],
      resource: "web-abc",
    });
    expect(persistedRooms).toBe(1);
  });

  test("persistForPageHide rejects a snapshot without the ordered entry array", () => {
    const { persistence, calls } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    let persistedRooms = 0;

    expect(() => store.persistForPageHide(
      { previd: "p1", inboundH: 3, outboundH: 4 } as never,
      "web-abc",
      () => { persistedRooms += 1; },
    )).toThrow("unhandledOutboundEntries must be an ordered array");

    expect(calls).toEqual([]);
    expect(store.state).toBeNull();
    expect(persistedRooms).toBe(0);
  });

  test("captureFromDisconnect stamps the resource and keeps state in memory only", async () => {
    const { persistence, calls } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);

    const captured = store.captureFromDisconnect(
      { get_resume_state: () => ({
        previd: "p2",
        inboundH: 1,
        outboundH: 2,
        unhandledOutboundEntries: [],
      }) },
      "web-xyz",
    );

    expect(captured).toEqual({
      previd: "p2",
      inboundH: 1,
      outboundH: 2,
      unhandledOutboundEntries: [],
      resource: "web-xyz",
    });
    expect(store.state).toEqual(captured);
    // In-memory only: ordinary disconnects never touch the persisted slot.
    expect(calls).toEqual([]);
  });

  test("discardState clears the persisted slot; clearAll also drops joined rooms", async () => {
    const { persistence, calls } = createRecordingPersistence();
    const store = new ResumeStateStore(persistence);
    store.captureFromDisconnect(
      { get_resume_state: () => ({
        previd: "p3",
        inboundH: 0,
        outboundH: 0,
        unhandledOutboundEntries: [],
      }) },
      "web-r",
    );

    await store.discardState();
    expect(store.state).toBeNull();
    expect(calls).toEqual(["clearSm"]);

    await store.clearAll();
  });

  test("clearAll attempts every teardown and aggregates typed stage failures", async () => {
    const { persistence } = createRecordingPersistence();
    const smFailure = new DOMException("SM clear failed", "AbortError");
    const roomsFailure = new Error("joined rooms clear failed");
    persistence.clearSm = async () => ({
      kind: "failed",
      reason: "aborted",
      cause: smFailure,
    });
    persistence.clearJoinedRooms = () => {
      throw roomsFailure;
    };
    const store = new ResumeStateStore(persistence);

    let failure: unknown;
    try {
      await store.clearAll();
    } catch (error) {
      failure = error;
    }
    expect(failure).toBeInstanceOf(ResumeStateTeardownError);
    const teardown = failure as ResumeStateTeardownError;
    expect(teardown.failures.map(({ stage }) => stage)).toEqual([
      "sm-clear",
      "joined-rooms-clear",
    ]);
    expect(teardown.failures[0]?.cause).toBeInstanceOf(OutboundPersistenceError);
    const persistenceFailure = teardown.failures[0]?.cause as OutboundPersistenceError;
    expect(persistenceFailure.operation).toBe("sm-clear-all");
    expect(persistenceFailure.reason).toBe("aborted");
    expect(persistenceFailure.cause).toBe(smFailure);
    expect(teardown.failures[1]?.cause).toBe(roomsFailure);
  });
});
