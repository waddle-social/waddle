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
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { countQueuedMessages } from "../src/lib/outbound-queue-store";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import { nullResumePersistence } from "../src/lib/xmpp/resume-persistence";
import type { ReconnectCatchupEntry } from "../src/lib/xmpp/reconnect-catchup";
import type { XmppStatusSnapshot } from "../src/lib/xmpp/types";

type StreamErrorPayload = { detail?: string | null; condition?: string | null };

type StubXmpp = {
  set_on_error: (cb: (payload: StreamErrorPayload) => void) => void;
  set_on_disconnected: (cb: () => void) => void;
  get_resume_state: () => { previd: string; inboundH: number; outboundH: number } | null;
  get_resume_state_handle?: () => unknown;
};

type PrivateState = {
  xmpp: unknown;
  connected: boolean;
  wireEvents: (xmpp: StubXmpp) => void;
  reconnect: { clearTimer: () => void; hasPendingRetry: () => boolean };
  resumeAfterPageShow: () => void;
  connectWithFreshBudget: () => Promise<void>;
  connectFromScheduler: () => Promise<void>;
  handleDisconnected: (xmpp: unknown, error?: Error) => void;
  handleMessage: (message: unknown) => void;
  connectTimeoutMs: number;
  doConnect: () => Promise<void>;
  loadModule: () => Promise<unknown>;
  runSessionReady: (xmpp: unknown, lifecycle: { type: "fresh" | "resumed" }) => Promise<void>;
  runReconnectCatchup: (...args: unknown[]) => Promise<void>;
  catchup: { onSessionStarted: () => ReconnectCatchupEntry[] };
  resume: {
    state: unknown;
    handle: unknown;
    beginOwnResume: (previd: string, clientId: string) => void;
  };
  ownResumePrevidByXmpp: Map<unknown, string>;
};

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

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
    addEventListener: () => undefined,
    localStorage: storage,
    removeEventListener: () => undefined,
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

