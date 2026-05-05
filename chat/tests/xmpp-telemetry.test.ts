import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import {
  __setFaroForTesting,
  initTelemetry,
  reportMessageAcked,
  reportMessageFailed,
  reportQueueDepthChange,
  reportSendEnqueued,
  reportSessionLifecycle,
  reportStatusChange,
} from "../src/lib/telemetry";
import { installInstrumentation } from "../src/lib/xmpp/xmpp-instrumentation";

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

/**
 * Minimal Faro API stub that records every pushEvent / pushMeasurement
 * call. The telemetry module never reaches into the full Faro instance
 * beyond `faro.api.*`, so mirroring the `api` surface is enough.
 */
function createFaroStub() {
  const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
  const measurements: Array<{
    type: string;
    values: Record<string, number>;
    context?: Record<string, string>;
  }> = [];
  const errors: Array<{
    error: Error;
    options?: { type?: string; context?: Record<string, string> };
  }> = [];
  return {
    events,
    measurements,
    errors,
    api: {
      pushEvent: (name: string, attributes?: Record<string, string>) => {
        events.push({ name, attributes });
      },
      pushMeasurement: (payload: {
        type: string;
        values: Record<string, number>;
        context?: Record<string, string>;
      }) => {
        measurements.push(payload);
      },
      pushError: (
        error: Error,
        options?: { type?: string; context?: Record<string, string> },
      ) => {
        errors.push({ error, options });
      },
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
  __setFaroForTesting(null);
});

afterEach(() => {
  localStorage.clear();
  __setFaroForTesting(null);
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

describe("telemetry module no-op behaviour", () => {
  test("initTelemetry without a URL is a no-op and never throws", () => {
    expect(() =>
      initTelemetry({ url: "", appName: "test" }),
    ).not.toThrow();
  });

  test("report functions are no-ops when Faro has not been initialized", () => {
    // Nothing to assert beyond 'must not throw' — if these hit undefined
    // faro refs the chat would blow up on every send in local dev.
    expect(() => {
      reportMessageAcked({ id: "x", kind: "room", latencyMs: 5 });
      reportMessageFailed({ id: "x", kind: "dm" });
      reportSendEnqueued({ kind: "room", reason: "offline" });
      reportQueueDepthChange({ persisted: 0, inflight: 0 });
      reportSessionLifecycle({ type: "fresh" });
      reportStatusChange({ state: "online" });
    }).not.toThrow();
  });

  test("report functions forward to Faro api when initialized", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    reportMessageAcked({ id: "m-1", kind: "room", latencyMs: 123.4 });
    reportMessageFailed({ id: "m-2", kind: "dm" });
    reportSendEnqueued({ kind: "dm", reason: "offline" });
    reportQueueDepthChange({ persisted: 2, inflight: 1 });
    reportSessionLifecycle({ type: "resumed" });
    reportStatusChange({ state: "reconnecting", detail: "lost" });
    reportStatusChange({ state: "online", reconnectDurationMs: 4_321 });

    const eventNames = stub.events.map((e) => e.name);
    expect(eventNames).toEqual([
      "chat.xmpp.message.acked",
      "chat.xmpp.message.failed",
      "chat.xmpp.send.enqueued",
      "chat.xmpp.session.lifecycle",
      "chat.xmpp.status",
      "chat.xmpp.status",
    ]);

    const measurementTypes = stub.measurements.map((m) => m.type);
    expect(measurementTypes).toEqual([
      "chat.xmpp.message.acked.latency_ms",
      "chat.xmpp.queue.depth",
      "chat.xmpp.reconnect.duration_ms",
    ]);

    const ackMeasurement = stub.measurements[0];
    expect(ackMeasurement.values.latency_ms).toBe(123.4);
    expect(ackMeasurement.context).toEqual({ kind: "room" });

    const depthMeasurement = stub.measurements[1];
    expect(depthMeasurement.values).toEqual({ persisted: 2, inflight: 1 });

    const reconnectMeasurement = stub.measurements[2];
    expect(reconnectMeasurement.values.duration_ms).toBe(4_321);
  });
});

describe("BrowserXmppClient telemetry hooks", () => {
  test("hooks fire in addition to primary handlers", () => {
    const client = new BrowserXmppClient(session());

    const primaryAcks: string[] = [];
    const hookAcks: Array<{ id: string; kind: "room" | "dm"; latencyMs: number }> = [];
    client.setMessageAckHandler((id) => primaryAcks.push(id));
    client.onMessageAcked((id, meta) => hookAcks.push({ id, kind: meta.kind, latencyMs: meta.latencyMs }));

    // Stub the pending-send bookkeeping by simulating a flush that
    // added an inflight entry plus a pending timestamp, then emit ack.
    const internal = client as unknown as {
      inflightQueuedIds: Set<string>;
      pendingSendAt: Map<string, { at: number; kind: "room" | "dm" }>;
      xmpp: { on: (name: string, fn: (msg: unknown) => void) => void; emit: (name: string, msg: unknown) => void };
      wireEvents: (xmpp: unknown) => void;
    };
    const handlers = new Map<string, Array<(msg: unknown) => void>>();
    const stubXmpp = {
      on(event: string, handler: (msg: unknown) => void) {
        const list = handlers.get(event) ?? [];
        list.push(handler);
        handlers.set(event, list);
      },
      emit(event: string, msg: unknown) {
        for (const h of handlers.get(event) ?? []) h(msg);
      },
    };
    internal.xmpp = stubXmpp as never;
    internal.wireEvents(stubXmpp);
    internal.inflightQueuedIds.add("room-1");
    internal.pendingSendAt.set("room-1", { at: performance.now() - 42, kind: "room" });

    stubXmpp.emit("message:acked", { id: "room-1" });

    expect(primaryAcks).toEqual(["room-1"]);
    expect(hookAcks).toHaveLength(1);
    expect(hookAcks[0].id).toBe("room-1");
    expect(hookAcks[0].kind).toBe("room");
    expect(hookAcks[0].latencyMs).toBeGreaterThanOrEqual(0);
  });

  test("installInstrumentation forwards client hooks to Faro api", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    const internal = client as unknown as {
      inflightQueuedIds: Set<string>;
      pendingSendAt: Map<string, { at: number; kind: "room" | "dm" }>;
      xmpp: unknown;
      wireEvents: (xmpp: unknown) => void;
      emitStatus: (snap: { state: string; detail?: string }) => void;
      emitSessionLifecycle: (evt: { type: "fresh" | "resumed" }) => void;
    };
    const handlers = new Map<string, Array<(msg: unknown) => void>>();
    const stubXmpp = {
      on(event: string, handler: (msg: unknown) => void) {
        const list = handlers.get(event) ?? [];
        list.push(handler);
        handlers.set(event, list);
      },
      emit(event: string, msg: unknown) {
        for (const h of handlers.get(event) ?? []) h(msg);
      },
    };
    internal.xmpp = stubXmpp;
    internal.wireEvents(stubXmpp);
    internal.inflightQueuedIds.add("dm-9");
    internal.pendingSendAt.set("dm-9", { at: performance.now() - 10, kind: "dm" });
    // Record the pending kind so the failure hook reports it truthfully.
    internal.pendingSendAt.set("live-1", { at: performance.now() - 5, kind: "dm" });

    // Ack → Faro event + measurement
    (stubXmpp as { emit: (e: string, m: unknown) => void }).emit("message:acked", { id: "dm-9" });
    // Fail → Faro event
    (stubXmpp as { emit: (e: string, m: unknown) => void }).emit("message:failed", { id: "live-1" });
    // Session lifecycle + status
    internal.emitSessionLifecycle({ type: "resumed" });
    internal.emitStatus({ state: "reconnecting", detail: "ws dropped" });
    internal.emitStatus({ state: "online", detail: "back" });

    const eventNames = stub.events.map((e) => e.name);
    expect(eventNames).toContain("chat.xmpp.message.acked");
    expect(eventNames).toContain("chat.xmpp.message.failed");
    expect(eventNames).toContain("chat.xmpp.session.lifecycle");
    expect(eventNames.filter((n) => n === "chat.xmpp.status")).toHaveLength(2);

    const reconnectMeasurement = stub.measurements.find(
      (m) => m.type === "chat.xmpp.reconnect.duration_ms",
    );
    expect(reconnectMeasurement).toBeDefined();
    expect(reconnectMeasurement?.values.duration_ms).toBeGreaterThanOrEqual(0);
  });

  test("client.onError forwards XMPP failures to Faro pushError with kind tagging", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    const internal = client as unknown as {
      emitError: (event: {
        kind: "stream" | "auth" | "connect-timeout";
        recoverable: boolean;
        detail: string;
        cause?: unknown;
        condition?: string;
      }) => void;
    };

    // Fatal stream error with an XMPP condition.
    const streamCause = new Error("peer closed stream");
    internal.emitError({
      kind: "stream",
      recoverable: false,
      detail: "not-authorized",
      cause: streamCause,
      condition: "not-authorized",
    });

    // Recoverable auth (treated as non-recoverable from the client's POV
    // since refresh failed — but the shape is what we verify).
    internal.emitError({
      kind: "auth",
      recoverable: false,
      detail: "Session expired (no refresh available)",
    });

    // Recoverable connect-timeout (Rust client stalled, client discards + retries).
    internal.emitError({
      kind: "connect-timeout",
      recoverable: true,
      detail: "Rust client reconnect stalled past 15s; discarding agent",
    });

    expect(stub.errors).toHaveLength(3);

    const streamErr = stub.errors[0];
    expect(streamErr.error).toBe(streamCause);
    expect(streamErr.options?.type).toBe("xmpp.stream");
    expect(streamErr.options?.context?.kind).toBe("xmpp.stream");
    expect(streamErr.options?.context?.recoverable).toBe("false");
    expect(streamErr.options?.context?.condition).toBe("not-authorized");

    const authErr = stub.errors[1];
    expect(authErr.options?.type).toBe("xmpp.auth");
    expect(authErr.options?.context?.recoverable).toBe("false");

    const timeoutErr = stub.errors[2];
    expect(timeoutErr.options?.type).toBe("xmpp.disconnect");
    expect(timeoutErr.options?.context?.recoverable).toBe("true");
  });

  test("enqueuing a send fires onSendEnqueued + onQueueDepthChange", async () => {
    const client = new BrowserXmppClient(session());
    // Force the slow path: make connect throw so we stay in "no-client" reason.
    (client as unknown as { connect: ReturnType<typeof mock> }).connect = mock(async () => {
      throw new Error("Reconnection timed out");
    });
    (client as unknown as { switchRoom: ReturnType<typeof mock> }).switchRoom = mock(async () => {
      throw new Error("Reconnection timed out");
    });

    const enqueued: Array<{ kind: string; reason: string }> = [];
    const depths: Array<{ persisted: number; inflight: number }> = [];
    client.onSendEnqueued((info) => enqueued.push(info));
    client.onQueueDepthChange((d) => depths.push(d));

    await client.sendDirectMessage("bob@example.com", "hi", { id: "dm-queued-1" });

    expect(enqueued).toHaveLength(1);
    expect(enqueued[0].kind).toBe("dm");
    expect(depths).toHaveLength(1);
    expect(depths[0].persisted).toBe(1);
  });

  test("hook exceptions are swallowed and do not break chat", () => {
    const client = new BrowserXmppClient(session());
    client.onMessageAcked(() => {
      throw new Error("hook failure");
    });

    const internal = client as unknown as {
      inflightQueuedIds: Set<string>;
      pendingSendAt: Map<string, { at: number; kind: "room" | "dm" }>;
      xmpp: unknown;
      wireEvents: (xmpp: unknown) => void;
    };
    const handlers = new Map<string, Array<(msg: unknown) => void>>();
    const stubXmpp = {
      on(event: string, handler: (msg: unknown) => void) {
        const list = handlers.get(event) ?? [];
        list.push(handler);
        handlers.set(event, list);
      },
      emit(event: string, msg: unknown) {
        for (const h of handlers.get(event) ?? []) h(msg);
      },
    };
    internal.xmpp = stubXmpp;
    internal.wireEvents(stubXmpp);
    internal.pendingSendAt.set("m-bad", { at: performance.now(), kind: "dm" });

    expect(() => {
      (stubXmpp as { emit: (e: string, m: unknown) => void }).emit("message:acked", { id: "m-bad" });
    }).not.toThrow();
  });

  test("message:failed emits queue depth and preserves recorded DM kind", () => {
    const client = new BrowserXmppClient(session());

    const fails: Array<{ id: string; kind: "room" | "dm" }> = [];
    const depths: Array<{ persisted: number; inflight: number }> = [];
    client.onMessageDeliveryFailed((id, meta) => fails.push({ id, kind: meta.kind }));
    client.onQueueDepthChange((d) => depths.push(d));

    const internal = client as unknown as {
      inflightQueuedIds: Set<string>;
      pendingSendAt: Map<string, { at: number; kind: "room" | "dm" }>;
      xmpp: unknown;
      wireEvents: (xmpp: unknown) => void;
    };
    const handlers = new Map<string, Array<(msg: unknown) => void>>();
    const stubXmpp = {
      on(event: string, handler: (msg: unknown) => void) {
        const list = handlers.get(event) ?? [];
        list.push(handler);
        handlers.set(event, list);
      },
      emit(event: string, msg: unknown) {
        for (const h of handlers.get(event) ?? []) h(msg);
      },
    };
    internal.xmpp = stubXmpp;
    internal.wireEvents(stubXmpp);
    internal.inflightQueuedIds.add("dm-fail-1");
    internal.pendingSendAt.set("dm-fail-1", { at: performance.now(), kind: "dm" });

    stubXmpp.emit("message:failed", { id: "dm-fail-1" });

    expect(fails).toEqual([{ id: "dm-fail-1", kind: "dm" }]);
    expect(depths).toHaveLength(1);
    expect(depths[0].inflight).toBe(0);
  });

  test("message:failed without a recorded pending entry does not fire the failure hook", () => {
    const client = new BrowserXmppClient(session());

    const fails: string[] = [];
    client.onMessageDeliveryFailed((id) => fails.push(id));

    const internal = client as unknown as {
      xmpp: unknown;
      wireEvents: (xmpp: unknown) => void;
    };
    const handlers = new Map<string, Array<(msg: unknown) => void>>();
    const stubXmpp = {
      on(event: string, handler: (msg: unknown) => void) {
        const list = handlers.get(event) ?? [];
        list.push(handler);
        handlers.set(event, list);
      },
      emit(event: string, msg: unknown) {
        for (const h of handlers.get(event) ?? []) h(msg);
      },
    };
    internal.xmpp = stubXmpp;
    internal.wireEvents(stubXmpp);

    stubXmpp.emit("message:failed", { id: "orphan" });

    expect(fails).toEqual([]);
  });

  test("reconnect duration is only reported on reconnecting → online", () => {
    const client = new BrowserXmppClient(session());

    const reconnectDurations: number[] = [];
    client.onStatus((_snap, meta) => {
      if (meta.reconnectDurationMs !== undefined) {
        reconnectDurations.push(meta.reconnectDurationMs);
      }
    });

    const internal = client as unknown as {
      emitStatus: (snap: { state: string; detail?: string }) => void;
    };

    // reconnecting → offline: should NOT emit a duration.
    internal.emitStatus({ state: "reconnecting" });
    internal.emitStatus({ state: "offline" });
    expect(reconnectDurations).toEqual([]);

    // reconnecting → online: SHOULD emit a duration.
    internal.emitStatus({ state: "reconnecting" });
    internal.emitStatus({ state: "online" });
    expect(reconnectDurations).toHaveLength(1);
    expect(reconnectDurations[0]).toBeGreaterThanOrEqual(0);

    // reconnecting → error: should NOT emit, and should reset the timer
    // so a subsequent bare `online` doesn't retroactively emit.
    internal.emitStatus({ state: "reconnecting" });
    internal.emitStatus({ state: "error", detail: "fatal" });
    internal.emitStatus({ state: "online" });
    expect(reconnectDurations).toHaveLength(1);
  });
});
