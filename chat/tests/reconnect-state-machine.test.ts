/**
 * #1164: honest reconnect state machine.
 *
 * The client must distinguish recoverable drops (keep retrying, show
 * "reconnecting") from terminal failures (auth failure / resource
 * conflict / retry exhaustion — emit `state: "error"` and stop), and
 * a stalled connect attempt must tear down and reschedule instead of
 * silently rejecting.
 *
 * These tests drive `wireEvents` with a stub WASM handle (the same
 * harness pattern as `xmpp-telemetry.test.ts`) and observe the
 * user-facing state through the client's status handler.
 */
import { afterEach, describe, expect, test } from "bun:test";
import type { WaddleResumeStateSnapshot } from "@waddle/xmpp-client-wasm";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import {
  OutboundAuthorityChangedError,
  type TimeoutScheduler,
} from "../src/lib/xmpp/client-connection";
import type { ReconnectCatchupEntry } from "../src/lib/xmpp/reconnect-catchup";
import type { XmppStatusSnapshot } from "../src/lib/xmpp/types";
import type { WasmControlErrorPayload as StreamErrorPayload } from "../src/lib/xmpp/wasm-types";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";
import {
  noopWasmClientCallbacks,
  type WasmClientCallbacks,
} from "./helpers/wasm-client-callbacks";

type StubXmpp = WasmClientCallbacks & {
  set_on_error: (cb: (payload: StreamErrorPayload) => void) => void;
  set_on_disconnected: (cb: () => void) => void;
  get_resume_state: () => null;
  disconnect: () => Promise<void>;
  dispose: () => void;
};

type PrivateState = {
  xmpp: unknown;
  connectEpoch: number;
  connected: boolean;
  wireEvents: (xmpp: StubXmpp) => void;
  reconnect: { clearTimer: () => void };
  connectWithFreshBudget: () => Promise<void>;
  connectFromScheduler: () => Promise<void>;
  handleDisconnected: (
    xmpp: unknown,
    generation?: number,
    error?: Error,
  ) => void;
  handleOutboundAuthorityLost: (
    error: OutboundAuthorityChangedError,
  ) => void;
  handleMessage: (message: unknown) => void;
  pendingDuringResume: unknown[] | null;
  carriedPendingDuringResume: unknown[];
  resumeBarrier: {
    xmpp: unknown;
    generation: number;
    promise: Promise<void>;
  } | null;
  connectTimeoutMs: number;
  doConnect: () => Promise<void>;
  loadModule: () => Promise<unknown>;
  runSessionReady: (xmpp: unknown, lifecycle: { type: "fresh" | "resumed" }) => Promise<void>;
  runReconnectCatchup: (...args: unknown[]) => Promise<void>;
  catchup: { onSessionStarted: () => ReconnectCatchupEntry[] };
  performDisconnect: () => Promise<void>;
  startConnectAttempt: () => Promise<void>;
  disconnectForLifecycle: () => Promise<void>;
  trackLifecycleWork: <T>(work: Promise<T>) => Promise<T>;
  whenLifecycleQuiescent: () => Promise<void>;
  outboundQueueHydration: Promise<void>;
  outboundQueue: {
    connectionGeneration: number;
    beginConnectionGeneration: (generation: number) => number;
    dispose: () => Promise<void>;
    reconcileNativeSnapshot: (
      generation: number,
      state: unknown,
    ) => Promise<unknown>;
    whenQuiescent: () => Promise<void>;
  };
  lifecycleState: "active" | "disposing" | "disposed";
};

function deferred<T = void>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

class ControllableTimeoutScheduler implements TimeoutScheduler {
  private nowMs = 0;
  private nextId = 1;
  private readonly tasks = new Map<number, { at: number; callback: () => void }>();

  setTimeout(callback: () => void, delayMs: number): unknown {
    const id = this.nextId;
    this.nextId += 1;
    this.tasks.set(id, { at: this.nowMs + Math.max(0, delayMs), callback });
    return id;
  }

  clearTimeout(handle: unknown): void {
    if (typeof handle === "number") this.tasks.delete(handle);
  }

  advanceBy(delayMs: number): void {
    const target = this.nowMs + delayMs;
    for (;;) {
      const next = [...this.tasks.entries()]
        .filter(([, task]) => task.at <= target)
        .sort((left, right) => left[1].at - right[1].at || left[0] - right[0])[0];
      if (!next) break;
      const [id, task] = next;
      this.tasks.delete(id);
      this.nowMs = task.at;
      task.callback();
    }
    this.nowMs = target;
  }
}

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

const createdClients: BrowserXmppClient[] = [];

function createTestClient(
  timeoutScheduler = new ControllableTimeoutScheduler(),
): BrowserXmppClient {
  const client = new BrowserXmppClient(session(), {
    durableRuntimeStore: new MemoryDurableOutboundStore(),
    timeoutScheduler,
  });
  createdClients.push(client);
  return client;
}

