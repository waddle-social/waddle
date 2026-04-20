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
 */
import { initializeFaro, type Faro } from "@grafana/faro-web-sdk";

type MessageKind = "room" | "dm";

interface InitTelemetryOptions {
  /** Faro collector URL (from Grafana Cloud Faro app config). */
  url: string;
  /** App name, typically the env-specific identifier e.g. `waddle-chat`. */
  appName: string;
  /** Commit SHA used as the app version, for Faro release correlation. */
  release?: string;
}

let faro: Faro | null = null;

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
    faro = initializeFaro({
      url: options.url,
      app: {
        name: options.appName || "waddle-chat",
        version: options.release,
      },
      instrumentations: [],
    });
  } catch (err) {
    // Faro itself throwing here is already a telemetry bug; log to the
    // console so it surfaces in devtools but never propagate — chat
    // must continue to work with or without telemetry.
    console.error("Faro initialization failed", err);
    faro = null;
  }
}

/** For tests only — inject a stub or clear state between test cases. */
export function __setFaroForTesting(instance: Faro | null): void {
  faro = instance;
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
