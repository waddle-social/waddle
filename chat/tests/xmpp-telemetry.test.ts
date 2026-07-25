import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import {
  BrowserXmppClient,
  roomBareJidFor,
  type SessionLifecycleEvent,
} from "../src/lib/xmpp-client";
import { __createFallbackXmppResourceForTesting } from "../src/lib/xmpp/client";
import { RoomJoinRetryCoordinator } from "../src/lib/xmpp/room-join-retry";
import {
  __clearSensitiveUrlsForTesting,
  __recordSpanExceptionForTesting,
  __scrubMissingFetchSpanUrlForTesting,
  __scrubSpanUrlForTesting,
  __scrubXhrSpanUrlForTesting,
  __sanitizeFaroTransportItemForTesting,
  __setFaroForTesting,
  __websocketUrlWithTraceparentForTesting,
  initTelemetry,
  markSensitiveUrlForTelemetry,
  reportCallAudioProcessing,
  reportCallMediaPath,
  reportCatchup,
  reportError,
  reportDisplayedMarkerFailure,
  reportMessageAcked,
  reportMessageFailed,
  reportQueueDepthChange,
  reportReconnectScheduled,
  reportResumeDrain,
  reportSendEnqueued,
  reportSessionLifecycle,
  reportStatusChange,
  setXmppResourceForTelemetry,
  websocketUrlWithTraceparent,
} from "../src/lib/telemetry";
import { DiscoTimeoutError, discoverChannels } from "../src/lib/xmpp/discovery";
import { installInstrumentation } from "../src/lib/xmpp/xmpp-instrumentation";
import type { ReconnectCatchupEntry } from "../src/lib/xmpp/reconnect-catchup";
import { ManualRoomJoinRetryTimer } from "./helpers/manual-room-join-retry-timer";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
    ...partial,
  } as WaddleSession;
}