async function bindCurrentGeneration(
  state: PrivateState,
): Promise<void> {
  await state.outboundQueueHydration;
  await state.outboundQueue.whenQuiescent();
  state.outboundQueue.beginConnectionGeneration(state.connectEpoch);
}

afterEach(async () => {
  for (const client of createdClients) {
    const state = client as unknown as PrivateState;
    state.reconnect.clearTimer();
    await state.outboundQueueHydration;
    await state.outboundQueue.whenQuiescent();
    state.connectEpoch = state.outboundQueue.connectionGeneration;
    state.xmpp = null;
    state.connected = false;
  }
  await Promise.all(createdClients.map((client) => client.dispose()));
  createdClients.length = 0;
});

function bufferedDirectMessage(id: string) {
  return {
    mam_id: id,
    id,
    from: "bob@example.com/phone",
    to: "alice@example.com/desktop",
    message_type: "chat",
    body: "buffered once",
    timestamp: "2026-07-17T12:00:00.000Z",
    reaction_emojis: [],
    shared_files: [],
    is_muc: false,
  };
}

function createHarness(
  timeoutScheduler = new ControllableTimeoutScheduler(),
) {
  const client = createTestClient(timeoutScheduler);
  const state = client as unknown as PrivateState;
  const statuses: XmppStatusSnapshot[] = [];
  const scheduled: Array<{ attempt: number; delayMs: number }> = [];
  client.setStatusHandler((snapshot) => statuses.push(snapshot));
  client.onReconnectScheduled((info) => scheduled.push(info));

  let fireError: (payload: StreamErrorPayload) => void = () => {};
  let fireDisconnected: () => void = () => {};
  const stub: StubXmpp = {
    ...noopWasmClientCallbacks(),
    set_on_error(cb) { fireError = cb; },
    set_on_disconnected(cb) { fireDisconnected = cb; },
    get_resume_state: () => null,
    disconnect: async () => undefined,
    dispose: () => undefined,
  };
  state.xmpp = stub;
  state.connected = true;
  state.wireEvents(stub);
  const initialGeneration = state.connectEpoch;
  const ready = state.outboundQueueHydration.then(() => {
    state.outboundQueue.beginConnectionGeneration(initialGeneration);
  });
  const installSuccessor = async () => {
    await bindCurrentGeneration(state);
    state.xmpp = stub;
    state.connected = true;
    state.wireEvents(stub);
  };

  return {
    client,
    state,
    stub,
    statuses,
    scheduled,
    timeoutScheduler,
    ready,
    installSuccessor,
    fireError: (payload: StreamErrorPayload) => fireError(payload),
    fireDisconnected: () => fireDisconnected(),
  };
}

function createDeterministicClient() {
  const timeoutScheduler = new ControllableTimeoutScheduler();
  return {
    client: createTestClient(timeoutScheduler),
    timeoutScheduler,
  };
}

function strictGeneratedMethodStubs() {
  return {
    ...noopWasmClientCallbacks(),
    request_stream_management_ack: async () => undefined,
    send_chat_message: async (
      _peerJid: string,
      _body: string,
      options: { stanza_id?: string },
    ) => ({ kind: "sent" as const, stanza_id: options.stanza_id ?? "test-dm" }),
    send_groupchat_message: async (
      _roomJid: string,
      _body: string,
      options: { stanza_id?: string },
    ) => ({ kind: "sent" as const, stanza_id: options.stanza_id ?? "test-room" }),
  };
}

describe("terminal error state (#1164 slice 1)", () => {
  test("a not-authorized stream error reaches state \"error\" and never schedules a retry", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ kind: "stream-error", condition: "not-authorized" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("error");
    expect(scheduled).toHaveLength(0);
    state.reconnect.clearTimer();
  });

  test("a resource conflict is terminal too", () => {
    const { statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ kind: "stream-error", condition: "conflict" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("error");
    expect(scheduled).toHaveLength(0);
  });

  test("a recoverable stream error keeps the reconnecting loop", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ kind: "stream-error", condition: "system-shutdown" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });
});

describe("retry-loop cap (#1164 slice 2)", () => {
  test("exhausting the reconnect attempts flips the client to terminal error", async () => {
    const { state, statuses, scheduled, fireDisconnected, installSuccessor } = createHarness();

    for (let i = 0; i < 11; i += 1) {
      // Re-arm the handle + drop the pending timer so each disconnect
      // is accepted and each schedule() call actually runs.
      state.reconnect.clearTimer();
      await installSuccessor();
      fireDisconnected();
    }

    // 10 scheduled attempts, then exhaustion: no 11th schedule.
    expect(scheduled).toHaveLength(10);
    expect(statuses.at(-1)?.state).toBe("error");
    state.reconnect.clearTimer();
  });
});

describe("room-scoped queueing keeps the global banner honest (#1164 slice 4)", () => {
  test("sendGroupMessage to an unjoined room while connected leaves the status untouched", async () => {
    const { client, statuses } = createHarness();

    const result = await client.sendGroupMessage("space-1", "general", "hello");

    expect(result?.state).toBe("queued");
    expect(statuses).toHaveLength(0);
  });
});