function session(jid = "alice@example.com/desktop"): WaddleSession {
  return {
    username: "alice",
    jid,
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

describe("own-resume conflict collateral (#1389)", () => {
  test("a foreign exact-previd resume supersedes the former stream without retaining or re-resuming it", () => {
    // This is deliberately two BrowserXmppClient instances, not two connects
    // in one instance: connectEpoch prevents the latter from retaining two
    // live handles. A tab replacement can instead resume the old stream while
    // the old document is still receiving its force-detach close.
    const resumingPrevid = new Map<string, string>();
    let retainedSm = true;
    const persistence = {
      ...nullResumePersistence,
      clearSm() { retainedSm = false; },
      beginOwnResume(previd: string, clientId: string) { resumingPrevid.set(previd, clientId); },
      finishOwnResume(previd: string, clientId: string) {
        if (resumingPrevid.get(previd) === clientId) resumingPrevid.delete(previd);
      },
      consumeOwnResume(previd: string, clientId: string) {
        const owner = resumingPrevid.get(previd);
        if (!owner) return "absent";
        if (owner === clientId) return "self";
        return "foreign";
      },
    };
    const client = new BrowserXmppClient(session(), persistence);
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    const scheduled: Array<{ attempt: number; delayMs: number }> = [];
    const errors: Array<{ recoverable: boolean }> = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));
    client.onReconnectScheduled((info) => scheduled.push(info));
    client.onError((event) => errors.push(event));

    let fireError: (payload: StreamErrorPayload) => void = () => {};
    let fireDisconnected: () => void = () => {};
    let resumeStateReads = 0;
    let resumeHandleReads = 0;
    const formerStream: StubXmpp = {
      set_on_error(cb) { fireError = cb; },
      set_on_disconnected(cb) { fireDisconnected = cb; },
      get_resume_state: () => {
        resumeStateReads += 1;
        return { previd: "former-stream", inboundH: 3, outboundH: 5 };
      },
      get_resume_state_handle: () => {
        resumeHandleReads += 1;
        return undefined;
      },
    };
    state.xmpp = formerStream;
    state.connected = true;
    state.wireEvents(formerStream);

    // The new page has just started the resume for the very stream receiving
    // this close. The former document must yield rather than resume it again.
    persistence.beginOwnResume("former-stream", "new-client");
    fireError({ condition: "conflict", detail: "replaced by resume" });
    fireDisconnected();

    expect(errors.at(-1)?.recoverable).toBe(true);
    expect(statuses.at(-1)).toEqual({
      state: "offline",
      detail: "This session was resumed in another tab.",
      kind: "superseded",
    });
    expect(retainedSm).toBe(false);
    expect(state.resume.state).toBeNull();
    expect(state.resume.handle).toBeNull();
    // One read classifies the foreign witness; no second read/handle capture
    // occurs from `captureFromDisconnect`.
    expect(resumeStateReads).toBe(1);
    expect(resumeHandleReads).toBe(0);
    expect(scheduled).toHaveLength(0);
    // The former tab did not clear the successor's witness, so there is no
    // second local resume that could force-detach the replacement stream.
    expect(resumingPrevid.get("former-stream")).toBe("new-client");
  });

  test("late conflicts from two former local handoffs both stay recoverable", () => {
    const resumingPrevid = new Map<string, string>();
    const persistence = {
      ...nullResumePersistence,
      beginOwnResume(previd: string, clientId: string) { resumingPrevid.set(previd, clientId); },
      finishOwnResume(previd: string, clientId: string) {
        if (resumingPrevid.get(previd) === clientId) resumingPrevid.delete(previd);
      },
      consumeOwnResume(previd: string, clientId: string) {
        const owner = resumingPrevid.get(previd);
        if (!owner) return "absent";
        return owner === clientId ? "self" : "foreign";
      },
    };
    const former = () => {
      const client = new BrowserXmppClient(session(), persistence);
      const state = client as unknown as PrivateState;
      const statuses: XmppStatusSnapshot[] = [];
      const scheduled: Array<{ attempt: number; delayMs: number }> = [];
      client.setStatusHandler((snapshot) => statuses.push(snapshot));
      client.onReconnectScheduled((info) => scheduled.push(info));
      let fireError: (payload: StreamErrorPayload) => void = () => {};
      let fireDisconnected: () => void = () => {};
      const stream: StubXmpp = {
        set_on_error(cb) { fireError = cb; },
        set_on_disconnected(cb) { fireDisconnected = cb; },
        get_resume_state: () => ({ previd: "same-former-stream", inboundH: 3, outboundH: 5 }),
      };
      state.xmpp = stream;
      state.connected = true;
      state.wireEvents(stream);
      return { state, statuses, scheduled, fireError, fireDisconnected };
    };
    const a = former();
    const b = former();

    // C has completed the latest handoff for the same XEP-0198 previd.
    resumingPrevid.set("same-former-stream", "c-client");
    for (const formerClient of [a, b]) {
      formerClient.fireError({ condition: "conflict", detail: "replaced by resume" });
      formerClient.fireDisconnected();
      expect(formerClient.statuses.at(-1)?.state).toBe("offline");
      expect(formerClient.scheduled).toHaveLength(0);
    }
  });

  test("the resuming handle cannot consume its own exact-previd witness", () => {
    const resumingPrevid = new Set<string>(["former-stream"]);
    const persistence = {
      ...nullResumePersistence,
      beginOwnResume(previd: string) { resumingPrevid.add(previd); },
      finishOwnResume(previd: string) { resumingPrevid.delete(previd); },
      consumeOwnResume() {
        return "self";
      },
    };
    const client = new BrowserXmppClient(session(), persistence);
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));
    let fireError: (payload: StreamErrorPayload) => void = () => {};
    let fireDisconnected: () => void = () => {};
    const resumingHandle: StubXmpp = {
      set_on_error(cb) { fireError = cb; },
      set_on_disconnected(cb) { fireDisconnected = cb; },
      get_resume_state: () => ({ previd: "former-stream", inboundH: 3, outboundH: 5 }),
    };
    state.xmpp = resumingHandle;
    state.connected = true;
    state.resume.beginOwnResume("former-stream", "former-client");
    state.ownResumePrevidByXmpp.set(resumingHandle, "former-stream");
    state.wireEvents(resumingHandle);

    fireError({ condition: "conflict", detail: "genuine later replacement" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("error");
    // It was not consumed as collateral; the terminal disconnect then clears
    // this attempt's marker through its normal failure lifecycle.
    expect(resumingPrevid.has("former-stream")).toBe(false);
  });

  test("a superseded former tab blocks background reconnects until explicit fresh recovery", async () => {
    const resumingPrevid = new Map<string, string>();
    const accountBareJid = "recovery-alice@example.com";
    let restoredSm: {
      previd: string;
      inboundH: number;
      outboundH: number;
      resource: string;
    } | null = {
      previd: "former-stream",
      inboundH: 3,
      outboundH: 5,
      resource: "web-former-stream",
    };
    const persistence = {
      ...nullResumePersistence,
      consumeSm() {
        const snapshot = restoredSm;
        restoredSm = null;
        return snapshot;
      },
      beginOwnResume(previd: string, clientId: string) { resumingPrevid.set(previd, clientId); },
      finishOwnResume(previd: string, clientId: string) {
        if (resumingPrevid.get(previd) === clientId) resumingPrevid.delete(previd);
      },
      consumeOwnResume(previd: string, clientId: string) {
        const owner = resumingPrevid.get(previd);
        if (!owner) return "absent";
        return owner === clientId ? "self" : "foreign";
      },
    };
    const client = new BrowserXmppClient(session(`${accountBareJid}/desktop`), persistence);
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));
    state.connectTimeoutMs = 5;
    expect(client.xmppResource).toBe("web-former-stream");

    let fireError: (payload: StreamErrorPayload) => void = () => {};
    let fireDisconnected: () => void = () => {};
    const formerStream: StubXmpp = {
      set_on_error(cb) { fireError = cb; },
      set_on_disconnected(cb) { fireDisconnected = cb; },
      get_resume_state: () => ({ previd: "former-stream", inboundH: 3, outboundH: 5 }),
    };
    state.xmpp = formerStream;
    state.connected = true;
    state.wireEvents(formerStream);

    resumingPrevid.set("former-stream", "new-client");
    fireError({ condition: "conflict", detail: "replaced by resume" });
    fireDisconnected();

    expect(statuses.at(-1)).toEqual({
      state: "offline",
      detail: "This session was resumed in another tab.",
      kind: "superseded",
    });

    let backgroundAttempts = 0;
    const originalDoConnect = state.doConnect.bind(state);
    state.doConnect = async () => {
      backgroundAttempts += 1;
    };

    await expect(client.setPresence("available")).rejects.toThrow("XMPP session is not ready");
    await expect(client.connect()).rejects.toThrow("XMPP session is not ready");
    state.resumeAfterPageShow();
    await sleep(0);

    expect(backgroundAttempts).toBe(0);

    const queued = await client.sendDirectMessage("bob@example.com/phone", "queued while superseded");
    expect(queued).toMatchObject({ state: "queued" });
    expect(statuses.at(-1)).toEqual({
      state: "offline",
      detail: "This session was resumed in another tab.",
      kind: "superseded",
    });

    const resourceBindings = new Map<string, "successor" | "recovered">([
      [client.xmppResource, "successor"],
    ]);
    const freshConfigs: string[] = [];
    const sent: Array<{ resource: string; peerJid: string; body: string; id: string }> = [];

    class StubConfig {
      readonly resource: string;

      constructor(
        _websocketUrl: string,
        _jid: string,
        _sessionId: string,
        resource: string,
      ) {
        this.resource = resource;
        freshConfigs.push(resource);
      }
    }

    class StubClient {
      readonly resource: string;
      private onSessionLifecycle?: (event: string) => void;
      private onAck?: (id: string) => void;

      constructor(config: StubConfig) {
        this.resource = config.resource;
      }

      set_on_session_lifecycle(cb: (event: string) => void) { this.onSessionLifecycle = cb; }
      set_on_disconnected(_cb: () => void) {}
      set_on_error(_cb: (payload: StreamErrorPayload) => void) {}
      set_on_message_delivery_acked(cb: (id: string) => void) { this.onAck = cb; }

      async connect() {
        backgroundAttempts += 1;
        if (resourceBindings.has(this.resource)) {
          throw new Error(`resource ${this.resource} displaced the successor`);
        }
        resourceBindings.set(this.resource, "recovered");
        this.onSessionLifecycle?.("fresh");
      }

      async disconnect() {
        resourceBindings.delete(this.resource);
      }

      async send_chat_message(
        peerJid: string,
        body: string,
        opts: { stanza_id?: string },
      ) {
        sent.push({ resource: this.resource, peerJid, body, id: opts.stanza_id ?? "" });
        if (opts.stanza_id) this.onAck?.(opts.stanza_id);
        return opts.stanza_id ?? null;
      }
    }

    state.loadModule = async () => ({ WaddleConfig: StubConfig, WaddleClient: StubClient });
    state.doConnect = originalDoConnect;
    state.connectTimeoutMs = 50;

    await expect(client.recoverSupersededSession()).resolves.toBeUndefined();
    await sleep(0);

    expect(backgroundAttempts).toBe(1);
    expect(state.connected).toBe(true);
    expect(statuses.at(-1)?.state).toBe("online");
    expect(client.xmppResource).not.toBe("web-former-stream");
    expect(freshConfigs).toEqual([client.xmppResource]);
    expect(resourceBindings.get("web-former-stream")).toBe("successor");
    expect(resourceBindings.get(client.xmppResource)).toBe("recovered");
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({
      resource: client.xmppResource,
      peerJid: "bob@example.com",
      body: "queued while superseded",
    });
    expect(countQueuedMessages(accountBareJid)).toBe(0);
  });

  test("a rejected explicit superseded recovery re-arms the reconnect latch", async () => {
    const resumingPrevid = new Map<string, string>();
    const persistence = {
      ...nullResumePersistence,
      beginOwnResume(previd: string, clientId: string) { resumingPrevid.set(previd, clientId); },
      finishOwnResume(previd: string, clientId: string) {
        if (resumingPrevid.get(previd) === clientId) resumingPrevid.delete(previd);
      },
      consumeOwnResume(previd: string, clientId: string) {
        const owner = resumingPrevid.get(previd);
        if (!owner) return "absent";
        return owner === clientId ? "self" : "foreign";
      },
    };
    const client = new BrowserXmppClient(session(), persistence);
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));

    let fireError: (payload: StreamErrorPayload) => void = () => {};
    let fireDisconnected: () => void = () => {};
    const formerStream: StubXmpp = {
      set_on_error(cb) { fireError = cb; },
      set_on_disconnected(cb) { fireDisconnected = cb; },
      get_resume_state: () => ({ previd: "former-stream", inboundH: 3, outboundH: 5 }),
    };
    state.xmpp = formerStream;
    state.connected = true;
    state.wireEvents(formerStream);

    resumingPrevid.set("former-stream", "new-client");
    fireError({ condition: "conflict", detail: "replaced by resume" });
    fireDisconnected();

    let attempts = 0;
    state.doConnect = async () => {
      attempts += 1;
      throw new Error("module load failed");
    };

    await expect(client.recoverSupersededSession()).rejects.toThrow("module load failed");
    expect(attempts).toBe(1);
    expect(statuses.at(-1)).toEqual({
      state: "offline",
      detail: "This session was resumed in another tab.",
      kind: "superseded",
    });

    await expect(client.recoverSupersededSession()).rejects.toThrow("module load failed");
    expect(attempts).toBe(2);
  });

  test("a disconnected explicit recovery cancels its background retry before re-arming superseded", async () => {
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState & {
      reconnectBlock: { kind: "superseded"; detail: string } | null;
    };
    state.reconnectBlock = {
      kind: "superseded",
      detail: "This session was resumed in another tab.",
    };

    const recoveryHandle: StubXmpp = {
      set_on_error: () => undefined,
      set_on_disconnected: () => undefined,
      get_resume_state: () => null,
    };
    state.doConnect = async () => {
      state.xmpp = recoveryHandle;
      state.connected = true;
      state.handleDisconnected(recoveryHandle, new Error("fresh recovery dropped"));
    };

    await expect(client.recoverSupersededSession()).rejects.toThrow("fresh recovery dropped");
    expect(state.reconnectBlock).toEqual({
      kind: "superseded",
      detail: "This session was resumed in another tab.",
    });
    expect(state.reconnect.hasPendingRetry()).toBeFalse();
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
    state.catchup.onSessionStarted = () => [{ kind: "dm", key: "bob@example.com", scope: "account" }];
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
  test("a fresh-budget connect after exhaustion restores the retry budget", async () => {
    const { state, stub, scheduled, fireDisconnected } = createHarness();
    for (let i = 0; i < 11; i += 1) {
      state.reconnect.clearTimer();
      state.xmpp = stub;
      state.connected = true;
      fireDisconnected();
    }
    expect(scheduled).toHaveLength(10);

    // User-explicit recovery (the online listener's path): the attempt
    // itself fails, but the budget is fresh — the next disconnect
    // schedules attempt 1 again instead of re-exhausting instantly.
    state.doConnect = async () => { throw new Error("still down"); };
    await state.connectWithFreshBudget().catch(() => undefined);
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
      const { state, stub, scheduled, fireDisconnected } = createHarness();
      state.doConnect = async () => { throw new Error("still down"); };
      for (let i = 0; i < 11; i += 1) {
        state.reconnect.clearTimer();
        state.xmpp = stub;
        state.connected = true;
        fireDisconnected();
      }
      expect(scheduled).toHaveLength(10);

      // Network returns: the listener fires the fresh-budget path.
      for (const cb of listeners.online ?? []) cb();
      await sleep(1);

      // Fresh budget: the next disconnect schedules attempt 1 again —
      // the exhausted/terminal gate no longer blocks the retry loop.
      state.reconnect.clearTimer();
      state.xmpp = stub;
      state.connected = true;
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
    const { client, state, stub, scheduled, fireDisconnected } = createHarness();
    state.doConnect = async () => { throw new Error("still down"); };
    for (let i = 0; i < 3; i += 1) {
      state.reconnect.clearTimer();
      state.xmpp = stub;
      state.connected = true;
      fireDisconnected();
    }
    expect(scheduled.at(-1)?.attempt).toBe(3);

    // Background triggers (idle tracker → setPresence, MAM pagers,
    // sendGroupMessage fallback) all route through connect() during
    // the outage — the budget must keep counting up, not restart.
    await client.connect().catch(() => undefined);
    await client.connect().catch(() => undefined);
    state.reconnect.clearTimer();
    state.xmpp = stub;
    state.connected = true;
    fireDisconnected();

    expect(scheduled.at(-1)?.attempt).toBe(4);
    state.reconnect.clearTimer();
  });

  test("internal connect() while terminal-error rejects without clearing the error state or scheduling", async () => {
    const { client, state, statuses, scheduled, fireError, fireDisconnected } = createHarness();
    let attempts = 0;
    state.connectTimeoutMs = 10;
    state.doConnect = async () => { attempts += 1; };

    fireError({ condition: "not-authorized", detail: "SASL authentication failed" });
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
    const { client, state, stub, statuses, scheduled, fireDisconnected } = createHarness();
    for (let i = 0; i < 11; i += 1) {
      state.reconnect.clearTimer();
      state.xmpp = stub;
      state.connected = true;
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
      state.handleDisconnected(stub, new Error("still down"));
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
  test("a stale connect timer cannot tear down a subsequent connect", async () => {
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    const scheduled: Array<{ attempt: number; delayMs: number }> = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));
    client.onReconnectScheduled((info) => scheduled.push(info));

    let handleBDisconnects = 0;
    const handleA = { disconnect: async () => {} };
    const handleB = { disconnect: async () => { handleBDisconnects += 1; } };
    let attempt = 0;
    state.doConnect = async () => {
      attempt += 1;
      // Socket opens but the session never establishes (stalled).
      state.xmpp = attempt === 1 ? handleA : handleB;
    };

    // Connect A stalls inside a 40ms budget, then the user disconnects.
    state.connectTimeoutMs = 40;
    const first = client.connect();
    first.catch(() => undefined);
    await sleep(5);
    await client.disconnect();
    // A's promise must settle at disconnect, not dangle until the
    // orphaned timer fires.
    let firstSettled = false;
    await Promise.race([first.catch(() => { firstSettled = true; }), sleep(5)]);
    expect(firstSettled).toBe(true);

    // Connect B starts within A's original budget window.
    statuses.length = 0;
    state.connectTimeoutMs = 500;
    const second = client.connect();
    second.catch(() => undefined);

    // Advance past A's original 40ms budget: the stale timer must not
    // tear down B's half-open handle or emit spurious statuses.
    await sleep(60);
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

  test("a free-text stream error merely containing 'conflict' stays recoverable", () => {
    // PR-review finding: bare word-matching in free text latched
    // terminal state on benign errors (e.g. a proxied transport
    // message). Only a structured condition or the WASM ClientError's
    // backtick-quoted `condition` may classify terminal.
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    (fireError as unknown as (payload: unknown) => void)(
      "write conflict on shard while syncing presence",
    );
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });

  test("an object payload whose detail merely mentions not-authorized stays recoverable", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ detail: "proxy said: upstream not-authorized for CONNECT" });
    fireDisconnected();

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

describe("stale terminalDisconnectDetail must not survive disconnect()", () => {
  test("terminal stream error + disconnect() before the disconnect callback: the next session's transient drop stays recoverable", async () => {
    const { client, state, stub, statuses, scheduled, fireError } = createHarness();

    // Terminal classification arrives, but the user disconnects before
    // the WASM disconnect callback consumes the armed detail (the
    // destroying path returns early; the late callback is dropped by
    // the handle guard).
    fireError({ condition: "not-authorized", detail: "SASL authentication failed" });
    await client.disconnect();

    // Next session on the same instance: a connect is in flight...
    statuses.length = 0;
    state.connectTimeoutMs = 200;
    state.doConnect = async () => {
      state.xmpp = stub;
      state.connected = true;
    };
    const pending = client.connect();
    pending.catch(() => undefined);
    await sleep(5);

    // ...and its FIRST transient disconnect must reconnect — not consume
    // the previous session's stale terminal detail into a false
    // "sign in again" error with zero retries.
    state.handleDisconnected(stub);

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });

  test("connect-budget timeout after a terminal stream error does not poison the scheduler's next attempt", async () => {
    const { client, state, stub, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    // A connect attempt is pending; a terminal stream error lands, then
    // the connect budget fires BEFORE the disconnect callback — nulling
    // `this.xmpp` and orphaning the armed detail.
    state.connected = false;
    state.connectTimeoutMs = 10;
    state.doConnect = async () => {};
    const pending = client.connect();
    pending.catch(() => undefined);
    fireError({ condition: "not-authorized", detail: "SASL authentication failed" });
    await sleep(25);
    expect(scheduled).toHaveLength(1);

    // The scheduler's next attempt suffers a transient disconnect: it
    // must stay in the reconnecting loop, not flip terminal off the
    // orphaned detail.
    state.reconnect.clearTimer();
    state.xmpp = stub;
    state.connected = true;
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(2);
    state.reconnect.clearTimer();
  });
});

describe("isExhausted must not fast-reject during the final in-flight attempt", () => {
  test("connect() during the last scheduled attempt joins it; after it truly exhausts, connect() fast-rejects", async () => {
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState;
    const statuses: XmppStatusSnapshot[] = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));

    // The budget is fully spent and the scheduler's FINAL timer has just
    // fired: attempt == MAX, timer == null, the attempt is in flight.
    (state.reconnect as unknown as { attempt: number }).attempt = 10;
    state.connectTimeoutMs = 30;
    state.doConnect = async () => {}; // socket never establishes a session
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
    await sleep(5);
    expect(settled).toBe(false);

    // The final attempt times out → NOW the loop is truly exhausted.
    await sleep(45);
    await joined;
    expect(rejection?.message).toBe("Reconnection timed out");
    expect(statuses.at(-1)?.state).toBe("error");

    await expect(client.connect()).rejects.toThrow("XMPP session is not ready");
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
