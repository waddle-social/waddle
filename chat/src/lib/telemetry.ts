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
 * become parents of backend spans in Tempo for every HTTP call.
 *
 * `withSpan` below is for work that isn't a fetch — e.g. the XMPP
 * connect handshake over WebSocket, outbound message bookkeeping —
 * so we still get client-side timing and failure attribution. The
 * browser WebSocket API does not allow custom headers on the upgrade
 * request, so these manual spans are NOT trace-parented to the
 * server's XMPP session by themselves. Any `fetch` issued inside a
 * `withSpan` callback IS propagated (via the active context set by
 * `context.with`) and will cross-link into backend spans correctly.
 */
import { initializeFaro, getWebInstrumentations, type Faro } from "@grafana/faro-web-sdk";
import { TracingInstrumentation } from "@grafana/faro-web-tracing";
import { context, SpanStatusCode, trace, type Span } from "@opentelemetry/api";

type MessageKind = "room" | "dm";

/** Errors we classify coarsely so Tempo filters stay useful. */
export type ErrorKind =
  | "xmpp.stream"
  | "xmpp.auth"
  | "xmpp.disconnect"
  | "xmpp.send"
  | "xmpp.receive"
  | "storage.quota"
  | "storage.read"
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
  installClientHealthTelemetry();
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
 * The span is also made the *active context* for the duration of
 * `fn`, so any nested auto-instrumented work (a `fetch` called inside
 * the callback, a further `withSpan`) attaches to this span instead
 * of whatever root context was active outside. The backend therefore
 * sees the manual span as the parent of the cross-origin HTTP span,
 * not an orphaned sibling.
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
  const activeCtx = trace.setSpan(context.active(), span);
  return context.with(activeCtx, async () => {
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
  });
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
  }, { context: { kind: payload.kind } });
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
  faro?.api.pushEvent("chat.client.visibility", visibilityTags());
}

function reportLongTask(durationMs: number): void {
  const metric = visibilityMetric();
  faro?.api.pushMeasurement({
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
  faro.api.pushMeasurement({
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
  faro.api.pushEvent("chat.xmpp.reconnect.scheduled", {
    attempt: String(payload.attempt),
    delay_ms: String(Math.round(payload.delayMs)),
    ...tags,
  });
  faro.api.pushMeasurement({
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
  messages: number;
  durationMs: number;
  processedConversations?: number;
  outcome?: "completed" | "aborted" | "failed";
}): void {
  const metric = visibilityMetric();
  faro?.api.pushMeasurement({
    type: "chat.xmpp.catchup",
    values: {
      conversations: payload.conversations,
      processed_conversations: payload.processedConversations ?? payload.conversations,
      pages: payload.pages,
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
  faro?.api.pushMeasurement({
    type: "chat.xmpp.resume_drain",
    values: { buffered: payload.buffered, duration_ms: Math.round(payload.durationMs), hidden_ms: metric.hiddenMs },
  }, { context: metric.context });
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