describe("connect budget measures to session-ready, not catch-up completion (C1)", () => {
  test("a reconnect catch-up outliving connectTimeoutMs does not tear down the established session", async () => {
    const timeoutScheduler = new ControllableTimeoutScheduler();
    const client = createTestClient(timeoutScheduler);
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));
    const bodies: string[] = [];
    client.setDirectMessageHandler((message) => bodies.push(message.body));

    state.connectTimeoutMs = 20;
    const catchupStarted = deferred();
    const releaseCatchup = deferred();
    state.runReconnectCatchup = async () => {
      catchupStarted.resolve(undefined);
      await releaseCatchup.promise;
    };
    state.catchup.onSessionStarted = () => [{ kind: "dm", key: "bob@example.com", scope: "account" }];
    let stalledHandleDisconnects = 0;
    const handle = {
      disconnect: async () => { stalledHandleDisconnects += 1; },
      dispose: () => undefined,
    };
    state.doConnect = async () => {
      state.xmpp = handle;
      void state.runSessionReady(handle, { type: "fresh" });
    };

    await bindCurrentGeneration(state);
    let settled: "resolved" | "rejected" | null = null;
    const connected = client.connect().then(
      () => { settled = "resolved"; },
      () => { settled = "rejected"; },
    );

    await catchupStarted.promise;
    // A live DM arriving mid-barrier is buffered for the drain.
    state.handleMessage({
      id: "dm-1",
      from: "bob@example.com/phone",
      to: "alice@example.com/desktop",
      message_type: "chat",
      body: "hi",
      timestamp: "2026-07-05T10:00:00.000Z",
      reaction_emojis: [],
      shared_files: [],
      is_muc: false,
    });

    // Advance past the connect budget while the barrier is still open.
    timeoutScheduler.advanceBy(40);
    expect(state.xmpp).toBe(handle);
    expect(stalledHandleDisconnects).toBe(0);
    expect(statuses.some((snapshot) => snapshot.state === "reconnecting")).toBe(false);

    releaseCatchup.resolve(undefined);
    await connected;
    expect(settled).toBe("resolved");
    expect(state.connected).toBe(true);
    expect(bodies).toEqual(["hi"]);
    state.reconnect.clearTimer();
  });
});

describe("exhaustion recovery (C2)", () => {
  test("a fresh-budget connect after exhaustion restores the retry budget", async () => {
    const { state, scheduled, fireDisconnected, installSuccessor } = createHarness();
    for (let i = 0; i < 11; i += 1) {
      state.reconnect.clearTimer();
      await installSuccessor();
      fireDisconnected();
    }
    expect(scheduled).toHaveLength(10);

    // User-explicit recovery (the online listener's path): the attempt
    // itself fails, but the budget is fresh — the next disconnect
    // schedules attempt 1 again instead of re-exhausting instantly.
    state.doConnect = async () => { throw new Error("still down"); };
    await state.connectWithFreshBudget().catch(() => undefined);
    state.reconnect.clearTimer();
    await installSuccessor();
    fireDisconnected();

    expect(scheduled).toHaveLength(11);
    expect(scheduled.at(-1)?.attempt).toBe(1);
    state.reconnect.clearTimer();
  });

  test("exhaustion while the browser is offline reports offline, not terminal error", async () => {
    const originalNavigator = globalThis.navigator;
    Object.defineProperty(globalThis, "navigator", {
      value: { onLine: false },
      configurable: true,
    });
    try {
      const { state, statuses, fireDisconnected, installSuccessor } = createHarness();
      for (let i = 0; i < 11; i += 1) {
        state.reconnect.clearTimer();
        await installSuccessor();
        fireDisconnected();
      }
      expect(statuses.at(-1)?.state).toBe("offline");
      state.reconnect.clearTimer();
    } finally {
      Object.defineProperty(globalThis, "navigator", {
        value: originalNavigator,
        configurable: true,
      });
    }
  });

  test("a window online event after exhaustion triggers a fresh-budget connect and clears terminal state", async () => {
    const listeners: Record<string, Array<() => void>> = {};
    const globals = globalThis as { window?: unknown };
    const originalWindow = globals.window;
    globals.window = {
      addEventListener: (name: string, cb: () => void) => { (listeners[name] ??= []).push(cb); },
      removeEventListener: (name: string, cb: () => void) => {
        listeners[name] = (listeners[name] ?? []).filter((l) => l !== cb);
      },
    };
    try {
      const { state, scheduled, fireDisconnected, installSuccessor } = createHarness();
      const onlineAttemptStarted = deferred();
      state.doConnect = async () => {
        onlineAttemptStarted.resolve(undefined);
        throw new Error("still down");
      };
      for (let i = 0; i < 11; i += 1) {
        state.reconnect.clearTimer();
        await installSuccessor();
        fireDisconnected();
      }
      expect(scheduled).toHaveLength(10);

      // Network returns: the listener fires the fresh-budget path.
      for (const cb of listeners.online ?? []) cb();
      await onlineAttemptStarted.promise;

      // Fresh budget: the next disconnect schedules attempt 1 again —
      // the exhausted/terminal gate no longer blocks the retry loop.
      state.reconnect.clearTimer();
      await installSuccessor();
      fireDisconnected();
      expect(scheduled).toHaveLength(11);
      expect(scheduled.at(-1)?.attempt).toBe(1);
      state.reconnect.clearTimer();
    } finally {
      if (originalWindow === undefined) delete globals.window;
      else globals.window = originalWindow;
    }
  });
});

