/**
 * Grafana Faro RUM wrapper.
 *
 * All reporting functions are no-ops when Faro isn't initialized, which
 * is the default in local dev and in tests. Initialization is gated on
 * the runtime presence of `PUBLIC_FARO_URL` — there is no "disabled"
 * mode in the SDK itself; we just skip `initializeFaro()` entirely so
 * no beacons leave the page.
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
 * become parents of backend spans in Tempo. `withSpan` below is for
 * work that isn't a fetch — e.g. the XMPP-over-WebSocket connect
 * handshake — so it still shows up in the same trace.
 */
import { initializeFaro, getWebInstrumentations, type Faro } from "@grafana/faro-web-sdk";
import { TracingInstrumentation } from "@grafana/faro-web-tracing";
import { SpanStatusCode, trace, type Span } from "@opentelemetry/api";

type MessageKind = "room" | "dm";

/** Errors we classify coarsely so Tempo filters stay useful. */
export type ErrorKind =
  | "xmpp.stream"
  | "xmpp.auth"
  | "xmpp.disconnect"
  | "xmpp.send"
  | "xmpp.receive"
  | "storage.quota"
  | "storage.write"
  | "http.fetch"
  | "upload";

interface InitTelemetryOptions {
  /** Faro collector URL (from Grafana Cloud Faro app config). */
  url: string;
  /** App name, typically the env-specific identifier e.g. `waddle-chat`. */
  appName: string;
  /** Commit SHA used as the app version, for Faro release correlation. */
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

const TRACER_NAME = "waddle-chat";

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
      .map((entry) => normalizePropagationEntry(entry));

    faro = initializeFaro({
      url: options.url,
      app: {
        name: options.appName || "waddle-chat",
        version: options.release,
      },
      instrumentations: [
        // Default browser instrumentations: uncaught errors, unhandled
        // promise rejections, console errors, web vitals, session
        // tracking, view/route changes.
        ...getWebInstrumentations(),
        // Wraps fetch + XMLHttpRequest in OTel spans and injects
        // traceparent/tracestate on requests whose URL matches one of
        // `propagateTraceHeaderCorsUrls`. Without a matching entry the
        // browser does NOT send those headers cross-origin, so the
        // backend can't join the trace.
        new TracingInstrumentation({
          instrumentationOptions: {
            propagateTraceHeaderCorsUrls: propagateUrls,
          },
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
}

/**
 * Turn a string into the RegExp or URL-prefix shape
 * `TracingInstrumentation` expects. A plain URL like
 * `https://xmpp.waddle.social` matches as a prefix, which is what we
 * want — every `/api/...` call under that origin joins the trace.
 */
function normalizePropagationEntry(entry: string): string | RegExp {
  const trimmed = entry.trim();
  return trimmed.endsWith("/") ? trimmed.slice(0, -1) : trimmed;
}

/** For tests only — inject a stub or clear state between test cases. */
export function __setFaroForTesting(instance: Faro | null): void {
  faro = instance;
}

/**
 * Run `fn` inside a manually-started OpenTelemetry span. Use this for
 * operations that aren't `fetch` / `XHR` (so Faro's automatic span
 * wrapping doesn't cover them) — XMPP connect, message send, room
 * join. When Faro isn't initialized, `fn` runs with no span overhead.
 *
 * The returned span is exposed to the callback so callers can attach
 * domain attributes ("room.jid", "message.kind", etc.) without
 * needing their own tracer handle.
 */
export async function withSpan<T>(
  name: string,
  attributes: Record<string, string | number | boolean>,
  fn: (span: Span | null) => Promise<T>,
): Promise<T> {
  if (!faro) return fn(null);
  const tracer = trace.getTracer(TRACER_NAME);
  const span = tracer.startSpan(name, { attributes });
  try {
    const result = await fn(span);
    span.setStatus({ code: SpanStatusCode.OK });
    return result;
  } catch (err) {
    span.setStatus({
      code: SpanStatusCode.ERROR,
      message: err instanceof Error ? err.message : String(err),
    });
    if (err instanceof Error) span.recordException(err);
    throw err;
  } finally {
    span.end();
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
  kind: ErrorKind,
  error: unknown,
  context: { recoverable: boolean; detail?: string; [attr: string]: unknown } = {
    recoverable: true,
  },
): void {
  if (!faro) return;
  const err = error instanceof Error ? error : new Error(String(error));
  const contextStrings: Record<string, string> = { kind, recoverable: String(context.recoverable) };
  for (const [k, v] of Object.entries(context)) {
    if (k === "recoverable") continue;
    if (v === undefined || v === null) continue;
    contextStrings[k] = typeof v === "string" ? v : JSON.stringify(v);
  }
  faro.api.pushError(err, { type: kind, context: contextStrings });
}

export function reportMessageFailed(payload: {
  id: string;
  kind: MessageKind;
}): void {
  faro?.api.pushEvent("chat.xmpp.message.failed", {
    id: payload.id,
    kind: payload.kind,
  });
}

export function reportMessageAcked(payload: {
  id: string;
  kind: MessageKind;
  latencyMs: number;
}): void {
  if (!faro) return;
  faro.api.pushEvent("chat.xmpp.message.acked", {
    id: payload.id,
    kind: payload.kind,
    latency_ms: String(Math.round(payload.latencyMs)),
  });
  faro.api.pushMeasurement({
    type: "chat.xmpp.message.acked.latency_ms",
    values: { latency_ms: payload.latencyMs },
    context: { kind: payload.kind },
  });
}

export function reportSendEnqueued(payload: {
  kind: MessageKind;
  reason: string;
}): void {
  faro?.api.pushEvent("chat.xmpp.send.enqueued", {
    kind: payload.kind,
    reason: payload.reason,
  });
}

export function reportQueueDepthChange(payload: {
  persisted: number;
  inflight: number;
}): void {
  if (!faro) return;
  faro.api.pushMeasurement({
    type: "chat.xmpp.queue.depth",
    values: {
      persisted: payload.persisted,
      inflight: payload.inflight,
    },
  });
}

export function reportSessionLifecycle(payload: {
  type: "fresh" | "resumed";
}): void {
  faro?.api.pushEvent("chat.xmpp.session.lifecycle", { type: payload.type });
}

export function reportStatusChange(payload: {
  state: string;
  detail?: string;
  reconnectDurationMs?: number;
}): void {
  if (!faro) return;
  faro.api.pushEvent("chat.xmpp.status", {
    state: payload.state,
    detail: payload.detail ?? "",
  });
  if (typeof payload.reconnectDurationMs === "number") {
    faro.api.pushMeasurement({
      type: "chat.xmpp.reconnect.duration_ms",
      values: { duration_ms: payload.reconnectDurationMs },
    });
  }
}
