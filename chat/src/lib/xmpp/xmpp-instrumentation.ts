/**
 * Glue between `BrowserXmppClient`'s telemetry hook API and Grafana Faro.
 *
 * Every function this installs is a no-op when Faro isn't initialized —
 * the SDK wrapper in `@/lib/telemetry` handles that at the report-site,
 * so tests and non-prod builds can `installInstrumentation()` freely
 * without any beacon traffic.
 */
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ErrorKind } from "@/lib/telemetry";
import {
  XMPP_ERROR_CONDITIONS,
  XMPP_STREAM_ERROR_CONDITIONS,
  type XmppErrorKind,
} from "@/lib/xmpp/types";
import {
  reportCatchup,
  reportError,
  reportMessageAcked,
  reportMessageFailed,
  reportQueueDepthChange,
  reportReconnectScheduled,
  reportResumeDrain,
  reportSendEnqueued,
  reportSessionLifecycle,
  reportStatusChange,
} from "@/lib/telemetry";

const ERROR_KIND_MAP: Record<XmppErrorKind, ErrorKind> = {
  "stream": "xmpp.stream",
  "auth": "xmpp.auth",
  "connect-timeout": "xmpp.disconnect",
  "history": "xmpp.stream",
  "member-query": "xmpp.stream",
};
export function installInstrumentation(client: BrowserXmppClient): void {
  client.onMessageAcked((id, meta) => {
    reportMessageAcked({ id, kind: meta.kind, latencyMs: meta.latencyMs });
  });
  client.onMessageDeliveryFailed((id, meta) => {
    reportMessageFailed({ id, kind: meta.kind });
  });
  client.onSessionLifecycle((event) => {
    reportSessionLifecycle({ type: event.type });
  });
  client.onStatus((status, meta) => {
    reportStatusChange({
      state: status.state,
      reconnectDurationMs: meta.reconnectDurationMs,
    });
  });
  client.onSendEnqueued((info) => {
    reportSendEnqueued({ kind: info.kind, reason: info.reason });
  });
  client.onQueueDepthChange((depth) => {
    reportQueueDepthChange(depth);
  });
  client.onError((event) => {
    const kind = ERROR_KIND_MAP[event.kind];
    const condition = telemetryCondition(event.kind, event.condition);
    const detail = telemetryErrorDetail(event, condition);
    const cause = new Error(detail);
    reportError(kind, cause, {
      recoverable: event.recoverable,
      detail,
      ...(condition ? { condition } : {}),
    });
  });
  // Background-tab RESULT_CODE_HUNG investigation (observe-only). These
  // pair with the page-global longtask/heap/visibility signals installed
  // by `installClientHealthTelemetry()` in `@/lib/telemetry`.
  client.onReconnectScheduled((info) => reportReconnectScheduled(info));
  client.onCatchup((info) => reportCatchup(info));
  client.onResumeDrain((info) => reportResumeDrain(info));
}

function telemetryErrorDetail(event: {
  kind: XmppErrorKind;
  detail: string;
  condition?: string;
}, condition: string | undefined): string {
  switch (event.kind) {
    case "auth":
      return "auth-error";
    case "connect-timeout":
      return event.detail.includes("self-presence")
        ? "room-self-presence-timeout"
        : "connect-timeout";
    case "history":
      return "reconnect-catchup-failed";
    case "member-query":
      if (event.detail === "missing list_room_members") return "missing-list-room-members";
      return condition ? `member-query-${condition}` : "member-query-failed";
    case "stream":
      return condition ? `stream-${condition}` : "stream-error";
  }
}

function telemetryCondition(kind: XmppErrorKind, condition: string | undefined): string | undefined {
  if (!condition) return undefined;
  const normalized = condition.trim().toLowerCase();
  if (!normalized) return undefined;
  const allowed = kind === "stream" ? XMPP_STREAM_ERROR_CONDITIONS : XMPP_ERROR_CONDITIONS;
  return allowed.has(normalized) ? normalized : "unknown";
}
