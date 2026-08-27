/**
 * Grafana Faro RUM wrapper.
 *
 * All reporting functions are no-ops when Faro isn't initialized.
 * Initialization is gated on the runtime presence of a Faro collector
 * URL — there is no "disabled" mode in the SDK itself; we just skip
 * `initializeFaro()` entirely so no beacons leave the page.
 *
 * Drop-detection signals emitted here all live under the
 * `chat.xmpp.*` event namespace so they're easy to filter in Grafana.
 * The server side ships the same information via the
 * `waddle_broadcast_*_total` and `waddle_sm_unacked_evicted_total`
 * counters scraped by the in-cluster Alloy collector, so Grafana
 * dashboards can cross-reference both sides of every drop.
 *
 * Tracing: when a server base URL is supplied to `initTelemetry`, the
 * `TracingInstrumentation` from `@grafana/faro-web-tracing` auto-wraps
 * `fetch` / `XMLHttpRequest` in OpenTelemetry spans AND injects W3C
 * `traceparent` / `tracestate` headers on outbound calls to that
 * origin. The Rust server's tower-http trace layer extracts those
 * headers (see `waddle-server`'s `telemetry.rs`), so frontend spans
 * become parents of backend spans in Tempo for every HTTP call.
 *
 * `withSpan` below is for work that isn't a fetch — e.g. the XMPP
 * connect handshake over WebSocket, outbound message bookkeeping —
 * so we still get client-side timing and failure attribution. Browsers
 * cannot attach headers to a WebSocket upgrade, and XMPP transports no
 * tracing context in its URL; any `fetch` issued inside a `withSpan`
 * callback is propagated through normal HTTP headers.
 */
import {
  initializeFaro,
  getWebInstrumentations,
  type Faro,
  type MetaAttributes,
  type MetaPage,
  type TransportItem,
} from "@grafana/faro-web-sdk";
import { getDefaultOTELInstrumentations, TracingInstrumentation } from "@grafana/faro-web-tracing";
import { context, SpanStatusCode, trace, type Span } from "@opentelemetry/api";
import {
  encodeTelemetryError,
  encodeTelemetrySpan,
  type TelemetryErrorContext,
  type TelemetryErrorObservation,
  type TelemetrySpanObservation,
} from "./telemetry-observations";
import {
  callAudioProcessingEventAttributes,
  type VerifiedCallAudioProcessing,
} from "./calls/call-audio-processing-telemetry";
import {
  callMediaPathEventAttributes,
  type CallMediaPathSnapshot,
} from "./calls/call-media-path-telemetry";
import {
  callIceEventAttributes,
  type IceCredentialEvent,
  type CallIceSnapshot,
} from "./calls/call-ice-telemetry";
import type {
  CallKind,
  CallLifecyclePayload,
} from "./calls/call-lifecycle-telemetry";
import type { XmppStatusSnapshot } from "./xmpp/types";

type MessageKind = "room" | "dm";
type DisplayedMarkerLatencyBand = "unknown" | "under-250ms" | "250ms-1s" | "1s-5s" | "over-5s";
type StreamManagementTelemetryPayload =
  | { kind: "ack-requested"; reason: "outbound-stanza" | "resumed-unacked-tail" | "peer-request" | "pagehide" }
  | { kind: "ack-validated"; progress: boolean }
  | { kind: "ack-retry"; attempt: number }
  | { kind: "ack-request-timed-out" }
  | { kind: "progress-timed-out" }
  | { kind: "failed" }
  | { kind: "lifecycle-failed"; operation: "prepare-xmpp" | "resume-xmpp" | "suspend-call" };

/** Errors we classify coarsely so Tempo filters stay useful. */
export type ErrorKind = TelemetryErrorObservation["kind"];

interface InitTelemetryOptions {
  /** Faro collector URL (from Grafana Cloud Faro app config). */
  url: string;
  /** App name, typically the env-specific identifier e.g. `waddle-chat`. */
  appName: string;
  /** Semantic application version shown in Faro application metadata. */
  appVersion?: string;
  /** Deployment environment shown in Faro application metadata. */
  environment?: string;
  /** Commit SHA used as the Faro release, for source/deploy correlation. */
  release?: string;
  /**
   * Cross-origin URLs where the browser should inject W3C trace
   * context headers. Usually just the `waddle-server` origin. Passing
   * this — or leaving it empty — is what decides whether the frontend
   * actually shows up as the parent span in backend traces.
   */
  propagateTraceHeadersTo?: string[];
}

let faro: Faro | null = null;
let spanFactoryForTesting: ((name: string, attributes: Record<string, string>) => Span) | null = null;
const TELEMETRY_EXCEPTION_RECORDED = Symbol("waddle.telemetry.exception-recorded");
let configuredTrustedSpanOrigins = new Set<string>();
const sensitiveSpanUrls = new Set<string>();