type TestXmppStreamErrorPayload = string | {
  detail?: string;
  condition?: string;
  streamManagementError?: { kind: "handled-count-too-high"; h: number; sendCount: number };
};

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
  const sessions: Array<{ attributes?: Record<string, string> }> = [];
  let currentSession: { id?: string; attributes?: Record<string, string> } = {
    id: "faro-session",
    attributes: { retained: "yes" },
  };
  return {
    events,
    measurements,
    errors,
    sessions,
    api: {
      getSession: () => currentSession,
      setSession: (meta: { attributes?: Record<string, string> }) => {
        currentSession = meta;
        sessions.push(meta);
      },
      pushEvent: (name: string, attributes?: Record<string, string>) => {
        events.push({ name, attributes });
      },
      pushMeasurement: (payload: {
        type: string;
        values: Record<string, number>;
        context?: Record<string, string>;
      }, options?: { context?: Record<string, string> }) => {
        measurements.push({ ...payload, context: options?.context });
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

describe("reportCallAudioProcessing", () => {
  test("is a no-op when Faro is not initialized", () => {
    expect(() =>
      reportCallAudioProcessing({
        processing: {
          kind: "active",
          noiseSuppression: "on",
          echoCancellation: "off",
          autoGainControl: "unknown",
        },
        aiNoiseFilter: { kind: "active", model: null },
      }, "dm"),
    ).not.toThrow();
  });

  test("pushes a single mapped audio-processing event when initialized", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    reportCallAudioProcessing({
      processing: {
        kind: "active",
        noiseSuppression: "off",
        echoCancellation: "off",
        autoGainControl: "unknown",
      },
      aiNoiseFilter: { kind: "active", model: "rnnoise" },
    }, "muc");

    expect(stub.events).toEqual([
      {
        name: "chat.call.audio_processing",
        attributes: {
          kind: "active",
          noise_suppression: "off",
          echo_cancellation: "off",
          auto_gain_control: "unknown",
          ai_noise_filter: "rnnoise",
          call_kind: "muc",
        },
      },
    ]);
  });
});

describe("call beacon kind tagging", () => {
  test("adds call_kind to media-path events", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    reportCallMediaPath({
      direction: "send",
      source: "camera",
      codec: "VP9",
      iceCandidateType: "host",
      iceTransport: "udp",
      audioBitrateBand: null,
      videoResolutionBand: "720p",
    }, "dm");

    expect(stub.events[0]).toEqual({
      name: "chat.call.media_path",
      attributes: {
        direction: "send",
        source: "camera",
        codec: "VP9",
        ice_candidate_type: "host",
        ice_transport: "udp",
        video_resolution_band: "720p",
        call_kind: "dm",
      },
    });
  });
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
      reportQueueDepthChange({ kind: "dm", persisted: 0, inflight: 0 });
      reportSessionLifecycle({ type: "fresh" });
      reportStatusChange({ state: "online" });
      reportReconnectScheduled({ attempt: 1, delayMs: 2_000 });
      reportCatchup({ conversations: 1, pages: 1, pageFailures: 0, messages: 1, durationMs: 10 });
      reportResumeDrain({ buffered: 1, durationMs: 10 });
    }).not.toThrow();
  });

  test("report functions forward to Faro api when initialized", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    reportMessageAcked({ id: "m-1", kind: "room", latencyMs: 123.4 });
    reportMessageFailed({ id: "m-2", kind: "dm" });
    reportSendEnqueued({ kind: "dm", reason: "offline" });
    reportQueueDepthChange({ kind: "room", persisted: 2, inflight: 1 });
    reportSessionLifecycle({ type: "resumed" });
    reportStatusChange({ state: "reconnecting" });
    reportStatusChange({ state: "online", reconnectDurationMs: 4_321 });
    reportReconnectScheduled({ attempt: 3, delayMs: 8_000 });
    reportCatchup({
      conversations: 2,
      processedConversations: 1,
      pages: 4,
      pageFailures: 2,
      messages: 12,
      durationMs: 88.6,
      outcome: "aborted",
    });
    reportResumeDrain({ buffered: 5, durationMs: 12.4 });

    const eventNames = stub.events.map((e) => e.name);
    expect(eventNames).toEqual([
      "chat.xmpp.message.acked",
      "chat.xmpp.message.failed",
      "chat.xmpp.send.enqueued",
      "chat.xmpp.session.lifecycle",
      "chat.xmpp.status",
      "chat.xmpp.status",
      "chat.xmpp.reconnect.scheduled",
    ]);
    expect(stub.events[0].attributes).toEqual({ kind: "room" });
    expect(stub.events[1].attributes).toEqual({ kind: "dm" });
    expect(stub.events[4].attributes).toEqual({ state: "reconnecting" });
    expect(stub.events[6].attributes).toEqual({ visibility: "visible", hidden_bucket: "visible" });

    const measurementTypes = stub.measurements.map((m) => m.type);
    expect(measurementTypes).toEqual([
      "chat.xmpp.message.acked.latency_ms",
      "chat.xmpp.queue.depth",
      "chat.xmpp.reconnect.duration_ms",
      "chat.xmpp.reconnect.attempt",
      "chat.xmpp.catchup",
      "chat.xmpp.resume_drain",
    ]);

    const ackMeasurement = stub.measurements[0];
    expect(ackMeasurement.values.latency_ms).toBe(123.4);
    expect(ackMeasurement.context).toEqual({ kind: "room" });

    const depthMeasurement = stub.measurements[1];
    expect(depthMeasurement.values).toEqual({ persisted: 2, inflight: 1 });
    expect(depthMeasurement.context).toEqual({ kind: "room" });

    const reconnectMeasurement = stub.measurements[2];
    expect(reconnectMeasurement.values.duration_ms).toBe(4_321);

    const reconnectAttempt = stub.measurements[3];
    expect(reconnectAttempt.values).toEqual({
      count: 1,
      attempt: 3,
      delay_ms: 8_000,
      hidden_ms: 0,
    });
    expect(reconnectAttempt.context).toEqual({ visibility: "visible", hidden_bucket: "visible" });

    const catchup = stub.measurements[4];
    expect(catchup.values).toEqual({
      conversations: 2,
      processed_conversations: 1,
      pages: 4,
      page_failures: 2,
      messages: 12,
      duration_ms: 89,
      hidden_ms: 0,
    });
    expect(catchup.context).toEqual({
      visibility: "visible",
      hidden_bucket: "visible",
      outcome: "aborted",
    });

    const resumeDrain = stub.measurements[5];
    expect(resumeDrain.values).toEqual({ buffered: 5, duration_ms: 12, hidden_ms: 0 });
    expect(resumeDrain.context).toEqual({ visibility: "visible", hidden_bucket: "visible" });
  });

  test("queue-depth measurements are deduped per kind until the reading changes (#1443)", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    // Every queue mutation reports both kinds; only the kind that actually
    // moved may reach Faro, otherwise each send ships two 2-4KB beacons.
    reportQueueDepthChange({ kind: "dm", persisted: 1, inflight: 0 });
    reportQueueDepthChange({ kind: "room", persisted: 0, inflight: 0 });
    reportQueueDepthChange({ kind: "dm", persisted: 1, inflight: 0 });
    reportQueueDepthChange({ kind: "room", persisted: 0, inflight: 0 });
    reportQueueDepthChange({ kind: "dm", persisted: 1, inflight: 1 });
    reportQueueDepthChange({ kind: "dm", persisted: 0, inflight: 0 });

    expect(stub.measurements.map((m) => ({ ...m.values, ...m.context }))).toEqual([
      { persisted: 1, inflight: 0, kind: "dm" },
      { persisted: 0, inflight: 0, kind: "room" },
      { persisted: 1, inflight: 1, kind: "dm" },
      { persisted: 0, inflight: 0, kind: "dm" },
    ]);
  });

  test("queue-depth dedupe state resets with the Faro instance (#1443)", () => {
    const first = createFaroStub();
    __setFaroForTesting(first as never);
    reportQueueDepthChange({ kind: "dm", persisted: 3, inflight: 2 });

    const second = createFaroStub();
    __setFaroForTesting(second as never);
    reportQueueDepthChange({ kind: "dm", persisted: 3, inflight: 2 });

    expect(second.measurements).toHaveLength(1);
  });

  test("maps displayed-marker failures to low-cardinality latency bands", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    reportDisplayedMarkerFailure({
      direction: "send",
      kind: "dm",
      reason: "send-failed",
      roundTripMs: 1_250,
    });
    reportDisplayedMarkerFailure({
      direction: "receive",
      kind: "room",
      reason: "receive-processing-failed",
      roundTripMs: null,
    });

    expect(stub.events).toEqual([
      {
        name: "chat.xmpp.displayed_marker.failed",
        attributes: {
          direction: "send",
          kind: "dm",
          reason: "send-failed",
          round_trip_latency_band: "1s-5s",
        },
      },
      {
        name: "chat.xmpp.displayed_marker.failed",
        attributes: {
          direction: "receive",
          kind: "room",
          reason: "receive-processing-failed",
          round_trip_latency_band: "unknown",
        },
      },
    ]);
  });

  test("records the generated XMPP resource as a Faro session attribute", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session({ jid: "alice@example.com" }));
    installInstrumentation(client);

    expect(client.fullJid).toMatch(/^alice@example\.com\/web-/);
    expect(stub.sessions).toEqual([{
      id: "faro-session",
      attributes: {
        retained: "yes",
        xmpp_resource: client.xmppResource,
      },
    }]);
  });

  test("keeps the no-randomUUID XMPP resource fallback UUID-shaped", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const resource = __createFallbackXmppResourceForTesting(new Uint8Array(16));
    setXmppResourceForTelemetry(resource);

    expect(resource).toBe("web-00000000-0000-4000-8000-000000000000");
    expect(stub.sessions.at(-1)?.attributes?.xmpp_resource).toBe(resource);
  });

  test("passes a valid traceparent while dropping session-bearing query values", () => {
    const traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    expect(__scrubSpanUrlForTesting(
      `wss://xmpp.example/ws?session_id=secret&traceparent=${traceparent}`,
      ["https://xmpp.example"],
    )).toBe(`wss://xmpp.example/:route?traceparent=${traceparent}`);
  });

  test("generates valid random trace context when there is no active span", () => {
    const result = new URL(
      websocketUrlWithTraceparent("wss://xmpp.example/ws?transport=websocket"),
    );

    expect(result.searchParams.get("transport")).toBe("websocket");
    expect(result.searchParams.get("traceparent"))
      .toMatch(/^00-(?!0{32})[0-9a-f]{32}-(?!0{16})[0-9a-f]{16}-00$/);
  });

  test("leaves the WebSocket URL unchanged when Web Crypto is unavailable", () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, "crypto");
    const value = "wss://xmpp.example/ws?transport=websocket";
    Object.defineProperty(globalThis, "crypto", {
      configurable: true,
      value: undefined,
    });

    try {
      expect(websocketUrlWithTraceparent(value)).toBe(value);
    } finally {
      if (original) Object.defineProperty(globalThis, "crypto", original);
      else Reflect.deleteProperty(globalThis, "crypto");
    }
  });

  test("passes a well-formed traceparent into the XMPP WebSocket configuration", async () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    let configuredUrl = "";

    class StubConfig {
      constructor(url: string, ..._args: unknown[]) {
        configuredUrl = url;
      }
    }
    class StubClient {
      async connect(): Promise<void> {}
      async disconnect(): Promise<void> {}
    }

    const client = new BrowserXmppClient(session({
      xmpp_websocket_url: "wss://xmpp.example/ws?session_id=secret",
    }));
    const state = client as unknown as {
      connectTimeoutMs: number;
      loadModule: () => Promise<unknown>;
      reconnect: { clearTimer: () => void };
    };
    state.connectTimeoutMs = 1_000;
    state.loadModule = async () => ({ WaddleConfig: StubConfig, WaddleClient: StubClient });

    const pendingConnect = client.connect();
    pendingConnect.catch(() => undefined);
    await new Promise((resolve) => setTimeout(resolve, 0));

    const configured = new URL(configuredUrl);
    expect(configured.searchParams.get("session_id")).toBe("secret");
    expect(configured.searchParams.get("traceparent"))
      .toMatch(/^00-(?!0{32})[0-9a-f]{32}-(?!0{16})[0-9a-f]{16}-00$/);

    await client.disconnect();
    await pendingConnect.catch(() => undefined);
    state.reconnect.clearTimer();
  });

  test("appends the active trace context without dropping existing WebSocket query values", () => {
    expect(__websocketUrlWithTraceparentForTesting(
      "wss://xmpp.example/ws?transport=websocket",
      {
        traceId: "4bf92f3577b34da6a3ce929d0e0e4736",
        spanId: "00f067aa0ba902b7",
        traceFlags: 1,
      },
    )).toBe(
      "wss://xmpp.example/ws?transport=websocket&traceparent=00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    );
  });

  test("drops W3C-invalid all-zero trace identifiers", () => {
    const zeroTraceparent = "00-00000000000000000000000000000000-0000000000000000-01";
    expect(__scrubSpanUrlForTesting(
      `wss://xmpp.example/ws?traceparent=${zeroTraceparent}`,
      ["https://xmpp.example"],
    )).toBe("wss://xmpp.example/:route");
  });

  test("reportError drops identifier-bearing context fields", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    reportError("storage.read", new Error("boom"), {
      recoverable: true,
      detail: "storage failed",
      accountKey: "alice@example.com",
      api_key: "secret",
      jid: "alice@example.com/desktop",
      key: "waddle.chat.sm-resume.alice@example.com",
      note: "download /api/files/slot-1/file.png?waddle_session_id=tok",
      queueSize: 2,
      storage_area: "outbound-queue",
      storageKey: "waddle.chat.outbound.alice@example.com",
    });

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].options?.context).toEqual({
      kind: "storage.read",
      recoverable: "true",
      detail: "storage failed",
      note: "download /api/files/:slot/:file?waddle_session_id=:redacted",
      queueSize: "2",
      storage_area: "outbound-queue",
    });
  });

  test("each failed join attempt emits one Faro exception across listeners and spans", async () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    const client = new BrowserXmppClient(session());
    installInstrumentation(client);
    const retryTimer = new ManualRoomJoinRetryTimer();
    const observedByFirstListener: Error[] = [];
    const observedBySecondListener: Error[] = [];
    client.onError((event) => {
      if (event.kind === "muc-join" && event.cause instanceof Error) {
        observedByFirstListener.push(event.cause);
      }
    });
    client.onError((event) => {
      if (event.kind === "muc-join" && event.cause instanceof Error) {
        observedBySecondListener.push(event.cause);
      }
    });
    const roomJid = roomBareJidFor(session(), "busy");
    let onPresence: ((presence: {
      from?: string;
      presence_type: string;
      error_condition?: string;
      error_type?: string;
    }) => void) | null = null;
    const joinRoom = mock(async () => undefined);
    const xmpp = {
      join_room: joinRoom,
      set_on_presence(callback: NonNullable<typeof onPresence>) {
        onPresence = callback;
      },
    };
    const internal = client as unknown as {
      xmpp: typeof xmpp;
      connected: boolean;
      retainedJoinedRoomJids: Set<string>;
      roomJoinRetry: RoomJoinRetryCoordinator;
      wireEvents: (xmpp: typeof xmpp) => void;
    };
    internal.xmpp = xmpp;
    internal.connected = true;
    internal.retainedJoinedRoomJids.add(roomJid);
    internal.roomJoinRetry = new RoomJoinRetryCoordinator({
      timer: retryTimer,
      random: () => 1,
    });
    internal.wireEvents(xmpp);

    const firstAttempt = client.fanOutAutoJoin([roomJid]);
    await Promise.resolve();
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
      error_condition: "resource-constraint",
      error_type: "wait",
    });
    await firstAttempt;

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].error.message).toBe("room-join-resource-constraint");
    const firstJoinListener = client.ensureJoined(roomJid);
    const secondJoinListener = client.ensureJoined(roomJid);
    retryTimer.runNext();
    await Promise.resolve();
    onPresence?.({
      from: `${roomJid}/alice`,
      presence_type: "error",
      error_condition: "resource-constraint",
      error_type: "wait",
    });
    await Promise.allSettled([firstJoinListener, secondJoinListener]);

    expect(joinRoom).toHaveBeenCalledTimes(2);
    expect(stub.errors).toHaveLength(2);
    expect(stub.errors.map(({ error }) => error.message)).toEqual([
      "room-join-resource-constraint",
      "room-join-resource-constraint",
    ]);
    expect(observedByFirstListener).toHaveLength(2);
    expect(observedBySecondListener).toHaveLength(2);
    for (const failure of observedByFirstListener) {
      expect(__recordSpanExceptionForTesting(failure)).toBe(0);
    }
  });

  test("each self-presence timeout attempt emits one Faro exception across listeners and spans", async () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    const client = new BrowserXmppClient(session());
    installInstrumentation(client);
    const retryTimer = new ManualRoomJoinRetryTimer();
    const observedByFirstListener: Error[] = [];
    const observedBySecondListener: Error[] = [];
    client.onError((event) => {
      if (event.kind === "muc-join-timeout" && event.cause instanceof Error) {
        observedByFirstListener.push(event.cause);
      }
    });
    client.onError((event) => {
      if (event.kind === "muc-join-timeout" && event.cause instanceof Error) {
        observedBySecondListener.push(event.cause);
      }
    });

    const roomJid = roomBareJidFor(session(), "slow");
    const joinRoom = mock(async () => undefined);
    const internal = client as unknown as {
      xmpp: { join_room: typeof joinRoom };
      connected: boolean;
      retainedJoinedRoomJids: Set<string>;
      roomJoinRetry: RoomJoinRetryCoordinator;
    };
    internal.xmpp = { join_room: joinRoom };
    internal.connected = true;
    internal.retainedJoinedRoomJids.add(roomJid);
    internal.roomJoinRetry = new RoomJoinRetryCoordinator({
      timer: retryTimer,
      random: () => 1,
    });

    const originalSetTimeout = globalThis.setTimeout;
    let selfPresenceTimeout: (() => void) | null = null;
    globalThis.setTimeout = ((callback: TimerHandler, delayMs?: number) => {
      if (delayMs === 15_000 && typeof callback === "function") {
        selfPresenceTimeout = callback;
        return 1 as unknown as ReturnType<typeof setTimeout>;
      }
      return originalSetTimeout(callback, delayMs);
    }) as typeof setTimeout;

    try {
      const firstAttempt = client.fanOutAutoJoin([roomJid]);
      await Promise.resolve();
      expect(selfPresenceTimeout).not.toBeNull();
      selfPresenceTimeout?.();
      await firstAttempt;

      expect(stub.errors).toHaveLength(1);
      expect(stub.errors[0].error.message).toBe("room-self-presence-timeout");

      const firstJoinListener = client.ensureJoined(roomJid);
      const secondJoinListener = client.ensureJoined(roomJid);
      retryTimer.runNext();
      await Promise.resolve();
      expect(selfPresenceTimeout).not.toBeNull();
      selfPresenceTimeout?.();
      await Promise.allSettled([firstJoinListener, secondJoinListener]);

      expect(joinRoom).toHaveBeenCalledTimes(2);
      expect(stub.errors).toHaveLength(2);
      expect(stub.errors.map(({ error }) => error.message)).toEqual([
        "room-self-presence-timeout",
        "room-self-presence-timeout",
      ]);
      expect(observedByFirstListener).toHaveLength(2);
      expect(observedBySecondListener).toHaveLength(2);
      for (const failure of observedByFirstListener) {
        expect(__recordSpanExceptionForTesting(failure)).toBe(0);
      }
    } finally {
      globalThis.setTimeout = originalSetTimeout;
    }
  });

  test("non-join XMPP failures remain available to span exception recording", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    const client = new BrowserXmppClient(session());
    installInstrumentation(client);
    const failure = new Error("history replay failed");
    const internal = client as unknown as {
      emitError: (event: {
        kind: "history";
        recoverable: boolean;
        detail: string;
        cause: Error;
      }) => void;
    };

    internal.emitError({
      kind: "history",
      recoverable: true,
      detail: "history replay failed",
      cause: failure,
    });

    expect(stub.errors).toHaveLength(1);
    expect(__recordSpanExceptionForTesting(failure)).toBe(1);
    expect(__recordSpanExceptionForTesting(failure)).toBe(0);
  });

  test("transport sanitizer replaces Faro page URLs with route templates", () => {
    let currentUrl = new URL("https://chat.example/dm/alice?thread=secret-thread#waddle_session_id=tok");
    const location = {
      get href() { return currentUrl.href; },
      get origin() { return currentUrl.origin; },
      get pathname() { return currentUrl.pathname; },
      get search() { return currentUrl.search; },
      get hash() { return currentUrl.hash; },
    } as Location;
    (globalThis as typeof globalThis & { window: Window }).window = {
      ...(originalWindow ?? {}),
      location,
    } as Window;

    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "event",
      payload: {},
      meta: {
        page: {
          id: currentUrl.href,
          url: currentUrl.href,
          attributes: {
            referrer: "https://chat.example/api/files/slot-1/file.png?waddle_session_id=tok",
          },
        },
      },
    } as never);

    expect(sanitized.meta.page).toEqual({
      id: "/dm/:user",
      url: "https://chat.example/dm/:user",
      attributes: {
        referrer: "https://chat.example/api/files/:slot/:file?waddle_session_id=:redacted",
      },
    });

    currentUrl = new URL("https://chat.example/r/general/x/polls/results?pinned=1&thread=reply");
    expect(__sanitizeFaroTransportItemForTesting({ payload: {}, meta: {} } as never).meta.page).toEqual({
      id: "/r/:room/x/:plugin/:route",
      url: "https://chat.example/r/:room/x/:plugin/:route",
      attributes: undefined,
    });
  });

  test("span URL scrubber redacts untrusted XEP file-transfer URLs", () => {
    expect(__scrubSpanUrlForTesting(
      "https://uploads.example/signed/slot-secret/file.png?token=secret#fragment",
      ["https://chat.example", "https://xmpp.example"],
    )).toBe("external:unknown");

    expect(__scrubSpanUrlForTesting(
      "https://xmpp.example/api/messages?session_id=tok#fragment",
      ["https://xmpp.example"],
    )).toBe("https://xmpp.example/api/:endpoint");

    expect(__scrubSpanUrlForTesting(
      "https://chat.example/api/files/slot-1/file.png?download=1",
      ["https://chat.example"],
    )).toBe("https://chat.example/api/files/:slot/:file");

    expect(__scrubSpanUrlForTesting(
      "https://chat.example/dm/alice?thread=secret-thread",
      ["https://chat.example"],
    )).toBe("https://chat.example/dm/:user");

    expect(__scrubSpanUrlForTesting(
      "https://chat.example/r/general/x/polls/results?pinned=1&thread=reply",
      ["https://chat.example"],
    )).toBe("https://chat.example/r/:room/x/:plugin/:route");

    markSensitiveUrlForTelemetry("https://chat.example/signed-upload/slot-secret/file.png?token=secret");
    expect(__scrubSpanUrlForTesting(
      "https://chat.example/signed-upload/slot-secret/file.png?token=secret",
      ["https://chat.example"],
    )).toBe("file-transfer:unknown");

    expect(__scrubXhrSpanUrlForTesting("", ["https://chat.example"])).toEqual({
      "http.host": ":redacted",
      "http.target": ":unknown",
      "http.url": "xhr:unknown",
      "server.address": ":redacted",
      "server.port": 0,
      "url.full": "xhr:unknown",
      "url.path": ":unknown",
    });

    expect(__scrubMissingFetchSpanUrlForTesting()).toEqual({
      "http.host": ":redacted",
      "http.target": ":unknown",
      "http.url": "fetch:unknown",
      "server.address": ":redacted",
      "server.port": 0,
      "url.full": "fetch:unknown",
      "url.path": ":unknown",
    });
  });

  test("transport sanitizer redacts finalized trace span URL attributes", () => {
    const currentUrl = new URL("https://chat.example/r/general");
    (globalThis as typeof globalThis & { window: Window }).window = {
      ...(originalWindow ?? {}),
      location: {
        get href() { return currentUrl.href; },
        get origin() { return currentUrl.origin; },
        get pathname() { return currentUrl.pathname; },
      } as Location,
    } as Window;

    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "trace",
      payload: {
        resourceSpans: [{
          scopeSpans: [{
            spans: [{
              attributes: [
                { key: "http.url", value: { stringValue: "https://uploads.example/signed/slot-secret/file.png?token=secret" } },
                { key: "url.full", value: { stringValue: "https://uploads.example/signed/slot-secret/file.png?token=secret" } },
                { key: "http.host", value: { stringValue: "uploads.example" } },
                { key: "http.target", value: { stringValue: "/signed/slot-secret/file.png?token=secret" } },
                { key: "http.status_text", value: { stringValue: "Signed URL expired for slot-secret" } },
                { key: "server.address", value: { stringValue: "uploads.example" } },
                { key: "server.port", value: { intValue: 443 } },
                { key: "url.path", value: { stringValue: "/signed/slot-secret/file.png" } },
                { key: "url.query", value: { stringValue: "token=secret" } },
              ],
            }],
          }],
        }],
      },
      meta: {},
    } as never) as unknown as {
      payload: {
        resourceSpans: Array<{
          scopeSpans: Array<{
            spans: Array<{
              attributes: Array<{ key: string; value: { stringValue?: string; intValue?: number } }>;
            }>;
          }>;
        }>;
      };
    };

    const attributes = Object.fromEntries(
      sanitized.payload.resourceSpans[0].scopeSpans[0].spans[0].attributes.map((attr) => [
        attr.key,
        attr.value.stringValue ?? attr.value.intValue,
      ]),
    );
    expect(attributes).toEqual({
      "http.host": ":redacted",
      "http.status_text": ":redacted",
      "http.target": ":unknown",
      "http.url": "external:unknown",
      "server.address": ":redacted",
      "server.port": 0,
      "url.full": "external:unknown",
      "url.path": ":unknown",
      "url.query": ":redacted",
    });
  });

  test("transport sanitizer redacts synthetic Faro trace event attributes", () => {
    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "event",
      payload: {
        name: "faro.tracing.fetch",
        attributes: {
          "http.host": "uploads.example",
          "http.status_text": "Signed URL expired for slot-secret",
          "http.target": "/signed/slot-secret/file.png?token=secret",
          "http.url": "https://uploads.example/signed/slot-secret/file.png?token=secret",
          "server.address": "uploads.example",
          "server.port": "443",
          "url.full": "https://uploads.example/signed/slot-secret/file.png?token=secret",
          "url.path": "/signed/slot-secret/file.png",
          "url.query": "token=secret",
          peer: "bob@example.com/phone",
        },
      },
      meta: {},
    } as never) as unknown as {
      payload: { attributes: Record<string, string> };
    };

    expect(sanitized.payload.attributes).toEqual({
      "http.host": ":redacted",
      "http.status_text": ":redacted",
      "http.target": ":unknown",
      "http.url": "external:unknown",
      "server.address": ":redacted",
      "server.port": "0",
      "url.full": "external:unknown",
      "url.path": ":unknown",
      "url.query": ":redacted",
      peer: ":jid",
    });
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
      pageFailures: 0,
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
      pageFailures: number;
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
        entries: ReconnectCatchupEntry[],
      ) => Promise<void>;
    };
    internal.xmpp = xmpp;
    internal.connected = true;

    await internal.runReconnectCatchup(xmpp, [
      { kind: "dm", key: "bob@example.com", scope: "account", after: "mam-1" },
      { kind: "dm", key: "carol@example.com", scope: "account", after: "mam-1" },
    ]);

    expect(events).toHaveLength(1);
    expect(events[0]).toMatchObject({
      conversations: 2,
      processedConversations: 2,
      pages: 1,
      pageFailures: 1,
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
        kind: "muc-join-timeout";
        recoverable: boolean;
        detail: string;
      }) => void;
    };

    internal.emitError({
      kind: "muc-join-timeout",
      recoverable: true,
      detail: "Timed out waiting for self-presence in c1@muc.example.com",
    });

    expect(stub.errors).toHaveLength(1);
    expect(stub.errors[0].options?.type).toBe("xmpp.disconnect");
    expect(stub.errors[0].options?.context?.recoverable).toBe("true");
    expect(stub.errors[0].options?.context?.detail).toBe("room-self-presence-timeout");
    expect(stub.errors[0].options?.context?.errorSource).toBe("local-timeout");
  });

  test("stanza error context carries errorType, errorText, and errorSource", () => {
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
        errorType?: string;
        errorText?: string;
      }) => void;
    };

    // Server-returned stanza error with full context.
    internal.emitError({
      kind: "member-query",
      recoverable: true,
      detail: "affiliation query failed for owner",
      condition: "item-not-found",
      errorType: "cancel",
      errorText: "no such room",
    });

    // RFC 6120 defined condition outside the old allowlist must survive.
    internal.emitError({
      kind: "member-query",
      recoverable: true,
      detail: "affiliation query failed for owner",
      condition: "not-allowed",
      errorType: "cancel",
    });

    // No condition: source is unattributed, so errorSource stays unset.
    internal.emitError({
      kind: "member-query",
      recoverable: true,
      detail: "affiliation query failed for owner",
    });

    expect(stub.errors).toHaveLength(3);

    const serverErr = stub.errors[0];
    expect(serverErr.options?.context?.condition).toBe("item-not-found");
    expect(serverErr.options?.context?.errorType).toBe("cancel");
    expect(serverErr.options?.context?.errorText).toBe("no such room");
    expect(serverErr.options?.context?.errorSource).toBe("server");
    expect(serverErr.options?.context?.detail).toBe("member-query-item-not-found");

    const definedCondition = stub.errors[1];
    expect(definedCondition.options?.context?.condition).toBe("not-allowed");
    expect(definedCondition.options?.context?.detail).toBe("member-query-not-allowed");

    const unattributed = stub.errors[2];
    expect(unattributed.options?.context?.condition).toBeUndefined();
    expect(unattributed.options?.context?.errorSource).toBeUndefined();
    expect(unattributed.options?.context?.detail).toBe("member-query-failed");
  });

  test("rejected MUC join presence reports the stanza error condition", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    const client = new BrowserXmppClient(session());
    installInstrumentation(client);

    const internal = client as unknown as {
      emitError: (event: {
        kind: "muc-join";
        recoverable: boolean;
        detail: string;
        condition?: string;
        errorType?: string;
        errorText?: string;
        roomLocalpart?: string;
      }) => void;
    };

    internal.emitError({
      kind: "muc-join",
      recoverable: true,
      detail: "room join rejected — room@muc.example.com",
      condition: "registration-required",
      errorType: "auth",
      errorText: "private invitation for alice@example.com says secret words",
      roomLocalpart: "room",
    });

    expect(stub.errors).toHaveLength(1);
    const joinErr = stub.errors[0];
    expect(joinErr.options?.type).toBe("xmpp.stream");
    expect(joinErr.options?.context?.detail).toBe("room-join-registration-required");
    expect(joinErr.options?.context?.condition).toBe("registration-required");
    expect(joinErr.options?.context?.errorType).toBe("auth");
    expect(joinErr.options?.context?.errorText).toBeUndefined();
    expect(joinErr.options?.context?.errorSource).toBe("server");
    expect(joinErr.options?.context?.roomLocalpart).toBe("room");
    expect(joinErr.options?.context?.roomLocalpart).not.toContain("@");
  });

  test("disco failures report to Faro with condition or local-timeout attribution", async () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    // Server-returned stanza error: the wasm bridge rejects send_raw_iq
    // with a structured Error carrying the condition.
    const stanzaRejection = Object.assign(
      new Error("server returned a stanza error: cancel: service-unavailable"),
      { condition: "service-unavailable", errorType: "cancel" },
    );
    await discoverChannels(
      { send_raw_iq: async () => { throw stanzaRejection; } },
      "alice@example.com",
    ).catch(() => undefined);
    const serverErr = stub.errors.find(
      (entry) => entry.options?.context?.detail === "disco-items-service-unavailable",
    );
    expect(serverErr).toBeDefined();
    expect(serverErr?.options?.context?.condition).toBe("service-unavailable");
    expect(serverErr?.options?.context?.errorSource).toBe("server");

    // Local timeout: DiscoTimeoutError is attributed to the client timer.
    stub.errors.length = 0;
    await discoverChannels(
      { send_raw_iq: async () => { throw new DiscoTimeoutError("muc.example.com", undefined, 30); } },
      "alice@example.com",
    ).catch(() => undefined);
    const timeoutErr = stub.errors.find(
      (entry) => entry.options?.context?.detail === "disco-items-timeout",
    );
    expect(timeoutErr).toBeDefined();
    expect(timeoutErr?.options?.context?.errorSource).toBe("local-timeout");
    expect(timeoutErr?.options?.context?.condition).toBeUndefined();

    // Condition-less rejections (e.g. "client is disconnected" during a
    // connection flap) must stay console-only — one beacon per in-flight
    // room IQ would flood Faro.
    stub.errors.length = 0;
    await discoverChannels(
      { send_raw_iq: async () => { throw "client is disconnected"; } },
      "alice@example.com",
    ).catch(() => undefined);
    expect(stub.errors).toHaveLength(0);
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
    expect(stub.errors[0].options?.context?.streamDetail).toBe("not-authorized");
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

  test("set_on_error bridge classifies driver errors without losing stream detail", () => {
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
    expect(stub.errors[0].options?.context?.streamDetail).toBe("websocket transport error");
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
    expect(stub.errors[0].options?.context?.streamDetail).toBe("handled-count-too-high");
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
    const depths: Array<{ kind: "room" | "dm"; persisted: number; inflight: number }> = [];
    client.onSendEnqueued((info) => enqueued.push(info));
    client.onQueueDepthChange((d) => depths.push(d));

    await client.sendDirectMessage("bob@example.com", "hi", { id: "dm-queued-1" });

    expect(enqueued).toHaveLength(1);
    expect(enqueued[0].kind).toBe("dm");
    expect(depths).toHaveLength(2);
    expect(depths.find((depth) => depth.kind === "dm")?.persisted).toBe(1);
    expect(depths.find((depth) => depth.kind === "room")?.persisted).toBe(0);
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
    const depths: Array<{ kind: "room" | "dm"; persisted: number; inflight: number }> = [];
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
    expect(depths).toHaveLength(2);
    expect(depths.find((depth) => depth.kind === "dm")?.inflight).toBe(0);
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