describe("internal connect() must not leak the fresh-budget reset (F1)", () => {
  test("repeated internal connect() during an outage does not reset the attempt counter", async () => {
    const { client, state, scheduled, fireDisconnected, installSuccessor } = createHarness();
    state.doConnect = async () => { throw new Error("still down"); };
    for (let i = 0; i < 3; i += 1) {
      state.reconnect.clearTimer();
      await installSuccessor();
      fireDisconnected();
    }
    expect(scheduled.at(-1)?.attempt).toBe(3);

    // Background triggers (idle tracker → setPresence, MAM pagers,
    // sendGroupMessage fallback) all route through connect() during
    // the outage — the budget must keep counting up, not restart.
    await client.connect().catch(() => undefined);
    await client.connect().catch(() => undefined);
    state.reconnect.clearTimer();
    await installSuccessor();
    fireDisconnected();

    expect(scheduled.at(-1)?.attempt).toBe(4);
    state.reconnect.clearTimer();
  });

  test("internal connect() while terminal-error rejects without clearing the error state or scheduling", async () => {
    const { client, state, statuses, scheduled, fireError, fireDisconnected } = createHarness();
    let attempts = 0;
    state.connectTimeoutMs = 10;
    state.doConnect = async () => { attempts += 1; };

    fireError({ kind: "stream-error", condition: "not-authorized" });
    fireDisconnected();
    expect(statuses.at(-1)?.state).toBe("error");
    const statusCount = statuses.length;

    await expect(client.connect()).rejects.toThrow();

    expect(attempts).toBe(0);
    expect(scheduled).toHaveLength(0);
    // No status churn: the terminal "sign in again" error stays put.
    expect(statuses).toHaveLength(statusCount);
    expect(statuses.at(-1)?.state).toBe("error");
    state.reconnect.clearTimer();
  });

  test("internal connect() after exhaustion rejects without restarting the retry loop", async () => {
    const { client, state, statuses, scheduled, fireDisconnected, installSuccessor } = createHarness();
    for (let i = 0; i < 11; i += 1) {
      state.reconnect.clearTimer();
      await installSuccessor();
      fireDisconnected();
    }
    expect(scheduled).toHaveLength(10);
    expect(statuses.at(-1)?.state).toBe("error");
    const statusCount = statuses.length;

    let attempts = 0;
    state.connectTimeoutMs = 10;
    state.doConnect = async () => { attempts += 1; };
    await expect(client.connect()).rejects.toThrow();

    expect(attempts).toBe(0);
    expect(scheduled).toHaveLength(10);
    expect(statuses).toHaveLength(statusCount);
    state.reconnect.clearTimer();
  });
});

describe("background connect() must not preempt an armed backoff timer (F5)", () => {
  test("connect() while a retry timer is armed fast-rejects without cancelling the timer or launching an attempt", async () => {
    const { client, state, scheduled, fireDisconnected } = createHarness();
    let attempts = 0;
    state.connectTimeoutMs = 50;
    state.doConnect = async () => { attempts += 1; };

    // Outage begins: the loop arms its first backoff timer.
    fireDisconnected();
    expect(scheduled).toHaveLength(1);
    const scheduler = state.reconnect as unknown as { timer: unknown; attempt: number };
    expect(scheduler.timer).not.toBeNull();

    // A background trigger (typing chat-state via requireConnectedXmpp,
    // presence, room switch, MAM pager) calls connect(): it must leave
    // the schedule intact and start nothing.
    await expect(client.connect()).rejects.toThrow("XMPP session is not ready");
    expect(attempts).toBe(0);
    expect(scheduler.timer).not.toBeNull();
    expect(scheduler.attempt).toBe(1);
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });

  test("repeated background connects during a short outage never burn the budget into terminal error", async () => {
    const { client, state, stub, statuses, scheduled, fireDisconnected } = createHarness();
    // Every launched attempt fails fast: the socket opens, then drops —
    // re-entering handleDisconnected → schedule() → attempt++.
    state.doConnect = async () => {
      state.xmpp = stub;
      state.connected = true;
      state.handleDisconnected(
        stub,
        state.connectEpoch,
        new Error("still down"),
      );
    };

    // Outage begins: attempt 1 armed.
    fireDisconnected();
    expect(scheduled).toHaveLength(1);

    // Ten user interactions while the backoff timer is armed (<1 min of
    // typing) must not exhaust the 10-attempt budget.
    for (let i = 0; i < 10; i += 1) {
      await client.connect().catch(() => undefined);
    }

    expect(scheduled).toHaveLength(1);
    expect(statuses.some((snapshot) => snapshot.state === "error")).toBe(false);
    expect(statuses.at(-1)?.state).toBe("reconnecting");
    state.reconnect.clearTimer();
  });
});

