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
  type XmppErrorEvent,
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
    const smCounts = event.kind === "stream"
      ? telemetryStreamManagementCounts(event)
      : undefined;
    const cause = new Error(detail);
    reportError(kind, cause, {
      recoverable: event.recoverable,
      detail,
      ...(condition ? { condition } : {}),
      ...(smCounts ?? {}),
    });
  });
  // Background-tab RESULT_CODE_HUNG investigation (observe-only). These
  // pair with the page-global longtask/heap/visibility signals installed
  // by `installClientHealthTelemetry()` in `@/lib/telemetry`.
  client.onReconnectScheduled((info) => reportReconnectScheduled(info));
  client.onCatchup((info) => reportCatchup(info));
  client.onResumeDrain((info) => reportResumeDrain(info));
}

function telemetryErrorDetail(event: XmppErrorEvent, condition: string | undefined): string {
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
      if (
        condition === "undefined-condition" &&
        event.streamManagementError?.kind === "handled-count-too-high"
      ) {
        return "stream-handled-count-too-high";
      }
      return condition ? `stream-${condition}` : telemetryStreamFallbackDetail(event.detail);
  }
}

function telemetryStreamFallbackDetail(detail: string): string {
  const normalized = detail.trim().toLowerCase();
  if (!normalized || normalized === "stream error") return "stream-error";
  if (normalized === "handled-count-too-high") return "stream-handled-count-too-high";
  if (
    normalized.includes("websocket transport error") ||
    normalized.includes("websocket transport failed") ||
    normalized.includes("websocket transport is already closed") ||
    normalized.includes("transport closed")
  ) {
    return "stream-transport-error";
  }
  if (
    normalized.includes("malformed xmpp framing") ||
    normalized.includes("invalid transport frame")
  ) {
    return "stream-invalid-transport-frame";
  }
  if (normalized.includes("empty xmpp frame")) return "stream-empty-transport-frame";
  if (normalized.includes("unsupported message type")) return "stream-unsupported-websocket-message";
  if (normalized.includes("disconnected")) return "stream-disconnected";
  return "stream-error";
}

function telemetryStreamManagementCounts(event: XmppErrorEvent): Record<string, number> | undefined {
  if (event.streamManagementError?.kind !== "handled-count-too-high") return undefined;
  return {
    smH: event.streamManagementError.h,
    smSendCount: event.streamManagementError.sendCount,
  };
}

function telemetryCondition(kind: XmppErrorKind, condition: string | undefined): string | undefined {
  if (!condition) return undefined;
  const normalized = condition.trim().toLowerCase();
  if (!normalized) return undefined;
  const allowed = kind === "stream" ? XMPP_STREAM_ERROR_CONDITIONS : XMPP_ERROR_CONDITIONS;
  return allowed.has(normalized) ? normalized : "unknown";
}
