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
import { describe, expect, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { XmppStatusSnapshot } from "../src/lib/xmpp/types";

type StreamErrorPayload = { detail?: string | null; condition?: string | null };

type StubXmpp = {
  set_on_error: (cb: (payload: StreamErrorPayload) => void) => void;
  set_on_disconnected: (cb: () => void) => void;
  get_resume_state: () => null;
};

type PrivateState = {
  xmpp: unknown;
  connected: boolean;
  wireEvents: (xmpp: StubXmpp) => void;
  reconnect: { clearTimer: () => void };
  handleDisconnected: (xmpp: unknown, error?: Error) => void;
  handleMessage: (message: unknown) => void;
  connectTimeoutMs: number;
  doConnect: () => Promise<void>;
  loadModule: () => Promise<unknown>;
  runSessionReady: (xmpp: unknown, lifecycle: { type: "fresh" | "resumed" }) => Promise<void>;
  runReconnectCatchup: (...args: unknown[]) => Promise<void>;
  catchup: { onSessionStarted: () => Array<{ kind: "dm" | "room"; key: string }> };
};

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

function createHarness() {
  const client = new BrowserXmppClient(session());
  const state = client as unknown as PrivateState;
  const statuses: XmppStatusSnapshot[] = [];
  const scheduled: Array<{ attempt: number; delayMs: number }> = [];
  client.setStatusHandler((snapshot) => statuses.push(snapshot));
  client.onReconnectScheduled((info) => scheduled.push(info));

  let fireError: (payload: StreamErrorPayload) => void = () => {};
  let fireDisconnected: () => void = () => {};
  const stub: StubXmpp = {
    set_on_error(cb) { fireError = cb; },
    set_on_disconnected(cb) { fireDisconnected = cb; },
    get_resume_state: () => null,
  };
  state.xmpp = stub;
  state.connected = true;
  state.wireEvents(stub);

  return {
    client,
    state,
    stub,
    statuses,
    scheduled,
    fireError: (payload: StreamErrorPayload) => fireError(payload),
    fireDisconnected: () => fireDisconnected(),
  };
}

describe("terminal error state (#1164 slice 1)", () => {
  test("a not-authorized stream error reaches state \"error\" and never schedules a retry", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ condition: "not-authorized", detail: "SASL authentication failed" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("error");
    expect(scheduled).toHaveLength(0);
    state.reconnect.clearTimer();
  });

  test("a resource conflict is terminal too", () => {
    const { statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ condition: "conflict", detail: "replaced by new connection" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("error");
    expect(scheduled).toHaveLength(0);
  });

  test("a recoverable stream error keeps the reconnecting loop", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ condition: "system-shutdown", detail: "going down" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });
});

describe("retry-loop cap (#1164 slice 2)", () => {
  test("exhausting the reconnect attempts flips the client to terminal error", () => {
    const { state, stub, statuses, scheduled, fireDisconnected } = createHarness();

    for (let i = 0; i < 11; i += 1) {
      // Re-arm the handle + drop the pending timer so each disconnect
      // is accepted and each schedule() call actually runs.
      state.reconnect.clearTimer();
      state.xmpp = stub;
      state.connected = true;
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
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));
    const bodies: string[] = [];
    client.setDirectMessageHandler((message) => bodies.push(message.body));

    state.connectTimeoutMs = 20;
    // Catch-up (behind the resume barrier) outlives the connect budget.
    state.runReconnectCatchup = () => sleep(60);
    state.catchup.onSessionStarted = () => [{ kind: "dm", key: "bob@example.com" }];
    let stalledHandleDisconnects = 0;
    const handle = { disconnect: async () => { stalledHandleDisconnects += 1; } };
    state.doConnect = async () => {
      state.xmpp = handle;
      void state.runSessionReady(handle, { type: "fresh" });
    };

    let settled: "resolved" | "rejected" | null = null;
    const connected = client.connect().then(
      () => { settled = "resolved"; },
      () => { settled = "rejected"; },
    );

    // A live DM arriving mid-barrier is buffered for the drain.
    await sleep(5);
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

    // Wait past the connect budget while the barrier is still open.
    await sleep(40);
    expect(state.xmpp).toBe(handle);
    expect(stalledHandleDisconnects).toBe(0);
    expect(statuses.some((snapshot) => snapshot.state === "reconnecting")).toBe(false);

    await connected;
    expect(settled).toBe("resolved");
    expect(state.connected).toBe(true);
    expect(bodies).toEqual(["hi"]);
    state.reconnect.clearTimer();
  });
});