const TRACER_NAME = "waddle-chat";
const UNKNOWN_EXTERNAL_URL = "external:unknown";
const UNKNOWN_FETCH_URL = "fetch:unknown";
const UNKNOWN_FILE_TRANSFER_URL = "file-transfer:unknown";
const UNKNOWN_XHR_URL = "xhr:unknown";
const REDACTED_SPAN_HOST = ":redacted";
const REDACTED_SPAN_PATH = ":unknown";
const REDACTED_SPAN_PORT = 0;
const FARO_CSP_INSTRUMENTATION = "@grafana/faro-web-sdk:instrumentation-csp";
const FARO_GLOBAL_ERROR_INSTRUMENTATION = "@grafana/faro-web-sdk:instrumentation-errors";
const FARO_NAVIGATION_INSTRUMENTATION = "@grafana/faro-web-sdk:instrumentation-navigation";
const FARO_TRACE_EVENT_PREFIX = "faro.tracing.";
const SPAN_URL_ATTRIBUTE_KEYS = ["http.url", "url.full"] as const;
const SPAN_ENDPOINT_ATTRIBUTE_KEYS = [
  "http.host",
  "http.target",
  "server.address",
  "server.port",
  "url.path",
] as const;
const SPAN_QUERY_ATTRIBUTE_KEYS = ["url.fragment", "url.query"] as const;
const SPAN_REDACTED_ATTRIBUTE_KEYS = ["http.response.status_text", "http.status_text"] as const;
const SENSITIVE_TRACE_URL_PATTERNS = [
  /[?&]session_id=/,
  /[?&]api_key=/,
  /\/api\/upload(?:[/?#]|$)/,
  /\/api\/files(?:[/?#]|$)/,
  /^https:\/\/api\.giphy\.com\//,
];
const SENSITIVE_ERROR_CONTEXT_KEYS = new Set([
  "accountkey",
  "barejid",
  "fulljid",
  "jid",
  "key",
  "storagekey",
]);
const XMPP_RESOURCE_PATTERN = /web-[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}/gi;

type SpanAttributeValue = string | number;
type FaroEventPayload = {
  attributes?: Record<string, string>;
  name?: string;
};
type OtelAnyValue = {
  stringValue?: string | null;
  boolValue?: boolean | null;
  intValue?: number | null;
  doubleValue?: number | null;
};
type OtelKeyValue = {
  key: string;
  value: OtelAnyValue;
};
type OtelSpan = {
  attributes?: OtelKeyValue[];
  events?: Array<{
    attributes?: OtelKeyValue[];
  }>;
  links?: Array<{
    attributes?: OtelKeyValue[];
  }>;
};
type FaroTracePayload = {
  resourceSpans?: Array<{
    resource?: {
      attributes?: OtelKeyValue[];
    };
    scopeSpans?: Array<{
      scope?: {
        attributes?: OtelKeyValue[];
      };
      spans?: OtelSpan[];
    }>;
  }>;
};

/**
 * Initialize Faro exactly once per page lifetime. Re-invocation is a
 * no-op — the module guards on `faro` being non-null.
 *
 * Missing `url` silently skips init. That's the shape callers rely on:
 * `initTelemetry({ url: import.meta.env.PUBLIC_FARO_URL, ... })` can be
 * fired unconditionally and does nothing when env vars are unset.
 */
export function initTelemetry(options: InitTelemetryOptions): void {
  if (faro) return;
  if (!options.url) return;

  try {
    const propagateUrls = (options.propagateTraceHeadersTo ?? [])
      .filter((entry) => entry && entry.trim().length > 0)
      .map((entry) => normalizeUrlPrefixEntry(entry));
    const ignoredTraceUrls = [
      ...SENSITIVE_TRACE_URL_PATTERNS,
      normalizeUrlPrefixEntry(options.url),
    ];
    const trustedSpanOrigins = trustedSpanUrlOrigins(options.propagateTraceHeadersTo ?? []);
    configuredTrustedSpanOrigins = trustedSpanOrigins;

    faro = initializeFaro({
      url: options.url,
      app: {
        name: options.appName || "waddle-chat",
        version: options.appVersion || "unknown",
        environment: options.environment || undefined,
        release: options.release,
      },
      pageTracking: {
        page: sanitizedPageMeta(),
        generatePageId: sanitizePagePathForTelemetry,
      },
      beforeSend: sanitizeFaroTransportItem,
      instrumentations: [
        // Default browser instrumentations, minus global error capture:
        // explicit app errors go through `reportError()` so only canonical
        // closed observations leave the page. Console and
        // resource performance capture stay disabled for the same reason.
        ...getPrivacySafeWebInstrumentations(),
        // Wraps fetch + XMLHttpRequest in OTel spans and injects
        // traceparent/tracestate on requests whose URL matches one of
        // `propagateTraceHeaderCorsUrls`. Without a matching entry the
        // browser does NOT send those headers cross-origin, so the
        // backend can't join the trace.
        new TracingInstrumentation({
          instrumentations: getDefaultOTELInstrumentations({
            ignoreUrls: ignoredTraceUrls,
            propagateTraceHeaderCorsUrls: propagateUrls,
            fetchInstrumentationOptions: {
              applyCustomAttributesOnSpan: (span, request, result) =>
                scrubFetchSpanUrl(span, request, result, trustedSpanOrigins),
            },
            xhrInstrumentationOptions: {
              applyCustomAttributesOnSpan: (span, xhr) =>
                scrubXhrSpanUrl(span, xhr, trustedSpanOrigins),
            },
          }),
        }),
      ],
    });
  } catch (err) {
    // Faro itself throwing here is already a telemetry bug; log to the
    // console so it surfaces in devtools but never propagate — chat
    // must continue to work with or without telemetry.
    console.error("Faro initialization failed", err);
    faro = null;
  }
  installClientHealthTelemetry();
  installGlobalErrorTelemetry();
}

// --- Global (window-level) error capture -----------------------------
//
// Faro's stock errors instrumentation is deliberately stripped in
// `getPrivacySafeWebInstrumentations()` because it ships raw messages
// and stacks. These handlers restore global capture but project every
// failure to a canonical closed observation instead.

const GLOBAL_ERROR_DEDUPE_WINDOW_MS = 5_000;
const BENIGN_RESIZE_OBSERVER_ERROR_MESSAGES = new Set([
  "ResizeObserver loop completed with undelivered notifications.",
  "ResizeObserver loop limit exceeded",
]);
// Guard is per-window (not a process-lifetime latch) so a swapped
// `window` — a fresh test stub, an HMR-recreated document — gets its
// own listeners instead of a silent no-op.
let globalErrorsInstalledOn: unknown = null;
let reportingGlobalError = false;
let telemetrySinkActive = false;
let lastGlobalErrorKey = "";
let lastGlobalErrorAtMs = 0;

/**
 * Install window `error` + `unhandledrejection` capture. Idempotent
 * per window instance and a no-op outside the browser. Called once
 * from {@link initTelemetry}; `reportError` already no-ops without
 * Faro, so installing before or without a collector is harmless.
 */
export function installGlobalErrorTelemetry(): void {
  if (typeof window === "undefined" || typeof window.addEventListener !== "function") return;
  if (globalErrorsInstalledOn === window) return;
  globalErrorsInstalledOn = window;

  window.addEventListener("error", (event) => {
    handleWindowErrorEvent(event);
  });
  window.addEventListener("unhandledrejection", (event) => {
    handleUnhandledRejectionEvent(event);
  });
}

/** Exported for tests and for {@link installGlobalErrorTelemetry}. */
export function handleWindowErrorEvent(event: { error?: unknown; message?: unknown }): void {
  const error = event.error
    ?? new Error(typeof event.message === "string" && event.message ? event.message : "window-error");
  if (isBenignResizeObserverError(error)) return;
  reportGlobalError("window-error", error);
}

function isBenignResizeObserverError(error: unknown): boolean {
  // Same normalization as reportGlobalError: browsers may surface the
  // window-error payload as a bare string rather than an Error.
  const message = error instanceof Error ? error.message : String(error);
  return BENIGN_RESIZE_OBSERVER_ERROR_MESSAGES.has(message);
}

/** Exported for tests and for {@link installGlobalErrorTelemetry}. */
export function handleUnhandledRejectionEvent(event: { reason?: unknown }): void {
  reportGlobalError("unhandled-rejection", event.reason ?? new Error("unhandled-rejection"));
}

/**
 * Report a Vue render/lifecycle error caught by `app.config.errorHandler`
 * (installed in `src/vue-app.ts`). Shares the loop guard + flood dedupe
 * with the window-level handlers.
 */
export function reportVueError(): void {
  reportGlobalError("vue-render-error", undefined);
}

function reportGlobalError(
  kind: "window-error" | "unhandled-rejection" | "vue-render-error",
  error: unknown,
): void {
  // Loop guard: an error thrown while reporting (inside Faro or our
  // sanitizers) would re-enter via the same global handlers forever.
  if (reportingGlobalError) return;
  reportingGlobalError = true;
  try {
    const message = error instanceof Error ? error.message : String(error);
    const key = `${kind}:${message}`;
    const now = Date.now();
    if (key === lastGlobalErrorKey && now - lastGlobalErrorAtMs < GLOBAL_ERROR_DEDUPE_WINDOW_MS) {
      return;
    }
    lastGlobalErrorKey = key;
    lastGlobalErrorAtMs = now;
    reportError({ kind, reason: "unexpected" });
  } catch {
    // Never let telemetry take the page down (or recurse).
  } finally {
    reportingGlobalError = false;
  }
}

function sanitizeFaroTransportItem(item: TransportItem): TransportItem {
  const sanitized = {
    ...item,
    meta: {
      ...item.meta,
      page: sanitizedPageMeta(item.meta.page),
    },
  };
  sanitizeTransportPayload(sanitized);
  return sanitized;
}

function sanitizeTransportPayload(item: TransportItem): void {
  if (item.type === "trace") {
    sanitizeTracePayload(item.payload, trustedSpanOriginsForTransport());
    return;
  }

  if (item.type === "event") {
    sanitizeEventPayload(item.payload, trustedSpanOriginsForTransport());
  }
}

function sanitizeEventPayload(payload: unknown, trustedOrigins: Set<string>): void {
  if (!isRecord(payload)) return;
  const event = payload as FaroEventPayload;
  const attributes = event.attributes;
  if (!isStringRecord(attributes)) return;

  if (event.name?.startsWith(FARO_TRACE_EVENT_PREFIX)) {
    sanitizeFlatSpanAttributes(attributes, trustedOrigins);
  }

  for (const [key, value] of Object.entries(attributes)) {
    if (isSensitiveContextKey(key)) {
      delete attributes[key];
      continue;
    }
    attributes[key] = sanitizeTelemetryText(value);
  }
}

function sanitizeTracePayload(payload: unknown, trustedOrigins: Set<string>): void {
  if (!isRecord(payload)) return;
  const tracePayload = payload as FaroTracePayload;
  for (const resourceSpan of tracePayload.resourceSpans ?? []) {
    sanitizeOtelAttributeStrings(resourceSpan.resource?.attributes);
    for (const scopeSpan of resourceSpan.scopeSpans ?? []) {
      sanitizeOtelAttributeStrings(scopeSpan.scope?.attributes);
      for (const span of scopeSpan.spans ?? []) {
        sanitizeOtelSpanAttributes(span.attributes, trustedOrigins);
        for (const event of span.events ?? []) {
          sanitizeOtelAttributeStrings(event.attributes);
        }
        for (const link of span.links ?? []) {
          sanitizeOtelAttributeStrings(link.attributes);
        }
      }
    }
  }
}

function sanitizeOtelSpanAttributes(attributes: OtelKeyValue[] | undefined, trustedOrigins: Set<string>): void {
  if (!attributes) return;
  const url = firstOtelSpanUrl(attributes);

  if (url) {
    const scrubbed = scrubUrl(url, trustedOrigins);
    if (scrubbed) setOtelSpanUrlAttributes(attributes, scrubbed);
  } else {
    redactOtelEndpointAttributes(attributes);
  }

  for (const key of SPAN_QUERY_ATTRIBUTE_KEYS) {
    const attribute = findOtelAttribute(attributes, key);
    if (attribute) setOtelAttributeValue(attribute, ":redacted");
  }
  for (const key of SPAN_REDACTED_ATTRIBUTE_KEYS) {
    const attribute = findOtelAttribute(attributes, key);
    if (attribute) setOtelAttributeValue(attribute, ":redacted");
  }

  for (const attribute of attributes) {
    if (typeof attribute.value?.stringValue === "string") {
      setOtelAttributeValue(attribute, sanitizeTelemetryText(attribute.value.stringValue));
    }
  }
}

function sanitizeOtelAttributeStrings(attributes: OtelKeyValue[] | undefined): void {
  if (!attributes) return;
  for (const attribute of attributes) {
    if (typeof attribute.value?.stringValue === "string") {
      setOtelAttributeValue(attribute, sanitizeTelemetryText(attribute.value.stringValue));
    }
  }
}

function firstOtelSpanUrl(attributes: OtelKeyValue[]): string | undefined {
  for (const key of SPAN_URL_ATTRIBUTE_KEYS) {
    const value = readOtelStringAttribute(attributes, key);
    if (value) return value;
  }
  return undefined;
}

function setOtelSpanUrlAttributes(attributes: OtelKeyValue[], value: string): void {
  const endpoint = safeSpanEndpoint(value);
  upsertOtelAttribute(attributes, "http.url", value);
  upsertOtelAttribute(attributes, "url.full", value);
  upsertOtelAttribute(attributes, "http.host", endpoint.host);
  upsertOtelAttribute(attributes, "http.target", endpoint.path);
  upsertOtelAttribute(attributes, "server.address", endpoint.address);
  upsertOtelAttribute(attributes, "server.port", endpoint.port);
  upsertOtelAttribute(attributes, "url.path", endpoint.path);
}

function redactOtelEndpointAttributes(attributes: OtelKeyValue[]): void {
  const endpoint = redactedSpanEndpoint();
  const replacements: Record<typeof SPAN_ENDPOINT_ATTRIBUTE_KEYS[number], SpanAttributeValue> = {
    "http.host": endpoint.host,
    "http.target": endpoint.path,
    "server.address": endpoint.address,
    "server.port": endpoint.port,
    "url.path": endpoint.path,
  };

  for (const key of SPAN_ENDPOINT_ATTRIBUTE_KEYS) {
    const attribute = findOtelAttribute(attributes, key);
    if (attribute) setOtelAttributeValue(attribute, replacements[key]);
  }
}

function readOtelStringAttribute(attributes: OtelKeyValue[], key: string): string | undefined {
  const value = findOtelAttribute(attributes, key)?.value?.stringValue;
  return typeof value === "string" ? value : undefined;
}

function findOtelAttribute(attributes: OtelKeyValue[], key: string): OtelKeyValue | undefined {
  return attributes.find((attribute) => attribute.key === key);
}

function upsertOtelAttribute(attributes: OtelKeyValue[], key: string, value: SpanAttributeValue): void {
  const existing = findOtelAttribute(attributes, key);
  if (existing) {
    setOtelAttributeValue(existing, value);
    return;
  }
  attributes.push({
    key,
    value: otelAttributeValue(value),
  });
}

function setOtelAttributeValue(attribute: OtelKeyValue, value: SpanAttributeValue): void {
  attribute.value = otelAttributeValue(value);
}

function otelAttributeValue(value: SpanAttributeValue): OtelAnyValue {
  return typeof value === "number" ? { intValue: value } : { stringValue: value };
}

function sanitizeFlatSpanAttributes(attributes: Record<string, string>, trustedOrigins: Set<string>): void {
  const url = firstFlatSpanUrl(attributes);

  if (url) {
    const scrubbed = scrubUrl(url, trustedOrigins);
    if (scrubbed) setFlatSpanUrlAttributes(attributes, scrubbed);
  } else {
    redactFlatEndpointAttributes(attributes);
  }

  for (const key of SPAN_QUERY_ATTRIBUTE_KEYS) {
    if (key in attributes) attributes[key] = ":redacted";
  }
  for (const key of SPAN_REDACTED_ATTRIBUTE_KEYS) {
    if (key in attributes) attributes[key] = ":redacted";
  }
}

function firstFlatSpanUrl(attributes: Record<string, string>): string | undefined {
  for (const key of SPAN_URL_ATTRIBUTE_KEYS) {
    const value = attributes[key];
    if (value) return value;
  }
  return undefined;
}

function setFlatSpanUrlAttributes(attributes: Record<string, string>, value: string): void {
  const endpoint = safeSpanEndpoint(value);
  attributes["http.url"] = value;
  attributes["url.full"] = value;
  attributes["http.host"] = endpoint.host;
  attributes["http.target"] = endpoint.path;
  attributes["server.address"] = endpoint.address;
  attributes["server.port"] = String(endpoint.port);
  attributes["url.path"] = endpoint.path;
}

function redactFlatEndpointAttributes(attributes: Record<string, string>): void {
  const endpoint = redactedSpanEndpoint();
  const replacements: Record<typeof SPAN_ENDPOINT_ATTRIBUTE_KEYS[number], string> = {
    "http.host": endpoint.host,
    "http.target": endpoint.path,
    "server.address": endpoint.address,
    "server.port": String(endpoint.port),
    "url.path": endpoint.path,
  };

  for (const key of SPAN_ENDPOINT_ATTRIBUTE_KEYS) {
    if (key in attributes) attributes[key] = replacements[key];
  }
}

function trustedSpanOriginsForTransport(): Set<string> {
  const origins = new Set(configuredTrustedSpanOrigins);
  const pageOrigin = currentPageOrigin();
  if (pageOrigin) origins.add(pageOrigin);
  return origins;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isStringRecord(value: unknown): value is Record<string, string> {
  return isRecord(value) && Object.values(value).every((entry) => typeof entry === "string");
}

function sanitizedPageMeta(page?: MetaPage): MetaPage {
  const route = sanitizePagePathForTelemetry();
  return {
    ...page,
    attributes: sanitizeMetaAttributes(page?.attributes),
    id: route,
    url: `${currentPageOrigin()}${route}`,
  };
}

function sanitizeMetaAttributes(attributes: MetaAttributes | undefined): MetaAttributes | undefined {
  if (!attributes) return undefined;
  return Object.fromEntries(
    Object.entries(attributes).map(([key, value]) => [key, sanitizeTelemetryText(value)]),
  );
}

function sanitizePagePathForTelemetry(locationOverride?: Location): string {
  return sanitizeRoutePath(currentPagePath(locationOverride));
}

function sanitizeRoutePath(path: string): string {
  const segments = path.split("/").filter(Boolean);
  if (segments.length === 0) return "/";

  switch (segments[0]) {
    case "admin":
      return segments.length > 1 ? "/admin/:panel" : "/admin";
    case "dm":
      return segments.length > 1 ? "/dm/:user" : "/dm";
    case "events":
    case "feed":
    case "settings":
    case "stories":
    case "threads":
      return `/${segments[0]}`;
    case "r":
      if (segments[2] === "x") return "/r/:room/x/:plugin/:route";
      return segments.length > 1 ? "/r/:room" : "/r";
    default:
      return "/:route";
  }
}

function currentPagePath(locationOverride?: Location): string {
  const pageLocation = locationOverride ?? (typeof window !== "undefined" ? window.location : undefined);
  const pathname = pageLocation?.pathname || "/";
  return pathname.startsWith("/") ? pathname : `/${pathname}`;
}

function currentPageOrigin(): string {
  if (typeof window === "undefined") return "";
  if (window.location?.origin) return window.location.origin;
  try {
    return new URL(window.location?.href ?? "").origin;
  } catch {
    return "";
  }
}

function getPrivacySafeWebInstrumentations() {
  return getWebInstrumentations({
    captureConsole: false,
    enableContentSecurityPolicyInstrumentation: false,
    enablePerformanceInstrumentation: false,
  }).filter((instrumentation) =>
    instrumentation.name !== FARO_CSP_INSTRUMENTATION
    && instrumentation.name !== FARO_GLOBAL_ERROR_INSTRUMENTATION
    && instrumentation.name !== FARO_NAVIGATION_INSTRUMENTATION
  );
}

/**
 * Turn a string into the URL-prefix RegExp `TracingInstrumentation`
 * expects. A plain string match is exact, so `https://xmpp.waddle.social`
 * would not match `/api/...`; the escaped prefix regex does.
 */
function normalizeUrlPrefixEntry(entry: string): RegExp {
  const trimmed = entry.trim();
  const prefix = trimmed.endsWith("/") ? trimmed.slice(0, -1) : trimmed;
  return new RegExp(`^${escapeRegExp(prefix)}(?:[/?#]|$)`);
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function trustedSpanUrlOrigins(entries: string[]): Set<string> {
  const origins = new Set<string>();
  const pageOrigin = currentPageOrigin();
  if (pageOrigin) origins.add(pageOrigin);

  for (const entry of entries) {
    try {
      origins.add(new URL(entry, currentPageHref()).origin);
    } catch {
      // Invalid config entry: leave it out rather than widening trust.
    }
  }
  return origins;
}

function scrubFetchSpanUrl(
  span: Span,
  request: unknown,
  result: unknown,
  trustedOrigins: Set<string>,
): void {
  const url = readResultUrl(result) ?? readRequestUrl(request);
  if (!url) {
    setSpanUrlAttributes(span, UNKNOWN_FETCH_URL);
    return;
  }
  scrubSpanUrlAttributes(span, url, trustedOrigins);
}

function scrubXhrSpanUrl(span: Span, xhr: XMLHttpRequest, trustedOrigins: Set<string>): void {
  if (!xhr.responseURL) {
    setSpanUrlAttributes(span, UNKNOWN_XHR_URL);
    return;
  }
  scrubSpanUrlAttributes(span, xhr.responseURL, trustedOrigins);
}

function readResultUrl(result: unknown): string | undefined {
  if (result && typeof result === "object" && "url" in result) {
    const url = (result as { url?: unknown }).url;
    return typeof url === "string" ? url : undefined;
  }
  return undefined;
}

function readRequestUrl(request: unknown): string | undefined {
  if (typeof Request !== "undefined" && request instanceof Request) {
    return request.url;
  }
  if (typeof request === "string") return request;
  if (typeof URL !== "undefined" && request instanceof URL) return request.toString();
  return undefined;
}

function scrubSpanUrlAttributes(span: Span, url: string | undefined, trustedOrigins: Set<string>): void {
  const scrubbed = scrubUrl(url, trustedOrigins);
  if (!scrubbed) return;
  setSpanUrlAttributes(span, scrubbed);
}

function setSpanUrlAttributes(span: Span, value: string): void {
  const endpoint = safeSpanEndpoint(value);
  span.setAttribute("http.url", value);
  span.setAttribute("url.full", value);
  span.setAttribute("http.host", endpoint.host);
  span.setAttribute("http.target", endpoint.path);
  span.setAttribute("server.address", endpoint.address);
  span.setAttribute("server.port", endpoint.port);
  span.setAttribute("url.path", endpoint.path);
}

function safeSpanEndpoint(value: string): {
  address: string;
  host: string;
  path: string;
  port: number;
} {
  try {
    const url = new URL(value);
    if (url.protocol !== "http:" && url.protocol !== "https:") return redactedSpanEndpoint();
    return {
      address: url.hostname,
      host: url.host,
      path: url.pathname || "/",
      port: url.port ? Number(url.port) : (url.protocol === "https:" ? 443 : 80),
    };
  } catch {
    return redactedSpanEndpoint();
  }
}

function redactedSpanEndpoint(): {
  address: string;
  host: string;
  path: string;
  port: number;
} {
  return {
    address: REDACTED_SPAN_HOST,
    host: REDACTED_SPAN_HOST,
    path: REDACTED_SPAN_PATH,
    port: REDACTED_SPAN_PORT,
  };
}

function scrubUrl(value: string | undefined, trustedOrigins: Set<string>): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value, currentPageHref());
    if (isSensitiveSpanUrl(url)) return UNKNOWN_FILE_TRANSFER_URL;
    if (!isTrustedSpanOrigin(url, trustedOrigins)) return scrubExternalUrl();
    url.pathname = scrubUrlPath(url.pathname);
    url.search = "";
    url.hash = "";
    return url.toString();
  } catch {
    return scrubUrlPath(value.split(/[?#]/, 1)[0] ?? "");
  }
}

function isTrustedSpanOrigin(url: URL, trustedOrigins: Set<string>): boolean {
  if (trustedOrigins.has(url.origin)) return true;
  if (url.protocol !== "ws:" && url.protocol !== "wss:") return false;
  const httpEquivalent = new URL(url.origin);
  httpEquivalent.protocol = url.protocol === "wss:" ? "https:" : "http:";
  return trustedOrigins.has(httpEquivalent.origin);
}

function currentPageHref(): string {
  if (typeof window === "undefined") return "http://localhost";
  return window.location?.href || "http://localhost";
}

function scrubExternalUrl(): string {
  return UNKNOWN_EXTERNAL_URL;
}

function scrubUrlPath(path: string): string {
  if (path.startsWith("/api/upload/")) return "/api/upload/:slot";
  if (path === "/api/upload") return path;
  if (path.startsWith("/api/files/")) return "/api/files/:slot/:file";
  if (path === "/api/files") return path;
  if (path.startsWith("/api/")) return "/api/:endpoint";
  if (isStaticAssetPath(path)) return path;
  return sanitizeRoutePath(path);
}

function isStaticAssetPath(path: string): boolean {
  return path.startsWith("/_astro/")
    || path === "/favicon.ico"
    || path === "/manifest.webmanifest"
    || path === "/waddle-logo.svg"
    || /^\/(?:apple-touch-icon|favicon-\d+x\d+)\.png$/.test(path);
}

function isSensitiveSpanUrl(url: URL): boolean {
  return sensitiveSpanUrls.has(normalizeSensitiveSpanUrl(url));
}

function normalizeSensitiveSpanUrl(url: URL): string {
  const copy = new URL(url);
  copy.search = "";
  copy.hash = "";
  return copy.toString();
}

export function markSensitiveUrlForTelemetry(value: string): void {
  try {
    sensitiveSpanUrls.add(normalizeSensitiveSpanUrl(new URL(value, currentPageHref())));
  } catch {
    // Invalid URLs never make it to fetch/XHR either; ignore.
  }
}

/** For tests only — clear dynamically marked sensitive URLs. */
export function __clearSensitiveUrlsForTesting(): void {
  sensitiveSpanUrls.clear();
}

/** For tests only — inject a stub or clear state between test cases. */
export function __setFaroForTesting(instance: Faro | null): void {
  faro = instance;
  lastQueueDepth.clear();
  if (!instance) {
    configuredTrustedSpanOrigins = new Set();
    lastGlobalErrorKey = "";
    lastGlobalErrorAtMs = 0;
  }
}

/** For tests only — inject manual span setup without installing a global OTel provider. */
export function __setSpanFactoryForTesting(
  factory: ((name: string, attributes: Record<string, string>) => Span) | null,
): void {
  spanFactoryForTesting = factory;
}

/** For tests only — exercise the final transport guard without calling Grafana. */
export function __sanitizeFaroTransportItemForTesting(item: TransportItem): TransportItem {
  return sanitizeFaroTransportItem(item);
}

/** For tests only — exercise span URL redaction without real OTel spans. */
export function __scrubSpanUrlForTesting(value: string, trustedOrigins: string[]): string | undefined {
  return scrubUrl(value, new Set(trustedOrigins));
}

/** For tests only — exercise XHR span URL redaction without real XHR instrumentation. */
export function __scrubXhrSpanUrlForTesting(responseURL: string, trustedOrigins: string[]): Record<string, string | number> {
  const attributes: Record<string, string | number> = {};
  const span = {
    setAttribute: (key: string, value: string | number) => {
      attributes[key] = value;
      return span;
    },
  } as unknown as Span;
  scrubXhrSpanUrl(span, { responseURL } as XMLHttpRequest, new Set(trustedOrigins));
  return attributes;
}

/** For tests only — exercise fetch span URL fallback without real fetch instrumentation. */
export function __scrubMissingFetchSpanUrlForTesting(): Record<string, string | number> {
  const attributes: Record<string, string | number> = {};
  const span = {
    setAttribute: (key: string, value: string | number) => {
      attributes[key] = value;
      return span;
    },
  } as unknown as Span;
  scrubFetchSpanUrl(span, undefined, undefined, new Set());
  return attributes;
}

/**
 * Run `fn` inside a manually-started OpenTelemetry span. Use this for
 * operations that aren't `fetch` / `XHR` (so Faro's automatic span
 * wrapping doesn't cover them) — XMPP connect, message send, room
 * join. When Faro isn't initialized, `fn` runs with no span overhead.
 *
 * The span is also made the *active context* for the duration of
 * `fn`, so any nested auto-instrumented work (a `fetch` called inside
 * the callback, a further `withSpan`) attaches to this span instead
 * of whatever root context was active outside. The backend therefore
 * sees the manual span as the parent of the cross-origin HTTP span,
 * not an orphaned sibling.
 *
 * The returned span is exposed to the callback so callers can attach
 * low-cardinality domain attributes ("message.kind", etc.) without
 * needing their own tracer handle.
 */
export async function withSpan<T>(
  observation: TelemetrySpanObservation,
  fn: (span: Span | null) => Promise<T>,
): Promise<T> {
  if (!faro) return fn(null);
  const encoded = encodeTelemetrySpan(observation);
  let span: Span;
  let activeCtx: ReturnType<typeof context.active>;
  try {
    span = spanFactoryForTesting
      ? spanFactoryForTesting(encoded.name, encoded.attributes)
      : trace.getTracer(TRACER_NAME).startSpan(encoded.name, { attributes: encoded.attributes });
    activeCtx = trace.setSpan(context.active(), span);
  } catch {
    return fn(null);
  }

  let callbackStarted = false;
  try {
    return await context.with(activeCtx, async () => {
      callbackStarted = true;
      try {
        const result = await fn(span);
        safelyUseTelemetrySink(() => span.setStatus({ code: SpanStatusCode.OK }));
        return result;
      } catch (err) {
        safelyUseTelemetrySink(() => span.setStatus({ code: SpanStatusCode.ERROR }));
        safelyUseTelemetrySink(() => recordSpanExceptionOnce(span, err, `${encoded.name}.error`));
        throw err;
      } finally {
        safelyUseTelemetrySink(() => span.end());
      }
    });
  } catch (err) {
    if (!callbackStarted) return fn(null);
    throw err;
  }
}

/**
 * Push an explicit error to Faro. Prefer this over `console.error`
 * for non-recoverable or user-visible failures — it produces both an
 * error beacon (browsable in Frontend Observability) and an exception
 * event on the currently active span, so the backend trace and the
 * frontend error are linked in Tempo.
 *
 * `recoverable=true` marks transient issues (e.g. reconnecting) that
 * shouldn't page anyone. `recoverable=false` is for terminal states
 * (session expired, stream closed fatally, storage quota exhausted).
 */
export function reportError(
  observation: TelemetryErrorObservation,
): void {
  if (!faro) return;
  const encoded = encodeTelemetryError(observation);
  safePushError(encoded.error, { type: encoded.type, context: encoded.context });
}

/**
 * Claims a thrown error whose canonical exception was emitted separately.
 * Manual spans may still report an error status, but they must not record
 * another exception event for the same failure.
 */
export function markErrorReportedToTelemetry(error: unknown): void {
  markTelemetryExceptionRecorded(error);
}

function recordSpanExceptionOnce(span: Span, error: unknown, fallbackMessage: string): void {
  if (telemetryExceptionWasRecorded(error)) return;
  markTelemetryExceptionRecorded(error);
  const canonical = new Error(fallbackMessage);
  canonical.stack = undefined;
  span.recordException(canonical);
}

function telemetryExceptionWasRecorded(error: unknown): boolean {
  return typeof error === "object"
    && error !== null
    && TELEMETRY_EXCEPTION_RECORDED in error;
}

function markTelemetryExceptionRecorded(error: unknown): void {
  let current = error;
  const visited = new Set<object>();
  while (typeof current === "object" && current !== null && !visited.has(current)) {
    visited.add(current);
    if (Object.isExtensible(current)) {
      Object.defineProperty(current, TELEMETRY_EXCEPTION_RECORDED, {
        configurable: false,
        enumerable: false,
        value: true,
        writable: false,
      });
    }
    current = current instanceof Error ? current.cause : undefined;
  }
}

/** For tests only — exercise the same exception-once gate used by `withSpan`. */
export function __recordSpanExceptionForTesting(error: unknown): number {
  let records = 0;
  const span = {
    recordException: () => {
      records += 1;
    },
  } as unknown as Span;
  recordSpanExceptionOnce(span, error, "test-span.error");
  return records;
}

function sanitizeTelemetryText(value: string): string {
  return value
    .replace(XMPP_RESOURCE_PATTERN, ":resource")
    .replace(/([#?&](?:waddle_session_id|session_id|api_key)=)[^\s&#)]+/gi, "$1:redacted")
    .replace(/\/api\/upload\/[^\s?#)]+/g, "/api/upload/:slot")
    .replace(/\/api\/files\/[^\s?#)]+(?:\/[^\s?#)]+)?/g, "/api/files/:slot/:file")
    .replace(/[A-Z0-9._%+-]+@[A-Z0-9.-]+(?:\/[^\s,;)]*)?/gi, ":jid");
}

function isSensitiveContextKey(key: string): boolean {
  const normalized = key.replace(/[-_]/g, "").toLowerCase();
  return SENSITIVE_ERROR_CONTEXT_KEYS.has(normalized) || normalized.endsWith("key");
}

function safelyUseTelemetrySink(operation: () => void): void {
  if (telemetrySinkActive) return;
  telemetrySinkActive = true;
  try {
    operation();
  } catch {
    // Telemetry is best-effort and must never affect protocol/UI behavior.
  } finally {
    telemetrySinkActive = false;
  }
}

function safePushEvent(name: string, attributes?: Record<string, string>): void {
  if (!faro) return;
  safelyUseTelemetrySink(() => faro?.api.pushEvent(name, attributes));
}

function safePushMeasurement(
  payload: { type: string; values: Record<string, number> },
  options?: { context?: Record<string, string> },
): void {
  if (!faro) return;
  safelyUseTelemetrySink(() => faro?.api.pushMeasurement(payload, options));
}

function safePushError(
  error: Error,
  options: { type: string; context: TelemetryErrorContext },
): void {
  if (!faro) return;
  safelyUseTelemetrySink(() => faro?.api.pushError(error, options));
}

export function reportMessageFailed(payload: {
  kind: MessageKind;
}): void {
  safePushEvent("chat.xmpp.message.failed", {
    kind: payload.kind,
  });
}

export function reportMessageAcked(payload: {
  kind: MessageKind;
  latencyMs: number;
}): void {
  if (!faro) return;
  safePushEvent("chat.xmpp.message.acked", {
    kind: payload.kind,
  });
  safePushMeasurement({
    type: "chat.xmpp.message.acked.latency_ms",
    values: { latency_ms: payload.latencyMs },
  }, { context: { kind: payload.kind } });
}

function displayedMarkerLatencyBand(latencyMs: number | null): DisplayedMarkerLatencyBand {
  if (latencyMs === null || !Number.isFinite(latencyMs) || latencyMs < 0) return "unknown";
  if (latencyMs < 250) return "under-250ms";
  if (latencyMs < 1_000) return "250ms-1s";
  if (latencyMs < 5_000) return "1s-5s";
  return "over-5s";
}

export function reportDisplayedMarkerFailure(payload: {
  direction: "send" | "receive";
  kind: MessageKind;
  reason: "send-failed" | "receive-processing-failed";
  roundTripMs: number | null;
}): void {
  safePushEvent("chat.xmpp.displayed_marker.failed", {
    direction: payload.direction,
    kind: payload.kind,
    reason: payload.reason,
    round_trip_latency_band: displayedMarkerLatencyBand(payload.roundTripMs),
  });
}

export function reportSendEnqueued(payload: {
  kind: MessageKind;
  reason: "offline" | "disposed" | "destroying" | "no-client" | "reconnecting" | "not-ready";
}): void {
  safePushEvent("chat.xmpp.send.enqueued", {
    kind: payload.kind,
    reason: payload.reason,
  });
}

/**
 * Last reported depth per kind. Queue mutations report *both* kinds on every
 * change, so without this the untouched kind (usually a 0/0 reading) ships a
 * multi-kilobyte Faro beacon per send (#1443).
 *
 * Unchanged *non-zero* readings still re-emit once per minute: the
 * client-experience dashboard reads the series through a short
 * `max_over_time` window, and a permanently stuck queue must stay visible
 * there rather than fading to no-data after its last transition.
 */
const lastQueueDepth = new Map<MessageKind, { reading: string; at: number }>();

const QUEUE_DEPTH_REEMIT_MS = 60_000;

export function reportQueueDepthChange(payload: {
  kind: MessageKind;
  persisted: number;
  inflight: number;
}): void {
  if (!faro) return;
  const reading = `${payload.persisted}/${payload.inflight}`;
  const prior = lastQueueDepth.get(payload.kind);
  const stuckNonZero =
    reading !== "0/0" && prior !== undefined && Date.now() - prior.at >= QUEUE_DEPTH_REEMIT_MS;
  if (prior?.reading === reading && !stuckNonZero) return;
  lastQueueDepth.set(payload.kind, { reading, at: Date.now() });
  safePushMeasurement({
    type: "chat.xmpp.queue.depth",
    values: {
      persisted: payload.persisted,
      inflight: payload.inflight,
    },
  }, { context: { kind: payload.kind } });
}

export function reportSessionLifecycle(payload: {
  type: "fresh" | "resumed";
}): void {
  safePushEvent("chat.xmpp.session.lifecycle", { type: payload.type });
}

/**
 * Beacon the verified audio-processing state of the local call mic (issues
 * #913 / #914) so we can measure, across the fleet, what fraction of calls
 * actually have noise cancellation applied and which layer (browser constraint
 * vs AI model) is doing it. The payload is the PII-free state from
 * {@link callAudioProcessingEventAttributes}; coarse browser/platform context
 * rides along in Faro's event meta. Pure observability — no XMPP/Jingle wire
 * effect. De-dup (at most once per call per distinct state) is the caller's job
 * via `createCallAudioProcessingBeacon`.
 */
export function reportCallAudioProcessing(
  state: VerifiedCallAudioProcessing,
  callKind: CallKind,
): void {
  safePushEvent("chat.call.audio_processing", {
    ...callAudioProcessingEventAttributes(state),
    call_kind: callKind,
  });
}

/**
 * Beacon the media path a call track actually got (#996): the negotiated video
 * codec and the succeeded ICE candidate-pair (type + transport). This is the
 * baseline the #995 codec/Opus/ICE levers verify against and what surfaces the
 * silent "stuck on TCP relay" rate. The payload is the PII-free attribute set
 * from {@link callMediaPathEventAttributes}; coarse browser/platform context
 * rides along in Faro's event meta. Pure observability — no XMPP/Jingle wire
 * effect. De-dup (at most once per call per distinct path) is the caller's job
 * via `createCallMediaPathBeacon`.
 */
export function reportCallMediaPath(snapshot: CallMediaPathSnapshot, callKind: CallKind): void {
  safePushEvent("chat.call.media_path", {
    ...callMediaPathEventAttributes(snapshot),
    call_kind: callKind,
  });
}

/**
 * Beacon the call's ICE/TURN connectivity (#1452): relay-vs-direct media
 * path, whether a TURN relay candidate was gathered at all, and how many
 * times ICE had to restart. Complements {@link reportCallMediaPath}, which
 * is per-track and blind to both "no relay candidate exists" and restart
 * churn. The payload is the PII-free attribute set from
 * {@link callIceEventAttributes} — candidate types and buckets, never an IP
 * address, port, or TURN hostname. Pure observability — no XMPP/Jingle wire
 * effect. De-dup (at most once per call per distinct state) is the caller's
 * job via `createCallIceBeacon`.
 */
export function reportCallIce(snapshot: CallIceSnapshot, callKind: CallKind): void {
  safePushEvent("chat.call.ice", {
    ...callIceEventAttributes(snapshot),
    call_kind: callKind,
  });
}

/**
 * Beacon XEP-0215 TURN credential refresh/expiry without including the
 * credential, server address, or expiry timestamp. The closed status value is
 * intentionally low-cardinality.
 */
export function reportCallIceCredentials(event: IceCredentialEvent, callKind: CallKind): void {
  safePushEvent("chat.call.ice_credentials", {
    credential_state: event,
    call_kind: callKind,
  });
}

export function reportCallLifecycle(
  payload: CallLifecyclePayload,
): void {
  safePushEvent("chat.call.lifecycle", {
    setup_outcome: payload.setupOutcome,
    end_reason: payload.endReason,
    duration_bucket: payload.durationBucket,
    call_kind: payload.callKind,
    rtt_band: payload.rttBand,
    packet_loss_band: payload.packetLossBand,
    connection_quality: payload.connectionQuality,
    reconnect_count: payload.reconnectCount,
  });
}

export function reportCallMediaError(
  mediaKind: "mic" | "cam" | "screen",
  reason: "denied" | "missing" | "in-use" | "failed",
): void {
  reportError({ kind: "call.media", mediaKind, reason });
}

export function reportStatusChange(payload: {
  state: XmppStatusSnapshot["state"];
  reconnectDurationMs?: number;
}): void {
  if (!faro) return;
  safePushEvent("chat.xmpp.status", {
    state: payload.state,
  });
  if (typeof payload.reconnectDurationMs === "number") {
    safePushMeasurement({
      type: "chat.xmpp.reconnect.duration_ms",
      values: { duration_ms: payload.reconnectDurationMs },
    });
  }
}

// ── Client-health telemetry ─────────────────────────────────────────
//
// Observe-only signals for the background-tab `RESULT_CODE_HUNG`
// investigation (`docs/planning/hung-tab-investigation.md`). Every
// signal is tagged with the page's `visibilityState` and a coarse
// hidden-duration bucket, with exact hidden milliseconds as a numeric
// measurement value. That lets the backend show what happens **in the
// background** — escalating long tasks (synchronous-burst hang), reconnect
// flapping, or unbounded heap growth (leak / GC death-spiral) while
// `hidden` each name a different root cause. No behavior change.

/** Sampling cadence for the JS heap. Background timers are clamped to
 * ≥1 min, so a 60 s base interval is the finest useful resolution while
 * backgrounded; foreground samples are exact. */
const HEAP_SAMPLE_INTERVAL_MS = 60_000;

let healthInstalled = false;
let visibility: string =
  typeof document !== "undefined" ? document.visibilityState : "visible";
let hiddenSinceMs: number | null =
  visibility === "hidden" && typeof performance !== "undefined" ? performance.now() : null;

/** Common tags so every health signal can be sliced by foreground vs
 * background without creating one metric series per millisecond hidden. */
function visibilityTags(): { visibility: string; hidden_bucket: string } {
  return { visibility, hidden_bucket: hiddenBucket(hiddenDurationMs()) };
}

function hiddenDurationMs(): number {
  if (hiddenSinceMs === null) return 0;
  if (typeof performance === "undefined") return 0;
  return Math.max(0, Math.round(performance.now() - hiddenSinceMs));
}

function hiddenBucket(msHidden: number): string {
  if (msHidden === 0) return "visible";
  if (msHidden < 60_000) return "lt_1m";
  if (msHidden < 5 * 60_000) return "1m_5m";
  if (msHidden < 15 * 60_000) return "5m_15m";
  if (msHidden < 60 * 60_000) return "15m_1h";
  return "gt_1h";
}

function visibilityMetric(): {
  context: { visibility: string; hidden_bucket: string };
  hiddenMs: number;
} {
  const msHidden =
    hiddenDurationMs();
  return { context: { visibility, hidden_bucket: hiddenBucket(msHidden) }, hiddenMs: msHidden };
}

function reportVisibility(): void {
  safePushEvent("chat.client.visibility", visibilityTags());
}

function reportLongTask(durationMs: number): void {
  const metric = visibilityMetric();
  safePushMeasurement({
    type: "chat.client.longtask.duration_ms",
    values: { duration_ms: Math.round(durationMs), hidden_ms: metric.hiddenMs },
  }, { context: metric.context });
}

interface ChromeMemory {
  usedJSHeapSize: number;
  totalJSHeapSize: number;
  jsHeapSizeLimit: number;
}

function sampleHeap(): void {
  if (!faro || typeof performance === "undefined") return;
  const mem = (performance as Performance & { memory?: ChromeMemory }).memory;
  if (!mem) return; // Chrome-only API; absent elsewhere
  const metric = visibilityMetric();
  safePushMeasurement({
    type: "chat.client.heap",
    values: {
      used_mb: Math.round(mem.usedJSHeapSize / 1_048_576),
      total_mb: Math.round(mem.totalJSHeapSize / 1_048_576),
      limit_mb: Math.round(mem.jsHeapSizeLimit / 1_048_576),
      hidden_ms: metric.hiddenMs,
    },
  }, { context: metric.context });
}

/** Background-flapping detector: one event + one count per scheduled
 * reconnect, tagged with visibility/hidden bucket. A burst while `hidden`
 * points at keepalive-throttling → server idle timeout → reconnect loops. */
export function reportReconnectScheduled(payload: { attempt: number; delayMs: number }): void {
  if (!faro) return;
  const tags = visibilityTags();
  const metric = visibilityMetric();
  safePushEvent("chat.xmpp.reconnect.scheduled", tags);
  safePushMeasurement({
    type: "chat.xmpp.reconnect.attempt",
    values: {
      count: 1,
      attempt: payload.attempt,
      delay_ms: Math.round(payload.delayMs),
      hidden_ms: metric.hiddenMs,
    },
  }, { context: metric.context });
}

/** Catch-up cost: how much work a single reconnect catch-up did. Large
 * or repeated bursts while `hidden` point at the unbounded resume apply. */
export function reportCatchup(payload: {
  conversations: number;
  pages: number;
  pageFailures: number;
  messages: number;
  durationMs: number;
  processedConversations?: number;
  outcome?: "completed" | "aborted" | "failed";
}): void {
  const metric = visibilityMetric();
  safePushMeasurement({
    type: "chat.xmpp.catchup",
    values: {
      conversations: payload.conversations,
      processed_conversations: payload.processedConversations ?? payload.conversations,
      pages: payload.pages,
      page_failures: payload.pageFailures,
      messages: payload.messages,
      duration_ms: Math.round(payload.durationMs),
      hidden_ms: metric.hiddenMs,
    },
  }, { context: { ...metric.context, outcome: payload.outcome ?? "completed" } });
}

/** Resume live-buffer drain: how many buffered messages were applied
 * synchronously on session-ready, and how long that one task took. */
export function reportResumeDrain(payload: { buffered: number; durationMs: number }): void {
  const metric = visibilityMetric();
  safePushMeasurement({
    type: "chat.xmpp.resume_drain",
    values: { buffered: payload.buffered, duration_ms: Math.round(payload.durationMs), hidden_ms: metric.hiddenMs },
  }, { context: metric.context });
}

/**
 * Closed XEP-0198 health signal. Its attributes are deliberately limited to
 * the event discriminant, closed request reason, boolean progress, and a
 * bounded retry attempt: no XML, JIDs, stanza IDs, or server-provided text
 * can reach the Faro collector through this path.
 */
export function reportStreamManagement(event: StreamManagementTelemetryPayload): void {
  const attributes: Record<string, string> = { kind: event.kind };
  switch (event.kind) {
    case "ack-requested":
      attributes.reason = event.reason;
      break;
    case "ack-validated":
      attributes.progress = String(event.progress);
      break;
    case "ack-retry":
      attributes.attempt = String(Math.min(10, Math.max(0, Math.floor(event.attempt))));
      break;
    case "lifecycle-failed":
      attributes.operation = event.operation;
      break;
    case "ack-request-timed-out":
    case "progress-timed-out":
    case "failed":
      break;
  }
  safePushEvent("chat.xmpp.stream_management", attributes);
}

/** Records the closed page-lifecycle failure projection used by XmppProvider. */
export function reportXmppPageLifecycleFailure(failure: {
  operation: "prepare-xmpp" | "resume-xmpp" | "suspend-call";
}): void {
  reportStreamManagement({ kind: "lifecycle-failed", operation: failure.operation });
}

/**
 * Install the page-global client-health observers (long tasks, JS heap,
 * visibility transitions). Idempotent and a no-op without Faro or
 * outside the browser. Called once from {@link initTelemetry}.
 */
function installClientHealthTelemetry(): void {
  if (healthInstalled) return;
  if (!faro || typeof window === "undefined" || typeof document === "undefined") return;
  healthInstalled = true;

  // Visibility transitions — maintain `hidden-since` for tagging and
  // grab a heap sample on every transition so the bg/fg trajectory is
  // captured even between timer ticks.
  document.addEventListener("visibilitychange", () => {
    const next = document.visibilityState;
    if (next === visibility) return;
    visibility = next;
    hiddenSinceMs = next === "hidden" ? performance.now() : null;
    reportVisibility();
    sampleHeap();
  });

  // Long tasks — the smoking gun for a HUNG renderer. The task that
  // ultimately wedges the tab won't complete (so won't be reported), but
  // the escalating tasks before it will, tagged `hidden`.
  if (
    "PerformanceObserver" in window &&
    PerformanceObserver.supportedEntryTypes?.includes("longtask")
  ) {
    try {
      const observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) reportLongTask(entry.duration);
      });
      observer.observe({ entryTypes: ["longtask"] });
    } catch {
      // longtask unsupported in this engine — skip silently.
    }
  }

  // JS heap trend (Chrome `performance.memory`). Detects a leak / GC
  // death-spiral building over a long background idle.
  window.setInterval(sampleHeap, HEAP_SAMPLE_INTERVAL_MS);
  sampleHeap();
}
