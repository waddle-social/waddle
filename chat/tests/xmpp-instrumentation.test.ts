import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { BrowserXmppClient, type SessionLifecycleEvent } from "../src/lib/xmpp-client";
import {
  __clearSensitiveUrlsForTesting,
  __setFaroForTesting,
  __setGateZeroFaroScopeForTesting,
} from "../src/lib/telemetry";
import { installInstrumentation } from "../src/lib/xmpp/xmpp-instrumentation";
import {
  createFaroStub,
  createStorageMock,
  GATE_ZERO_SCOPE,
  session,
} from "./support/telemetry";

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
  __setGateZeroFaroScopeForTesting(GATE_ZERO_SCOPE);
  __clearSensitiveUrlsForTesting();
});

afterEach(() => {
  localStorage.clear();
  __setFaroForTesting(null);
  __clearSensitiveUrlsForTesting();
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
      outboundQueue: {
        markInflight: (id: string) => void;
        notePendingSend: (id: string | null, kind: "room" | "dm") => void;
      };
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
    internal.outboundQueue.markInflight("room-1");
    internal.outboundQueue.notePendingSend("room-1", "room");

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
      outboundQueue: {
        markInflight: (id: string) => void;
        notePendingSend: (id: string | null, kind: "room" | "dm") => void;
      };
      xmpp: unknown;
      wireEvents: (xmpp: unknown) => void;
      emitStatus: (snap: { state: string; detail?: string }) => void;
      emitSessionLifecycle: (evt: SessionLifecycleEvent) => void;
      events: { emitSafe: (event: string, ...args: unknown[]) => void };
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
    internal.outboundQueue.markInflight("dm-9");
    internal.outboundQueue.notePendingSend("dm-9", "dm");
    // Record the pending kind so the failure hook reports it truthfully.
    internal.outboundQueue.notePendingSend("live-1", "dm");

    // Ack → Faro event + measurement
    (stubXmpp as { emit: (e: string, m: unknown) => void }).emit("message:acked", { id: "dm-9" });
    // Fail → Faro event
    (stubXmpp as { emit: (e: string, m: unknown) => void }).emit("message:failed", { id: "live-1" });
    // Session lifecycle + status
    internal.emitSessionLifecycle({ type: "resumed" });
    internal.emitStatus({ state: "reconnecting", detail: "ws dropped" });
    internal.emitStatus({ state: "online", detail: "back" });
    // Background-tab health hooks
    internal.events.emitSafe("reconnectScheduled", { attempt: 1, delayMs: 2_000 });
    internal.events.emitSafe("catchup", {
      conversations: 1,
      processedConversations: 1,
      pages: 2,
      messages: 3,
      durationMs: 4,
      outcome: "completed",
    });
    internal.events.emitSafe("resumeDrain", { buffered: 7, durationMs: 8 });

    const eventNames = stub.events.map((e) => e.name);
    expect(eventNames).toContain("chat.xmpp.message.acked");
    expect(eventNames).toContain("chat.xmpp.message.failed");
    expect(eventNames).toContain("chat.xmpp.session.lifecycle");
    expect(eventNames).toContain("chat.xmpp.reconnect.scheduled");
    expect(eventNames.filter((n) => n === "chat.xmpp.status")).toHaveLength(2);

    const reconnectMeasurement = stub.measurements.find(
      (m) => m.type === "chat.xmpp.reconnect.duration_ms",
    );
    expect(reconnectMeasurement).toBeDefined();
    expect(reconnectMeasurement?.values.duration_ms).toBeGreaterThanOrEqual(0);
    expect(stub.measurements.some((m) => m.type === "chat.xmpp.reconnect.attempt")).toBe(true);
    expect(stub.measurements.some((m) => m.type === "chat.xmpp.catchup")).toBe(true);
    expect(stub.measurements.some((m) => m.type === "chat.xmpp.resume_drain")).toBe(true);
  });

  test("catch-up hook reports failed outcomes and processed conversation count", async () => {
    const client = new BrowserXmppClient(session());
    const events: Array<{
      conversations: number;
      processedConversations: number;
      pages: number;
      messages: number;
      outcome: string;
    }> = [];
    client.onCatchup((info) => events.push(info));

    const xmpp = {
      fetch_dm_history_page: mock(async (peer: string) => {
        if (peer === "carol@example.com") throw new Error("MAM failed");
        return {
          messages: [{
            mam_id: "mam-2",
            id: "dm-2",
            from: "bob@example.com/phone",
            to: "alice@example.com/desktop",
            message_type: "chat",
            body: "missed while suspended",
            timestamp: "2024-01-01T00:00:01.000Z",
            reaction_emojis: [],
            shared_files: [],
          }],
          complete: true,
        };
      }),
    };

    const internal = client as unknown as {
      xmpp: unknown;
      connected: boolean;
      runReconnectCatchup: (
        xmpp: unknown,
        entries: Array<{ kind: "dm"; key: string; after?: string }>,
      ) => Promise<void>;
    };
    internal.xmpp = xmpp;
    internal.connected = true;

    await internal.runReconnectCatchup(xmpp, [
      { kind: "dm", key: "bob@example.com", after: "mam-1" },
      { kind: "dm", key: "carol@example.com", after: "mam-1" },
    ]);

    expect(events).toHaveLength(1);
    expect(events[0]).toMatchObject({
      conversations: 2,
      processedConversations: 2,
      pages: 1,
      messages: 1,
      outcome: "failed",
    });
  });

  test("client.onError forwards XMPP failures to Faro pushError with kind tagging", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    const internal = client as unknown as {
      emitError: (event: {
        kind: "stream" | "auth" | "connect-timeout" | "member-query";
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
    internal.emitError({
      kind: "member-query",
      recoverable: true,
      detail: "affiliation query failed for owner",
      condition: "room@example.com",
    });

    expect(stub.errors).toHaveLength(4);

    const streamErr = stub.errors[0];
    expect(streamErr.error).not.toBe(streamCause);
    expect(streamErr.error.message).toBe("stream-not-authorized");
    expect(streamErr.options?.type).toBe("xmpp.stream");
    expect(streamErr.options?.context?.kind).toBe("xmpp.stream");
    expect(streamErr.options?.context?.recoverable).toBe("false");
    expect(streamErr.options?.context?.detail).toBe("stream-not-authorized");
    expect(streamErr.options?.context?.condition).toBe("not-authorized");

    const authErr = stub.errors[1];
    expect(authErr.options?.type).toBe("xmpp.auth");
    expect(authErr.options?.context?.recoverable).toBe("false");
    expect(authErr.options?.context?.detail).toBe("auth-error");

    const timeoutErr = stub.errors[2];
    expect(timeoutErr.options?.type).toBe("xmpp.disconnect");
    expect(timeoutErr.options?.context?.recoverable).toBe("true");
    expect(timeoutErr.options?.context?.detail).toBe("connect-timeout");

    const memberErr = stub.errors[3];
    expect(memberErr.options?.type).toBe("xmpp.stream");
    expect(memberErr.options?.context?.condition).toBe("unknown");
    expect(memberErr.options?.context?.detail).toBe("member-query-unknown");
  });

  test("self-presence join timeout reports a stable room timeout detail", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    const internal = client as unknown as {
      emitError: (event: {
        kind: "connect-timeout";
        recoverable: boolean;
        detail: string;
      }) => void;
    };

    internal.emitError({
      kind: "connect-timeout",
      recoverable: true,
      detail: "Timed out waiting for self-presence in c1@muc.example.com",
    });

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].options?.type).toBe("xmpp.disconnect");
    expect(stub.errors[0].options?.context?.recoverable).toBe("true");
    expect(stub.errors[0].options?.context?.detail).toBe("room-self-presence-timeout");
  });

  test("set_on_error bridge preserves stream error conditions for telemetry", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    let onError: ((detail: TestXmppStreamErrorPayload) => void) | null = null;
    const xmpp = {
      set_on_error(cb: NonNullable<typeof onError>) {
        onError = cb;
      },
    };
    const internal = client as unknown as {
      xmpp: typeof xmpp;
      wireEvents: (xmpp: typeof xmpp) => void;
    };
    internal.xmpp = xmpp;
    internal.wireEvents(xmpp);

    onError?.({ detail: "stream error", condition: "not-authorized" });

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].error.message).toBe("stream-not-authorized");
    expect(stub.errors[0].options?.type).toBe("xmpp.stream");
    expect(stub.errors[0].options?.context?.condition).toBe("not-authorized");
    expect(stub.errors[0].options?.context?.detail).toBe("stream-not-authorized");
  });

  test("set_on_error bridge recovers stream condition from object detail", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    let onError: ((detail: TestXmppStreamErrorPayload) => void) | null = null;
    const xmpp = {
      set_on_error(cb: NonNullable<typeof onError>) {
        onError = cb;
      },
    };
    const internal = client as unknown as {
      xmpp: typeof xmpp;
      wireEvents: (xmpp: typeof xmpp) => void;
    };
    internal.xmpp = xmpp;
    internal.wireEvents(xmpp);

    onError?.({ detail: "not-authorized" });

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].error.message).toBe("stream-not-authorized");
    expect(stub.errors[0].options?.type).toBe("xmpp.stream");
    expect(stub.errors[0].options?.context?.condition).toBe("not-authorized");
    expect(stub.errors[0].options?.context?.detail).toBe("stream-not-authorized");
  });

  test("set_on_error bridge keeps stanza-only conditions out of stream telemetry", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    let onError: ((detail: TestXmppStreamErrorPayload) => void) | null = null;
    const xmpp = {
      set_on_error(cb: NonNullable<typeof onError>) {
        onError = cb;
      },
    };
    const internal = client as unknown as {
      xmpp: typeof xmpp;
      wireEvents: (xmpp: typeof xmpp) => void;
    };
    internal.xmpp = xmpp;
    internal.wireEvents(xmpp);

    onError?.("forbidden");

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].error.message).toBe("stream-error");
    expect(stub.errors[0].options?.type).toBe("xmpp.stream");
    expect(stub.errors[0].options?.context?.condition).toBeUndefined();
    expect(stub.errors[0].options?.context?.detail).toBe("stream-error");
  });

  test("set_on_error bridge classifies driver errors without exporting raw stream detail", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    let onError: ((detail: TestXmppStreamErrorPayload) => void) | null = null;
    const xmpp = {
      set_on_error(cb: NonNullable<typeof onError>) {
        onError = cb;
      },
    };
    const internal = client as unknown as {
      xmpp: typeof xmpp;
      wireEvents: (xmpp: typeof xmpp) => void;
    };
    internal.xmpp = xmpp;
    internal.wireEvents(xmpp);

    onError?.("websocket transport error");

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].error.message).toBe("stream-transport-error");
    expect(stub.errors[0].options?.type).toBe("xmpp.stream");
    expect(stub.errors[0].options?.context?.condition).toBeUndefined();
    expect(stub.errors[0].options?.context?.detail).toBe("stream-transport-error");
    expect(stub.errors[0].options?.context?.streamDetail).toBeUndefined();
  });

  test("set_on_error bridge never exports arbitrary XMPP stream text", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    let onError: ((detail: TestXmppStreamErrorPayload) => void) | null = null;
    const xmpp = {
      set_on_error(cb: NonNullable<typeof onError>) {
        onError = cb;
      },
    };
    const internal = client as unknown as {
      xmpp: typeof xmpp;
      wireEvents: (xmpp: typeof xmpp) => void;
    };
    internal.xmpp = xmpp;
    internal.wireEvents(xmpp);

    onError?.("peer text included alice@example.com/phone and raw-stream-secret");

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].error.message).toBe("stream-error");
    expect(stub.errors[0].error.stack).toBeUndefined();
    expect(stub.errors[0].options?.context).toEqual({
      kind: "xmpp.stream",
      recoverable: "true",
      detail: "stream-error",
    });
    expect(JSON.stringify(stub.errors[0])).not.toContain("alice@example.com");
    expect(JSON.stringify(stub.errors[0])).not.toContain("raw-stream-secret");
  });

  test("set_on_error bridge names handled-count detail when SM metadata is absent", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    let onError: ((detail: TestXmppStreamErrorPayload) => void) | null = null;
    const xmpp = {
      set_on_error(cb: NonNullable<typeof onError>) {
        onError = cb;
      },
    };
    const internal = client as unknown as {
      xmpp: typeof xmpp;
      wireEvents: (xmpp: typeof xmpp) => void;
    };
    internal.xmpp = xmpp;
    internal.wireEvents(xmpp);

    onError?.({ detail: "handled-count-too-high" });

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].error.message).toBe("stream-handled-count-too-high");
    expect(stub.errors[0].options?.type).toBe("xmpp.stream");
    expect(stub.errors[0].options?.context?.condition).toBeUndefined();
    expect(stub.errors[0].options?.context?.detail).toBe("stream-handled-count-too-high");
  });

  test("set_on_error bridge names XEP-0198 handled-count stream failures", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    let onError: ((detail: TestXmppStreamErrorPayload) => void) | null = null;
    const xmpp = {
      set_on_error(cb: NonNullable<typeof onError>) {
        onError = cb;
      },
    };
    const internal = client as unknown as {
      xmpp: typeof xmpp;
      wireEvents: (xmpp: typeof xmpp) => void;
    };
    internal.xmpp = xmpp;
    internal.wireEvents(xmpp);

    onError?.({
      detail: "stream error",
      condition: "undefined-condition",
      streamManagementError: {
        kind: "handled-count-too-high",
        h: 3,
        sendCount: 2,
      },
    });

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].error.message).toBe("stream-handled-count-too-high");
    expect(stub.errors[0].options?.type).toBe("xmpp.stream");
    expect(stub.errors[0].options?.context?.condition).toBe("undefined-condition");
    expect(stub.errors[0].options?.context?.detail).toBe("stream-handled-count-too-high");
    expect(stub.errors[0].options?.context?.smH).toBe("3");
    expect(stub.errors[0].options?.context?.smSendCount).toBe("2");
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
      outboundQueue: {
        markInflight: (id: string) => void;
        notePendingSend: (id: string | null, kind: "room" | "dm") => void;
      };
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
    internal.outboundQueue.notePendingSend("m-bad", "dm");

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
      outboundQueue: {
        markInflight: (id: string) => void;
        notePendingSend: (id: string | null, kind: "room" | "dm") => void;
      };
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
    internal.outboundQueue.markInflight("dm-fail-1");
    internal.outboundQueue.notePendingSend("dm-fail-1", "dm");

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