describe("disconnect() cancels the pending connect attempt (F3)", () => {
  test("a concurrent connect waits for the active disconnect to settle", async () => {
    const { client } = createDeterministicClient();
    const state = client as unknown as PrivateState;
    const order: string[] = [];
    let releaseDisconnect!: () => void;
    const disconnectGate = new Promise<void>((resolve) => {
      releaseDisconnect = resolve;
    });
    state.performDisconnect = async () => {
      state.xmpp = null;
      state.connected = false;
      order.push("disconnect-start");
      await disconnectGate;
      order.push("disconnect-finish");
    };
    state.startConnectAttempt = async () => {
      order.push("connect-start");
    };

    const disconnect = client.disconnect();
    const connect = client.connect();
    await Promise.resolve();
    expect(order).toEqual(["disconnect-start"]);

    releaseDisconnect();
    await Promise.all([disconnect, connect]);

    expect(order).toEqual([
      "disconnect-start",
      "disconnect-finish",
      "connect-start",
    ]);
  });

  test("final disposal waits for tracked lifecycle work before disposing authority", async () => {
    const { client } = createDeterministicClient();
    const state = client as unknown as PrivateState;
    const order: string[] = [];
    let releaseLifecycle!: () => void;
    const lifecycleGate = new Promise<void>((resolve) => {
      releaseLifecycle = resolve;
    });
    state.disconnectForLifecycle = async () => {
      order.push("disconnect");
    };
    const disposeOutboundQueue = state.outboundQueue.dispose.bind(
      state.outboundQueue,
    );
    state.outboundQueue.dispose = async () => {
      order.push("outbound-dispose");
      await disposeOutboundQueue();
    };
    void state.trackLifecycleWork(lifecycleGate.then(() => {
      order.push("lifecycle-finish");
    }));

    const disposal = client.dispose();
    await Promise.resolve();
    expect(order).toEqual(["disconnect"]);
    expect(state.lifecycleState).toBe("disposing");

    releaseLifecycle();
    await disposal;

    expect(order).toEqual([
      "disconnect",
      "lifecycle-finish",
      "outbound-dispose",
    ]);
    expect(state.lifecycleState).toBe("disposed");
  });

  test("lifecycle quiescence observes work registered by a settling first wave", async () => {
    const { client } = createDeterministicClient();
    const state = client as unknown as PrivateState;
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
    const order: string[] = [];
    void state.trackLifecycleWork(firstGate.then(() => {
      order.push("first");
      void state.trackLifecycleWork(secondGate.then(() => {
        order.push("second");
      }));
      markSecondStarted();
    }));

    let quiescent = false;
    const barrier = state.whenLifecycleQuiescent().then(() => {
      quiescent = true;
    });
    releaseFirst();
    await secondStarted;
    expect(quiescent).toBe(false);

    releaseSecond();
    await barrier;
    expect(order).toEqual(["first", "second"]);
    expect(quiescent).toBe(true);
  });

  test("lifecycle quiescence drains a rejected wave before reporting each failure once", async () => {
    const { client } = createDeterministicClient();
    const state = client as unknown as PrivateState;
    const firstError = new Error("first lifecycle wave failed");
    const secondError = new Error("second lifecycle wave failed");
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
    void state.trackLifecycleWork(firstGate.then(() => {
      void state.trackLifecycleWork(secondGate.then(() => {
        throw secondError;
      }));
      markSecondStarted();
      throw firstError;
    }));

    let settled = false;
    const barrier = state.whenLifecycleQuiescent().finally(() => {
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

  test("real connect B waits for A catch-up to carry and drain its buffer exactly once", async () => {
    const { client } = createDeterministicClient();
    const state = client as unknown as PrivateState;
    state.connectTimeoutMs = 2_000;

    class StubConfig {
      constructor(..._arguments: unknown[]) {}
      with_resume_state(_state: WaddleResumeStateSnapshot): void {}
    }
    const creationWaiters: Array<(client: StubClient) => void> = [];
    const waitForCreatedClient = () => new Promise<StubClient>((resolve) => {
      creationWaiters.push(resolve);
    });
    class StubClient {
      private sessionLifecycle: ((event: "fresh" | "resumed") => void) | null = null;
      private markConnectStarted!: () => void;
      readonly connectStarted = new Promise<void>((resolve) => {
        this.markConnectStarted = resolve;
      });

      constructor(..._arguments: unknown[]) {
        const methods = strictGeneratedMethodStubs();
        Reflect.deleteProperty(methods, "set_on_session_lifecycle");
        Object.assign(this, methods);
        const waiter = creationWaiters.shift();
        if (!waiter) throw new Error("unexpected XMPP client construction");
        waiter(this);
      }

      set_on_session_lifecycle(
        callback: (event: "fresh" | "resumed") => void,
      ): void {
        this.sessionLifecycle = callback;
      }

      get_resume_state(): null {
        return null;
      }

      async connect(): Promise<void> {
        this.markConnectStarted();
      }

      async disconnect(): Promise<void> {}

      dispose(): void {}

      startFreshSession(): void {
        if (!this.sessionLifecycle) {
          throw new Error("session lifecycle callback was not installed");
        }
        this.sessionLifecycle("fresh");
      }
    }
    const stubModule = {
      WaddleConfig: StubConfig,
      WaddleClient: StubClient,
    };
    state.loadModule = async () => stubModule;

    let releaseCatchupA!: () => void;
    let markCatchupAStarted!: () => void;
    const catchupA = new Promise<void>((resolve) => {
      releaseCatchupA = resolve;
    });
    const catchupAStarted = new Promise<void>((resolve) => {
      markCatchupAStarted = resolve;
    });
    let catchupGeneration = 0;
    state.catchup.onSessionStarted = () => {
      catchupGeneration += 1;
      return catchupGeneration === 1
        ? [{ kind: "dm", key: "bob@example.com", scope: "account" }]
        : [];
    };
    state.runReconnectCatchup = async () => {
      markCatchupAStarted();
      await catchupA;
    };
    const delivered: string[] = [];
    client.setDirectMessageHandler((message) => delivered.push(message.body));

    const createdA = waitForCreatedClient();
    const connectA = client.connect();
    void connectA.catch(() => undefined);
    const handleA = await createdA;
    await handleA.connectStarted;
    handleA.startFreshSession();
    await catchupAStarted;
    expect(state.resumeBarrier?.xmpp).toBe(handleA);

    state.handleMessage(bufferedDirectMessage("carried-a"));
    expect(state.pendingDuringResume).toHaveLength(1);

    const disconnectA = client.disconnect();
    await connectA.catch(() => undefined);
    const createdB = waitForCreatedClient();
    let handleBCreated = false;
    void createdB.then(() => {
      handleBCreated = true;
    });
    const connectB = client.connect();
    void connectB.catch(() => undefined);
    expect(handleBCreated).toBe(false);

    releaseCatchupA();
    await disconnectA;
    const handleB = await createdB;
    await handleB.connectStarted;
    expect(state.carriedPendingDuringResume).toHaveLength(1);

    handleB.startFreshSession();
    await connectB;
    await client.whenLifecycleQuiescent();
    expect(delivered).toEqual(["buffered once"]);
    expect(state.carriedPendingDuringResume).toHaveLength(0);

    handleB.startFreshSession();
    await client.whenLifecycleQuiescent();
    expect(delivered).toEqual(["buffered once"]);

    await client.disconnect();
  });

  test("a delayed authority-fence retirement cannot reschedule after a successor generation disconnects", async () => {
    const { client } = createDeterministicClient();
    const state = client as unknown as PrivateState;
    const scheduled: Array<{ attempt: number; delayMs: number }> = [];
    client.onReconnectScheduled((info) => scheduled.push(info));
    state.outboundQueue.reconcileNativeSnapshot = async () => ({
      terminalIds: [],
      missingIds: [],
    });

    let releaseRetirement!: () => void;
    const retirementGate = new Promise<void>((resolve) => {
      releaseRetirement = resolve;
    });
    const handleA = {
      disconnect: () => retirementGate,
      dispose: () => undefined,
    };
    const handleB = {
      get_resume_state: () => null,
      disconnect: async () => undefined,
      dispose: () => undefined,
    };
    state.xmpp = handleA;
    state.connected = true;

    state.handleOutboundAuthorityLost(
      new OutboundAuthorityChangedError("owner-a", "instance-a", 1, 1),
    );
    const generationB = state.connectEpoch;
    await bindCurrentGeneration(state);
    state.xmpp = handleB;
    state.connected = true;
    state.handleDisconnected(
      handleB,
      generationB,
      new Error("successor transport dropped"),
    );

    expect(state.connectEpoch).toBe(generationB + 1);
    expect(scheduled).toHaveLength(1);

    releaseRetirement();
    await state.whenLifecycleQuiescent();

    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });

  test("a stale connect timer cannot tear down a subsequent connect", async () => {
    const timeoutScheduler = new ControllableTimeoutScheduler();
    const client = createTestClient(timeoutScheduler);
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    const scheduled: Array<{ attempt: number; delayMs: number }> = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));
    client.onReconnectScheduled((info) => scheduled.push(info));

    let handleBDisconnects = 0;
    const handleA = {
      disconnect: async () => {},
      dispose: () => undefined,
    };
    const handleB = {
      disconnect: async () => { handleBDisconnects += 1; },
      dispose: () => undefined,
    };
    let attempt = 0;
    const firstAttemptStarted = deferred();
    const secondAttemptStarted = deferred();
    state.doConnect = async () => {
      attempt += 1;
      // Socket opens but the session never establishes (stalled).
      state.xmpp = attempt === 1 ? handleA : handleB;
      (attempt === 1 ? firstAttemptStarted : secondAttemptStarted).resolve(undefined);
    };

    // Connect A stalls inside a 40ms budget, then the user disconnects.
    state.connectTimeoutMs = 40;
    await bindCurrentGeneration(state);
    const first = client.connect();
    first.catch(() => undefined);
    await firstAttemptStarted.promise;
    await client.disconnect();
    // A's promise must settle at disconnect, not dangle until the
    // orphaned timer fires.
    let firstSettled = false;
    await first.catch(() => { firstSettled = true; });
    expect(firstSettled).toBe(true);

    // Connect B starts within A's original budget window.
    statuses.length = 0;
    state.connectTimeoutMs = 500;
    await bindCurrentGeneration(state);
    const second = client.connect();
    second.catch(() => undefined);
    await secondAttemptStarted.promise;

    // Advance past A's original 40ms budget: the stale timer must not
    // tear down B's half-open handle or emit spurious statuses.
    timeoutScheduler.advanceBy(60);
    expect(state.xmpp).toBe(handleB);
    expect(handleBDisconnects).toBe(0);
    expect(statuses.some((snapshot) => snapshot.state === "reconnecting")).toBe(false);
    expect(scheduled).toHaveLength(0);

    // Cleanup: cancel B's pending attempt too.
    await client.disconnect();
    state.reconnect.clearTimer();
  });
});

describe("stalled WASM load cannot double-connect (C3)", () => {
  test("a module load resolving after timeout + second connect yields exactly one live handle", async () => {
    const timeoutScheduler = new ControllableTimeoutScheduler();
    const client = createTestClient(timeoutScheduler);
    const state = client as unknown as PrivateState;
    client.setStatusHandler(() => {});

    class StubConfig {
      constructor(..._args: unknown[]) {}
      with_resume_state(_state: WaddleResumeStateSnapshot): void {}
    }
    const created: Array<{ connects: number; disconnects: number }> = [];
    const handleCreated = deferred();
    const handleConnectStarted = deferred();
    class StubClient {
      connects = 0;
      disconnects = 0;
      constructor(..._args: unknown[]) {
        Object.assign(this, strictGeneratedMethodStubs());
        created.push(this);
        handleCreated.resolve(undefined);
      }
      get_resume_state() { return null; }
      async connect() {
        this.connects += 1;
        handleConnectStarted.resolve(undefined);
      }
      async disconnect() { this.disconnects += 1; }
      dispose() {}
    }
    const stubModule = { WaddleConfig: StubConfig, WaddleClient: StubClient };
    const firstLoad = deferred<typeof stubModule>();
    const firstLoadStarted = deferred();
    const secondLoadStarted = deferred();
    let loads = 0;
    state.loadModule = () => {
      loads += 1;
      if (loads === 1) {
        firstLoadStarted.resolve(undefined);
        return firstLoad.promise as Promise<never>;
      }
      secondLoadStarted.resolve(undefined);
      return Promise.resolve(stubModule) as Promise<never>;
    };
    state.connectTimeoutMs = 30;

    // Connect #1: the module load stalls past the connect budget.
    const first = client.connect();
    first.catch(() => undefined);
    await firstLoadStarted.promise;
    timeoutScheduler.advanceBy(30);
    await first.catch(() => undefined);
    state.reconnect.clearTimer();

    // Connect #2 starts while #1's module load is still pending.
    const second = client.connect();
    second.catch(() => undefined);
    firstLoad.resolve(stubModule);
    await secondLoadStarted.promise;
    await handleCreated.promise;
    await handleConnectStarted.promise;

    // Exactly one handle was created and connected; the stale
    // continuation from connect #1 aborted without creating a zombie.
    expect(created).toHaveLength(1);
    expect(created[0]!.connects).toBe(1);
    expect(state.xmpp).toBe(created[0]);

    timeoutScheduler.advanceBy(30);
    await second.catch(() => undefined);
    state.reconnect.clearTimer();
  });
});

describe("terminal classification requires the structured stream-error (C4)", () => {
  test("a free-text disconnect reason containing 'conflict' stays recoverable", () => {
    const { state, stub, statuses, scheduled } = createHarness();

    state.handleDisconnected(
      stub,
      state.connectEpoch,
      new Error("write conflict on shard"),
    );

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });

  test("a typed driver error whose detail merely contains 'conflict' stays recoverable", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({
      kind: "driver-error",
      reason: "core-error",
    });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });

  test("an object payload whose detail merely mentions not-authorized stays recoverable", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({
      kind: "driver-error",
      reason: "core-error",
    });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });

  test("the WASM driver's typed SASL rejection classifies terminal via set_on_error", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({
      kind: "driver-error",
      reason: "authentication-rejected",
      authenticationCondition: "not-authorized",
    });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("error");
    expect(scheduled).toHaveLength(0);
    state.reconnect.clearTimer();
  });
});

