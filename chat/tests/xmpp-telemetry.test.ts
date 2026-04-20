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
    xmpp_websocket_url: "wss://example.com/xmpp",
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
  return {
    events,
    measurements,
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
});
