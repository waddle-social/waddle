import { getFaro, observeTelemetry, pushEventObserveOnly, pushMeasurementObserveOnly } from "./runtime";

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
  pushEventObserveOnly("chat.client.visibility", visibilityTags());
}

function reportLongTask(durationMs: number): void {
  const metric = visibilityMetric();
  pushMeasurementObserveOnly({
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
  if (!getFaro() || typeof performance === "undefined") return;
  const mem = (performance as Performance & { memory?: ChromeMemory }).memory;
  if (!mem) return; // Chrome-only API; absent elsewhere
  const metric = visibilityMetric();
  pushMeasurementObserveOnly({
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
  observeTelemetry(() => {
    if (!getFaro()) return;
    const tags = visibilityTags();
    const metric = visibilityMetric();
    pushEventObserveOnly("chat.xmpp.reconnect.scheduled", tags);
    pushMeasurementObserveOnly({
      type: "chat.xmpp.reconnect.attempt",
      values: {
        count: 1,
        attempt: payload.attempt,
        delay_ms: Math.round(payload.delayMs),
        hidden_ms: metric.hiddenMs,
      },
    }, { context: metric.context });
  });
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
  observeTelemetry(() => {
    const metric = visibilityMetric();
    pushMeasurementObserveOnly({
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
  });
}

/** Resume live-buffer drain: how many buffered messages were applied
 * synchronously on session-ready, and how long that one task took. */
export function reportResumeDrain(payload: { buffered: number; durationMs: number }): void {
  observeTelemetry(() => {
    const metric = visibilityMetric();
    pushMeasurementObserveOnly({
      type: "chat.xmpp.resume_drain",
      values: { buffered: payload.buffered, duration_ms: Math.round(payload.durationMs), hidden_ms: metric.hiddenMs },
    }, { context: metric.context });
  });
}

/**
 * Install the page-global client-health observers (long tasks, JS heap,
 * visibility transitions). Idempotent and a no-op without Faro or
 * outside the browser. Called once from {@link initTelemetry}.
 */
export function installClientHealthTelemetry(): void {
  if (healthInstalled) return;
  if (!getFaro() || typeof window === "undefined" || typeof document === "undefined") return;
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
