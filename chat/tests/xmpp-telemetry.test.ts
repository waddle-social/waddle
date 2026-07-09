import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  __clearSensitiveUrlsForTesting,
  __setFaroForTesting,
  __setGateZeroFaroScopeForTesting,
  initTelemetry,
  reportCallAudioProcessing,
  reportAuthBootstrap,
  reportCatchup,
  reportMessageAcked,
  reportMessageFailed,
  reportQueueDepthChange,
  reportReconnectScheduled,
  reportResumeDrain,
  reportSendEnqueued,
  reportSessionLifecycle,
  reportStatusChange,
} from "../src/lib/telemetry";
import {
  createFaroStub,
  createStorageMock,
  GATE_ZERO_SCOPE,
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
      }),
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
    });

    expect(stub.events).toEqual([
      {
        name: "chat.call.audio_processing",
        attributes: {
          kind: "active",
          noise_suppression: "off",
          echo_cancellation: "off",
          auto_gain_control: "unknown",
          ai_noise_filter: "rnnoise",
        },
      },
    ]);
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
      reportAuthBootstrap({ outcome: "ready", durationMs: 10 });
      reportMessageFailed({ id: "x", kind: "dm" });
      reportSendEnqueued({ kind: "room", reason: "offline" });
      reportQueueDepthChange({ persisted: 0, inflight: 0 });
      reportSessionLifecycle({ type: "fresh" });
      reportStatusChange({ state: "online" });
      reportReconnectScheduled({ attempt: 1, delayMs: 2_000 });
      reportCatchup({ conversations: 1, pages: 1, messages: 1, durationMs: 10 });
      reportResumeDrain({ buffered: 1, durationMs: 10 });
    }).not.toThrow();
  });

  test("auth bootstrap telemetry cannot break authentication state", () => {
    __setFaroForTesting({
      api: {
        pushEvent: () => {
          throw new Error("event transport failed");
        },
        pushMeasurement: () => {
          throw new Error("measurement transport failed");
        },
      },
    } as never);

    expect(() =>
      reportAuthBootstrap({ outcome: "ready", durationMs: 10 }),
    ).not.toThrow();
  });

  test("report functions forward to Faro api when initialized", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);

    reportMessageAcked({ id: "m-1", kind: "room", latencyMs: 123.4 });
    reportAuthBootstrap({ outcome: "expired", durationMs: 27.6 });
    reportMessageFailed({ id: "m-2", kind: "dm" });
    reportSendEnqueued({ kind: "dm", reason: "offline" });
    reportQueueDepthChange({ persisted: 2, inflight: 1 });
    reportSessionLifecycle({ type: "resumed" });
    reportStatusChange({ state: "reconnecting" });
    reportStatusChange({ state: "online", reconnectDurationMs: 4_321 });
    reportReconnectScheduled({ attempt: 3, delayMs: 8_000 });
    reportCatchup({
      conversations: 2,
      processedConversations: 1,
      pages: 4,
      messages: 12,
      durationMs: 88.6,
      outcome: "aborted",
    });
    reportResumeDrain({ buffered: 5, durationMs: 12.4 });

    const eventNames = stub.events.map((e) => e.name);
    expect(eventNames).toEqual([
      "chat.xmpp.message.acked",
      "chat.journey.auth",
      "chat.xmpp.message.failed",
      "chat.xmpp.send.enqueued",
      "chat.xmpp.session.lifecycle",
      "chat.xmpp.status",
      "chat.xmpp.status",
      "chat.xmpp.reconnect.scheduled",
    ]);
    expect(stub.events[0].attributes).toEqual({ kind: "room", ...GATE_ZERO_SCOPE });
    expect(stub.events[1].attributes).toEqual({ outcome: "expired", ...GATE_ZERO_SCOPE });
    expect(stub.events[2].attributes).toEqual({ kind: "dm" });
    expect(stub.events[4].attributes).toEqual({ type: "resumed", ...GATE_ZERO_SCOPE });
    expect(stub.events[5].attributes).toEqual({ state: "reconnecting" });
    expect(stub.events[7].attributes).toEqual({ visibility: "visible", hidden_bucket: "visible" });

    const measurementTypes = stub.measurements.map((m) => m.type);
    expect(measurementTypes).toEqual([
      "chat.xmpp.message.acked.latency_ms",
      "chat.journey.auth.duration_ms",
      "chat.xmpp.queue.depth",
      "chat.xmpp.reconnect.duration_ms",
      "chat.xmpp.reconnect.attempt",
      "chat.xmpp.catchup",
      "chat.xmpp.resume_drain",
    ]);

    const ackMeasurement = stub.measurements[0];
    expect(ackMeasurement.values.latency_ms).toBe(123.4);
    expect(ackMeasurement.context).toEqual({ kind: "room", ...GATE_ZERO_SCOPE });

    const authMeasurement = stub.measurements[1];
    expect(authMeasurement.values).toEqual({ duration_ms: 28 });
    expect(authMeasurement.context).toEqual({ outcome: "expired", ...GATE_ZERO_SCOPE });

    const depthMeasurement = stub.measurements[2];
    expect(depthMeasurement.values).toEqual({ persisted: 2, inflight: 1 });

    const reconnectMeasurement = stub.measurements[3];
    expect(reconnectMeasurement.values.duration_ms).toBe(4_321);
    expect(reconnectMeasurement.context).toEqual(GATE_ZERO_SCOPE);

    const reconnectAttempt = stub.measurements[4];
    expect(reconnectAttempt.values).toEqual({
      count: 1,
      attempt: 3,
      delay_ms: 8_000,
      hidden_ms: 0,
    });
    expect(reconnectAttempt.context).toEqual({ visibility: "visible", hidden_bucket: "visible" });

    const catchup = stub.measurements[5];
    expect(catchup.values).toEqual({
      conversations: 2,
      processed_conversations: 1,
      pages: 4,
      messages: 12,
      duration_ms: 89,
      hidden_ms: 0,
    });
    expect(catchup.context).toEqual({
      visibility: "visible",
      hidden_bucket: "visible",
      outcome: "aborted",
    });

    const resumeDrain = stub.measurements[6];
    expect(resumeDrain.values).toEqual({ buffered: 5, duration_ms: 12, hidden_ms: 0 });
    expect(resumeDrain.context).toEqual({ visibility: "visible", hidden_bucket: "visible" });
  });

});