describe("stale terminalDisconnectDetail must not survive disconnect()", () => {
  test("terminal stream error + disconnect() before the disconnect callback: the next session's transient drop stays recoverable", async () => {
    const { client, state, stub, statuses, scheduled, fireError, ready } = createHarness();

    // Terminal classification arrives, but the user disconnects before
    // the WASM disconnect callback consumes the armed detail (the
    // destroying path returns early; the late callback is dropped by
    // the handle guard).
    await ready;
    fireError({ kind: "stream-error", condition: "not-authorized" });
    await client.disconnect();

    // Next session on the same instance: a connect is in flight...
    statuses.length = 0;
    state.connectTimeoutMs = 200;
    const connectStarted = deferred();
    state.doConnect = async () => {
      state.xmpp = stub;
      state.connected = true;
      connectStarted.resolve(undefined);
    };
    await bindCurrentGeneration(state);
    const pending = client.connect();
    pending.catch(() => undefined);
    await connectStarted.promise;

    // ...and its FIRST transient disconnect must reconnect — not consume
    // the previous session's stale terminal detail into a false
    // "sign in again" error with zero retries.
    state.handleDisconnected(stub);
    await pending.catch(() => undefined);

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });

  test("connect-budget timeout after a terminal stream error does not poison the scheduler's next attempt", async () => {
    const timeoutScheduler = new ControllableTimeoutScheduler();
    const {
      client,
      state,
      statuses,
      scheduled,
      fireError,
      fireDisconnected,
      ready,
      installSuccessor,
    } = createHarness(timeoutScheduler);

    // A connect attempt is pending; a terminal stream error lands, then
    // the connect budget fires BEFORE the disconnect callback — nulling
    // `this.xmpp` and orphaning the armed detail.
    await ready;
    state.connected = false;
    state.connectTimeoutMs = 10;
    state.doConnect = async () => {};
    const pending = client.connect();
    pending.catch(() => undefined);
    fireError({ kind: "stream-error", condition: "not-authorized" });
    timeoutScheduler.advanceBy(10);
    await pending.catch(() => undefined);
    expect(scheduled).toHaveLength(1);

    // The scheduler's next attempt suffers a transient disconnect: it
    // must stay in the reconnecting loop, not flip terminal off the
    // orphaned detail.
    state.reconnect.clearTimer();
    await installSuccessor();
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(2);
    state.reconnect.clearTimer();
  });
});