describe("exhaustion recovery (C2)", () => {
  test("an explicit connect() after exhaustion restores a fresh retry budget", async () => {
    const { client, state, stub, scheduled, fireDisconnected } = createHarness();
    for (let i = 0; i < 11; i += 1) {
      state.reconnect.clearTimer();
      state.xmpp = stub;
      state.connected = true;
      fireDisconnected();
    }
    expect(scheduled).toHaveLength(10);

    // User-triggered connect: the attempt itself fails, but the budget
    // is fresh — the next disconnect schedules attempt 1 again instead
    // of re-exhausting instantly.
    state.doConnect = async () => { throw new Error("still down"); };
    await client.connect().catch(() => undefined);
    state.reconnect.clearTimer();
    state.xmpp = stub;
    state.connected = true;
    fireDisconnected();

    expect(scheduled).toHaveLength(11);
    expect(scheduled.at(-1)?.attempt).toBe(1);
    state.reconnect.clearTimer();
  });

  test("exhaustion while the browser is offline reports offline, not terminal error", () => {
    const originalNavigator = globalThis.navigator;
    Object.defineProperty(globalThis, "navigator", {
      value: { onLine: false },
      configurable: true,
    });
    try {
      const { state, stub, statuses, fireDisconnected } = createHarness();
      for (let i = 0; i < 11; i += 1) {
        state.reconnect.clearTimer();
        state.xmpp = stub;
        state.connected = true;
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

  test("a window online event after exhaustion triggers a fresh connect", () => {
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
      const { client, state, stub, fireDisconnected } = createHarness();
      for (let i = 0; i < 11; i += 1) {
        state.reconnect.clearTimer();
        state.xmpp = stub;
        state.connected = true;
        fireDisconnected();
      }
      let connects = 0;
      (client as unknown as { connect: () => Promise<void> }).connect = async () => { connects += 1; };
      for (const cb of listeners.online ?? []) cb();
      expect(connects).toBe(1);
      state.reconnect.clearTimer();
    } finally {
      if (originalWindow === undefined) delete globals.window;
      else globals.window = originalWindow;
    }
  });
});

describe("stalled WASM load cannot double-connect (C3)", () => {
  test("a module load resolving after timeout + second connect yields exactly one live handle", async () => {
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState;
    client.setStatusHandler(() => {});

    class StubConfig {
      constructor(..._args: unknown[]) {}
    }
    const created: Array<{ connects: number; disconnects: number }> = [];
    class StubClient {
      connects = 0;
      disconnects = 0;
      constructor(..._args: unknown[]) { created.push(this); }
      async connect() { this.connects += 1; }
      async disconnect() { this.disconnects += 1; }
    }
    const stubModule = { WaddleConfig: StubConfig, WaddleClient: StubClient };
    let releaseFirstLoad!: () => void;
    const firstLoad = new Promise((resolve) => { releaseFirstLoad = () => resolve(stubModule); });
    let loads = 0;
    state.loadModule = () => {
      loads += 1;
      return (loads === 1 ? firstLoad : Promise.resolve(stubModule)) as Promise<never>;
    };
    state.connectTimeoutMs = 30;

    // Connect #1: the module load stalls past the connect budget.
    const first = client.connect();
    first.catch(() => undefined);
    await sleep(45);
    state.reconnect.clearTimer();

    // Connect #2 starts while #1's module load is still pending.
    const second = client.connect();
    second.catch(() => undefined);
    await sleep(5);
    releaseFirstLoad();
    await sleep(5);

    // Exactly one handle was created and connected; the stale
    // continuation from connect #1 aborted without creating a zombie.
    expect(created).toHaveLength(1);
    expect(created[0]!.connects).toBe(1);
    expect(state.xmpp).toBe(created[0]);

    await second.catch(() => undefined);
    state.reconnect.clearTimer();
  });
});

describe("terminal classification requires the structured stream-error (C4)", () => {
  test("a free-text disconnect reason containing 'conflict' stays recoverable", () => {
    const { state, stub, statuses, scheduled } = createHarness();

    state.handleDisconnected(stub, new Error("write conflict on shard"));

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });

  test("the WASM driver's SASL-rejection string still classifies terminal via set_on_error", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    // The WASM core reports connect-time SASL failure through
    // `set_on_error` with the ClientError display string — not through
    // the disconnect callback (which carries no error at all).
    (fireError as unknown as (payload: unknown) => void)(
      "server rejected SASL authentication with condition `not-authorized`",
    );
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("error");
    expect(scheduled).toHaveLength(0);
    state.reconnect.clearTimer();
  });
});

describe("connect-timeout teardown (#1164 slice 3)", () => {
  test("a stalled connect tears down the handle, emits reconnecting, and reschedules", async () => {
    const client = new BrowserXmppClient(session());
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
      state.xmpp = { disconnect: async () => { stalledHandleDisconnects += 1; } };
    };

    await expect(client.connect()).rejects.toThrow("Reconnection timed out");

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    expect(state.xmpp).toBeNull();
    expect(stalledHandleDisconnects).toBe(1);
    state.reconnect.clearTimer();
  });
});
