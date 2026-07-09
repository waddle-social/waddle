import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  __clearSensitiveUrlsForTesting,
  __faroSessionTrackingConfigForTesting,
  __sanitizeFaroTransportItemForTesting,
  __setFaroForTesting,
  __setGateZeroFaroScopeForTesting,
  reportAuthBootstrap,
  reportError,
  reportMessageAcked,
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

describe("telemetry privacy boundary", () => {
  test("Gate 0 signals fail closed when static deployment scope is absent", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    __setGateZeroFaroScopeForTesting(null);

    reportAuthBootstrap({ outcome: "ready", durationMs: 10 });
    reportMessageAcked({ id: "m-1", kind: "dm", latencyMs: 20 });
    reportSessionLifecycle({ type: "fresh" });
    reportStatusChange({ state: "online", reconnectDurationMs: 30 });

    expect(stub.events).toEqual([{ name: "chat.xmpp.status", attributes: { state: "online" } }]);
    expect(stub.measurements).toEqual([]);
  });

  test("reportError exports only bounded categories and numeric context", () => {
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
    expect(stub.errors[0].error.message).toBe("storage.read");
    expect(stub.errors[0].error.stack).toBeUndefined();
    expect(stub.errors[0].options?.context).toEqual({
      kind: "storage.read",
      recoverable: "true",
      queueSize: "2",
      storage_area: "outbound-queue",
  });
});
  test("reportError never exports arbitrary messages, stacks, or unknown context strings", () => {
    const stub = createFaroStub();
    __setFaroForTesting(stub as never);
    const error = new Error(
      "callback https://idp.example/oauth/callback?code=code-secret&state=state-secret#done Authorization: Bearer bearer-secret redirect_uri=https%3A%2F%2Fchat.example%2Fcallback",
    );
    error.stack = [
      error.message,
      "at authorize (https://chat.example/assets/auth.js?access_token=access-secret#L12:8)",
      "refresh_token=refresh-secret Basic basic-secret",
    ].join("\n");

    reportError("xmpp.auth", error, {
      recoverable: false,
      detail: "POST /oauth/callback?code=context-code&state=context-state#complete",
    });

    const reported = stub.errors[0];
    const emitted = JSON.stringify(reported);
    for (const secret of [
      "code-secret",
      "state-secret",
      "bearer-secret",
      "access-secret",
      "refresh-secret",
      "basic-secret",
      "context-code",
      "context-state",
    ]) {
      expect(emitted).not.toContain(secret);
    }
    expect(reported.error.message).toBe("xmpp.auth");
    expect(reported.error.stack).toBeUndefined();
    expect(reported.options?.context).toEqual({
      kind: "xmpp.auth",
      recoverable: "false",
    });
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
      payload: { name: "chat.xmpp.status", attributes: { state: "online" } },
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
        referrer: "https://chat.example/:route",
      },
    });

    const externalReferrer = __sanitizeFaroTransportItemForTesting({
      type: "event",
      payload: { name: "chat.xmpp.status", attributes: { state: "online" } },
      meta: { page: { attributes: { referrer: "https://search.example/results?q=private" } } },
    } as never);
    expect(externalReferrer.meta.page?.attributes?.referrer).toBe("external");

    currentUrl = new URL("https://chat.example/r/general/x/polls/results?pinned=1&thread=reply");
    expect(__sanitizeFaroTransportItemForTesting({
      type: "event",
      payload: { name: "chat.xmpp.status", attributes: { state: "online" } },
      meta: {},
    } as never)?.meta.page).toEqual({
      id: "/r/:room/x/:plugin/:route",
      url: "https://chat.example/r/:room/x/:plugin/:route",
      attributes: undefined,
    });
  });

  test("transport sanitizer allowlists meta and removes identifier-bearing payload fields", () => {
    const currentUrl = new URL("https://chat.example/r/private-room?session_id=meta-secret");
    (globalThis as typeof globalThis & { window: Window }).window = {
      ...(originalWindow ?? {}),
      location: {
        get href() { return currentUrl.href; },
        get origin() { return currentUrl.origin; },
        get pathname() { return currentUrl.pathname; },
      } as Location,
    } as Window;

    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "event",
      payload: {
        name: "chat.journey.auth",
        attributes: {
          outcome: "ready",
          deploymentEnvironment: "production",
          cluster: "waddle-cloud",
          namespace: "waddle",
          sourceId: "waddle-chat",
          release: "0123456789abcdef0123456789abcdef01234567",
          session_id: "payload-session-secret",
          userId: "payload-user-secret",
          authorization: "Bearer payload-token-secret",
          note: "https://idp.example/authorize?code=url-code-secret&state=url-state-secret#complete",
        },
      },
      meta: {
        app: {
          name: "waddle-chat",
          version: "1.2.3",
          environment: "production",
          release: "0123456789abcdef0123456789abcdef01234567",
          gitHash: "0123456789abcdef0123456789abcdef01234567",
          installationId: "installation-secret",
        },
        sdk: {
          name: "faro-web-sdk",
          version: "2.7.0",
          integrations: [{ name: "fetch", version: "1" }],
        },
        user: { id: "user-secret", email: "alice@example.com" },
        session: { id: "session-secret", attributes: { previousSessionId: "previous-secret" } },
        browser: { userAgent: "fingerprint-secret" },
        page: {
          attributes: {
            referrer: "https://chat.example/dm/alice?code=referrer-secret",
            roomId: "room-secret",
            previousSessionId: "previous-secret",
          },
        },
      },
    } as never) as unknown as {
      meta: Record<string, unknown> & {
        app: Record<string, unknown>;
        page: { attributes?: Record<string, string>; id?: string; url?: string };
      };
      payload: { attributes: Record<string, string> };
    };

    expect(sanitized.meta).toEqual({
      app: {
        environment: "production",
        gitHash: "0123456789abcdef0123456789abcdef01234567",
        name: "waddle-chat",
        release: "0123456789abcdef0123456789abcdef01234567",
        version: "1.2.3",
      },
      sdk: {
        integrations: [{ name: "fetch", version: "1" }],
        name: "faro-web-sdk",
        version: "2.7.0",
      },
      page: {
        attributes: { referrer: "https://chat.example/dm/:user" },
        id: "/r/:room",
        url: "https://chat.example/r/:room",
      },
    });
    expect(sanitized.payload.attributes).toEqual({
      outcome: "ready",
      deploymentEnvironment: "production",
      cluster: "waddle-cloud",
      namespace: "waddle",
      sourceId: "waddle-chat",
      release: "0123456789abcdef0123456789abcdef01234567",
    });
    const emitted = JSON.stringify(sanitized);
    for (const secret of [
      "installation-secret",
      "user-secret",
      "session-secret",
      "previous-secret",
      "fingerprint-secret",
      "payload-session-secret",
      "payload-user-secret",
      "payload-token-secret",
      "url-code-secret",
      "url-state-secret",
      "referrer-secret",
      "room-secret",
      "alice@example.com",
    ]) {
      expect(emitted).not.toContain(secret);
    }
  });

  test("transport sanitizer drops raw bodies, rooms, tokens, cookies, URLs, and phone numbers", () => {
    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "event",
      payload: {
        name: "chat.xmpp.status",
        attributes: {
          state: "online",
          body: "private message body",
          room: "secret-room",
          token: "opaque-secret",
          url: "https://chat.example/r/private-room",
          phone: "+44 7700 900123",
        },
        body: "raw stanza body",
        headers: { cookie: "waddle_session=secret-cookie" },
      },
      meta: {},
    } as never) as unknown as { payload: Record<string, unknown> };

    expect(sanitized.payload).toEqual({
      name: "chat.xmpp.status",
      attributes: { state: "online" },
    });
    expect(JSON.stringify(sanitized)).not.toMatch(
      /private message|secret-room|opaque-secret|private-room|7700|stanza body|secret-cookie/,
    );

    expect(__sanitizeFaroTransportItemForTesting({
      type: "event",
      payload: {
        name: "future.unreviewed",
        attributes: { safeLookingKey: "private free text" },
      },
      meta: {},
    } as never)).toBeNull();
  });

  test("transport sanitizer accepts every catalogued Gate 0 Faro envelope only with static scope", () => {
    const items = [
      {
        type: "event",
        payload: {
          name: "chat.journey.auth",
          attributes: { outcome: "ready", ...GATE_ZERO_SCOPE },
        },
      },
      {
        type: "measurement",
        payload: {
          type: "chat.journey.auth.duration_ms",
          values: { duration_ms: 42 },
          context: { outcome: "ready", ...GATE_ZERO_SCOPE },
        },
      },
      {
        type: "measurement",
        payload: {
          type: "chat.xmpp.message.acked.latency_ms",
          values: { latency_ms: 42 },
          context: { kind: "dm", ...GATE_ZERO_SCOPE },
        },
      },
      {
        type: "event",
        payload: {
          name: "chat.xmpp.session.lifecycle",
          attributes: { type: "resumed", ...GATE_ZERO_SCOPE },
        },
      },
      {
        type: "measurement",
        payload: {
          type: "chat.xmpp.reconnect.duration_ms",
          values: { duration_ms: 42 },
          context: GATE_ZERO_SCOPE,
        },
      },
    ];

    for (const item of items) {
      expect(__sanitizeFaroTransportItemForTesting({ ...item, meta: {} } as never)).not.toBeNull();
    }

    const unscoped = structuredClone(items[2]);
    delete (unscoped.payload as { context?: unknown }).context;
    expect(__sanitizeFaroTransportItemForTesting({ ...unscoped, meta: {} } as never)).toBeNull();
  });

  test("Faro sessions are disabled and session headers are removed at transport", () => {
    expect(__faroSessionTrackingConfigForTesting()).toEqual({ enabled: false });

    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "measurement",
      payload: {
        type: "chat.xmpp.queue.depth",
        values: { persisted: 1, inflight: 0 },
        headers: {
          "Content-Type": "application/json",
          "x-faro-session-id": "faro-session-secret",
        },
      },
      meta: {},
    } as never) as unknown as { payload: Record<string, unknown> };

    expect(sanitized.payload).toEqual({
      type: "chat.xmpp.queue.depth",
      values: { persisted: 1, inflight: 0 },
    });
    expect(JSON.stringify(sanitized)).not.toContain("faro-session-secret");
  });

  test("transport sanitizer removes Faro browser/runtime resource fingerprints", () => {
    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "trace",
      payload: {
        resourceSpans: [{
          resource: {
            attributes: [
              { key: "service.name", value: { stringValue: "waddle-chat" } },
              { key: "service.version", value: { stringValue: "1.2.3" } },
              { key: "telemetry.sdk.name", value: { stringValue: "opentelemetry" } },
              { key: "browser.brands", value: { stringValue: "Chromium 137 fingerprint-secret" } },
              { key: "browser.language", value: { stringValue: "private-language" } },
              { key: "browser.mobile", value: { boolValue: false } },
              { key: "browser.platform", value: { stringValue: "private-platform" } },
              { key: "user_agent.original", value: { stringValue: "full-user-agent-secret" } },
              { key: "process.runtime.name", value: { stringValue: "browser" } },
              { key: "process.runtime.version", value: { stringValue: "runtime-user-agent-secret" } },
            ],
          },
          scopeSpans: [{
            spans: [{
              traceId: "01".repeat(16),
              spanId: "02".repeat(8),
              name: "HTTP GET private-span-name-secret",
              kind: 3,
              startTimeUnixNano: "1",
              endTimeUnixNano: "2",
              status: { code: 2, message: "raw-span-error-secret" },
              attributes: [
                { key: "http.request.method", value: { stringValue: "GET" } },
                { key: "network.protocol.version", value: { stringValue: "alice@example.com/private" } },
                { key: "user_agent.original", value: { stringValue: "span-user-agent-secret" } },
                { key: "browser.platform", value: { stringValue: "span-platform-secret" } },
                { key: "process.runtime.version", value: { stringValue: "span-runtime-secret" } },
                { key: "faro.action.user.name", value: { stringValue: "send-message" } },
                { key: "faro.action.user.parentId", value: { stringValue: "span-action-parent-secret" } },
              ],
              events: [{
                timeUnixNano: "2",
                name: "private-event-name-secret",
                droppedAttributesCount: 0,
                attributes: [
                  { key: "browser.language", value: { stringValue: "event-language-secret" } },
                  { key: "exception.message", value: { stringValue: "exception-message-secret" } },
                  { key: "exception.stacktrace", value: { stringValue: "exception-stack-secret" } },
                  { key: "event.category", value: { stringValue: "network" } },
                ],
              }],
              links: [{
                traceId: "03".repeat(16),
                spanId: "04".repeat(8),
                droppedAttributesCount: 0,
                attributes: [
                  { key: "http.user_agent", value: { stringValue: "link-user-agent-secret" } },
                  { key: "link.category", value: { stringValue: "retry" } },
                ],
              }],
            }],
          }],
        }],
      },
      meta: {},
    } as never) as unknown as {
      payload: {
        resourceSpans: Array<{
          resource: { attributes: Array<{ key: string }> };
          scopeSpans: Array<{
            spans: Array<{
              status: { message: string };
              attributes: Array<{ key: string }>;
              events: Array<{ attributes: Array<{ key: string }> }>;
              links: Array<{ attributes: Array<{ key: string }> }>;
            }>;
          }>;
        }>;
      };
    };

    const resourceSpan = sanitized.payload.resourceSpans[0];
    expect(resourceSpan.resource.attributes.map(({ key }) => key)).toEqual([
      "service.name",
      "service.version",
      "telemetry.sdk.name",
    ]);
    const span = resourceSpan.scopeSpans[0].spans[0];
    expect(span.status.message).toBe("operation-error");
    expect(span.attributes.map(({ key }) => key)).toEqual(["http.request.method"]);
    expect(span.events[0].attributes.map(({ key }) => key)).toEqual(["event.category"]);
    expect(span.links[0].attributes.map(({ key }) => key)).toEqual(["link.category"]);
    expect(JSON.stringify(sanitized)).not.toContain("secret");
    expect(JSON.stringify(sanitized)).not.toContain("alice@example.com");
  });

  test("transport sanitizer canonicalizes duplicate trace attributes before wire reconstruction", () => {
    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "trace",
      payload: {
        resourceSpans: [{
          resource: {
            attributes: [
              { key: "service.name", value: { stringValue: "waddle-chat" } },
              { key: "service.name", value: { stringValue: "private-service" } },
            ],
          },
          scopeSpans: [{
            spans: [{
              traceId: "01".repeat(16),
              spanId: "02".repeat(8),
              name: "HTTP GET",
              kind: 3,
              startTimeUnixNano: "1",
              endTimeUnixNano: "2",
              attributes: [
                { key: "http.host", value: { stringValue: "trusted.example" } },
                { key: "http.host", value: { stringValue: "private-host.internal" } },
              ],
              events: [{
                timeUnixNano: "2",
                name: "event",
                attributes: [
                  { key: "event.category", value: { stringValue: "network" } },
                  { key: "event.category", value: { stringValue: "network" } },
                ],
              }],
              links: [{
                traceId: "03".repeat(16),
                spanId: "04".repeat(8),
                attributes: [
                  { key: "link.category", value: { stringValue: "retry" } },
                  { key: "link.category", value: { stringValue: "retry" } },
                ],
              }],
            }],
          }],
        }],
      },
      meta: {},
    } as never) as unknown as {
      payload: {
        resourceSpans: Array<{
          resource: { attributes: Array<{ key: string; value: { stringValue: string } }> };
          scopeSpans: Array<{
            spans: Array<{
              attributes: Array<{ key: string; value: { stringValue: string } }>;
              events: Array<{ attributes: Array<{ key: string }> }>;
              links: Array<{ attributes: Array<{ key: string }> }>;
            }>;
          }>;
        }>;
      };
    };

    const resourceSpan = sanitized.payload.resourceSpans[0];
    expect(resourceSpan.resource.attributes).toEqual([
      { key: "service.name", value: { stringValue: "waddle-chat" } },
    ]);
    const span = resourceSpan.scopeSpans[0].spans[0];
    expect(span.attributes).toEqual([
      { key: "http.host", value: { stringValue: ":redacted" } },
    ]);
    expect(span.events[0].attributes).toHaveLength(1);
    expect(span.links[0].attributes).toHaveLength(1);
    expect(JSON.stringify(sanitized)).not.toContain("private-service");
    expect(JSON.stringify(sanitized)).not.toContain("private-host.internal");
  });

  test("transport sanitizer fails closed on malformed trace containers", () => {
    expect(__sanitizeFaroTransportItemForTesting({
      type: "trace",
      payload: { resourceSpans: { private: "raw-trace-secret" } },
      meta: {},
    } as never)).toBeNull();

    expect(__sanitizeFaroTransportItemForTesting({
      type: "trace",
      payload: { resourceSpans: [null, { scopeSpans: { private: "raw-trace-secret" } }] },
      meta: {},
    } as never)).toBeNull();
  });

  test("transport sanitizer drops unknown synthetic events and user-action envelopes", () => {
    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "event",
      payload: {
        name: "faro.tracing.fetch",
        attributes: {
          "faro.action.user.name": "send-message",
          "faro.action.user.parentId": "flat-parent-secret",
          "http.request.method": "POST",
          "user_agent.original": "event-user-agent-secret",
        },
        action: {
          name: "send-message",
          id: "action-id-secret",
          parentId: "action-parent-secret",
        },
      },
      meta: {},
    } as never);

    expect(sanitized).toBeNull();
  });

  test("transport sanitizer drops free-text logs and bounds exception payloads", () => {
    expect(__sanitizeFaroTransportItemForTesting({
      type: "log",
      payload: { message: "private-log-secret", level: "error" },
      meta: {},
    } as never)).toBeNull();

    const sanitized = __sanitizeFaroTransportItemForTesting({
      type: "exception",
      payload: {
        type: "PrivateErrorType",
        value: "private-exception-secret",
        fingerprint: "private-fingerprint-secret",
        stacktrace: {
          frames: [{
            filename: "https://chat.example/private-file-secret.ts?token=secret",
            function: "privateFunctionSecret",
          }],
        },
        context: {
          recoverable: "true",
          detail: "private-context-secret",
          note: "private-note-secret",
          queueSize: "3",
        },
      },
      meta: {},
    } as never) as unknown as {
      payload: Record<string, unknown> & { context: Record<string, string> };
    };

    expect(sanitized.payload).toEqual(expect.objectContaining({
      type: "window-error",
      value: "window-error",
      context: {
        kind: "window-error",
        recoverable: "true",
        queueSize: "3",
      },
    }));
    expect(sanitized.payload.stacktrace).toBeUndefined();
    expect(sanitized.payload.fingerprint).toBeUndefined();
    expect(JSON.stringify(sanitized)).not.toContain("secret");
  });

});