describe("isExhausted must not fast-reject during the final in-flight attempt", () => {
  test("connect() during the last scheduled attempt joins it; after it truly exhausts, connect() fast-rejects", async () => {
    const timeoutScheduler = new ControllableTimeoutScheduler();
    const client = createTestClient(timeoutScheduler);
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));

    // The budget is fully spent and the scheduler's FINAL timer has just
    // fired: attempt == MAX, timer == null, the attempt is in flight.
    (state.reconnect as unknown as { attempt: number }).attempt = 10;
    state.connectTimeoutMs = 30;
    const finalAttemptStarted = deferred();
    state.doConnect = async () => {
      finalAttemptStarted.resolve(undefined);
    }; // socket never establishes a session
    await bindCurrentGeneration(state);
    const inflight = state.connectFromScheduler();
    inflight.catch(() => undefined);

    // A user action during the window must join the in-flight attempt
    // (which may still succeed), not fast-reject.
    let rejection: Error | null = null;
    let settled = false;
    const joined = client.connect().then(
      () => { settled = true; },
      (error: Error) => { settled = true; rejection = error; },
    );
    await finalAttemptStarted.promise;
    expect(settled).toBe(false);

    // The final attempt times out → NOW the loop is truly exhausted.
    timeoutScheduler.advanceBy(30);
    await joined;
    expect(rejection?.message).toBe("Reconnection timed out");
    expect(statuses.at(-1)?.state).toBe("error");

    await expect(client.connect()).rejects.toThrow("XMPP session is not ready");
    state.reconnect.clearTimer();
  });
});

describe("connect-timeout teardown (#1164 slice 3)", () => {
  test("a stalled connect tears down the handle, emits reconnecting, and reschedules", async () => {
    const timeoutScheduler = new ControllableTimeoutScheduler();
    const client = createTestClient(timeoutScheduler);
    const state = client as unknown as PrivateState & {
      connectTimeoutMs: number;
      doConnect: () => Promise<void>;
    };
    const statuses: XmppStatusSnapshot[] = [];
    const scheduled: Array<{ attempt: number; delayMs: number }> = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));
    client.onReconnectScheduled((info) => scheduled.push(info));

    let stalledHandleDisconnects = 0;
    state.connectTimeoutMs = 10;
    state.doConnect = async () => {
      // Socket opened but the session never establishes.
      state.xmpp = {
        get_resume_state: () => null,
        disconnect: async () => { stalledHandleDisconnects += 1; },
        dispose: () => undefined,
      };
    };

    await bindCurrentGeneration(state);
    const connect = client.connect();
    timeoutScheduler.advanceBy(10);
    await expect(connect).rejects.toThrow("Reconnection timed out");

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    expect(state.xmpp).toBeNull();
    expect(stalledHandleDisconnects).toBe(1);
    state.reconnect.clearTimer();
  });
});
