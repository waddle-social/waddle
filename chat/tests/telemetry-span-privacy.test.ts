import { afterEach, describe, expect, test } from "bun:test";
import {
  __scrubMissingFetchSpanUrlForTesting,
  __scrubSpanUrlForTesting,
  __scrubXhrSpanUrlForTesting,
  __sanitizeFaroTransportItemForTesting,
  markSensitiveUrlForTelemetry,
} from "../src/lib/telemetry";

const originalWindow = globalThis.window;

afterEach(() => {
  if (originalWindow === undefined) {
    Reflect.deleteProperty(globalThis, "window");
  } else {
    (globalThis as typeof globalThis & { window: Window & typeof globalThis }).window =
      originalWindow;
  }
});

describe("telemetry span privacy boundary", () => {
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

    expect(__scrubSpanUrlForTesting(
      "https://chat.example/_astro/private-session-token.js?download=1",
      ["https://chat.example"],
    )).toBe("https://chat.example/_astro/:asset");

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
              traceId: "01".repeat(16),
              spanId: "02".repeat(8),
              name: "HTTP GET",
              kind: 3,
              startTimeUnixNano: "1",
              endTimeUnixNano: "2",
              status: { code: 1 },
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
      "http.target": ":unknown",
      "http.url": "external:unknown",
      "server.address": ":redacted",
      "server.port": 0,
      "url.full": "external:unknown",
      "url.path": ":unknown",
      "url.query": ":redacted",
    });
  });

  test("transport sanitizer templates untrusted same-origin Astro asset suffixes", () => {
    const currentUrl = new URL("https://chat.example/");
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
              traceId: "01".repeat(16),
              spanId: "02".repeat(8),
              name: "HTTP GET",
              startTimeUnixNano: "1",
              endTimeUnixNano: "2",
              attributes: [{
                key: "http.url",
                value: {
                  stringValue: "https://chat.example/_astro/private-session-token.js",
                },
              }],
            }],
          }],
        }],
      },
      meta: {},
    } as never);

    expect(JSON.stringify(sanitized)).not.toContain("private-session-token");
    expect(JSON.stringify(sanitized)).toContain("https://chat.example/_astro/:asset");
  });

  test("transport sanitizer rejects synthetic Faro trace events outside the typed event catalog", () => {
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
    } as never);

    expect(sanitized).toBeNull();
  });
});
