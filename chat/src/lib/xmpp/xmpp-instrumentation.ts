/**
 * Glue between `BrowserXmppClient`'s telemetry hook API and Grafana Faro.
 *
 * Every function this installs is a no-op when Faro isn't initialized —
 * the SDK wrapper in `@/lib/telemetry` handles that at the report-site,
 * so tests and non-prod builds can `installInstrumentation()` freely
 * without any beacon traffic.
 */
import type { BrowserXmppClient } from "@/lib/xmpp-client";
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
  reportStreamManagement,
  markErrorReportedToTelemetry,
} from "@/lib/telemetry";
import type {
  TelemetryErrorObservation,
  XmppCondition,
  XmppOperationReason,
  XmppStreamReason,
} from "@/lib/telemetry-observations";

export function installInstrumentation(client: BrowserXmppClient): void {
  client.onMessageAcked((meta) => {
    reportMessageAcked({ kind: meta.kind, latencyMs: meta.latencyMs });
  });
  client.onMessageDeliveryFailed((meta) => {
    reportMessageFailed({ kind: meta.kind });
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
    reportSendEnqueued({ kind: info.kind, reason: telemetryEnqueueReason(info.reason) });
  });
  client.onQueueDepthChange((depth) => {
    reportQueueDepthChange(depth);
  });
  client.onError((event) => {
    const isRoomJoinFailure = event.kind === "muc-join"
      || event.kind === "muc-join-timeout";
    if (isRoomJoinFailure && event.cause !== undefined) {
      markErrorReportedToTelemetry(event.cause);
    }
    reportError(telemetryErrorObservation(event));
  });
  // Background-tab RESULT_CODE_HUNG investigation (observe-only). These
  // pair with the page-global longtask/heap/visibility signals installed
  // by `installClientHealthTelemetry()` in `@/lib/telemetry`.
  client.onReconnectScheduled((info) => reportReconnectScheduled(info));
  client.onCatchup((info) => reportCatchup(info));
  client.onResumeDrain((info) => reportResumeDrain(info));
  client.onStreamManagement((event) => reportStreamManagement(event));
}

function telemetryErrorDetail(
  event: XmppErrorEvent,
  condition: XmppCondition | undefined,
): XmppStreamReason | XmppOperationReason | "auth-error" | "connect-timeout" | "room-self-presence-timeout" {
  switch (event.kind) {
    case "auth":
      return "auth-error";
    case "connect-timeout":
      return "connect-timeout";
    case "history":
      return "reconnect-catchup-failed";
    case "member-query":
      if (event.detail === "missing list_room_members") return "missing-list-room-members";
      return condition ? `member-query-${condition}` : "member-query-failed";
    case "muc-join":
      return condition ? `room-join-${condition}` : "room-join-rejected";
    case "muc-join-timeout":
      return "room-self-presence-timeout";
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

function telemetryStreamFallbackDetail(detail: string): XmppStreamReason {
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

function telemetryStreamManagementCounts(event: XmppErrorEvent): { h: number; sendCount: number } | undefined {
  if (event.streamManagementError?.kind !== "handled-count-too-high") return undefined;
  return { h: event.streamManagementError.h, sendCount: event.streamManagementError.sendCount };
}

/** Attribute the failure: a condition means the server answered with an
 * error stanza/stream error; timeout events are client-side timers.
 * Anything else is left
 * unattributed rather than guessed. */
function telemetryErrorSource(
  kind: XmppErrorKind,
  condition: XmppCondition | undefined,
): "server" | "local-timeout" | undefined {
  if (condition) return "server";
  if (kind === "connect-timeout" || kind === "muc-join-timeout") return "local-timeout";
  return undefined;
}

function telemetryCondition(kind: XmppErrorKind, condition: string | undefined): XmppCondition | undefined {
  if (!condition) return undefined;
  const normalized = condition.trim().toLowerCase();
  if (!normalized) return undefined;
  const allowed = kind === "stream" ? XMPP_STREAM_ERROR_CONDITIONS : XMPP_ERROR_CONDITIONS;
  return (allowed.has(normalized) ? normalized : "unknown") as XmppCondition;
}

function telemetryEnqueueReason(reason: string): "offline" | "disposed" | "destroying" | "no-client" | "reconnecting" | "not-ready" {
  switch (reason) {
    case "offline":
    case "disposed":
    case "destroying":
    case "no-client":
    case "reconnecting":
      return reason;
    default:
      return "not-ready";
  }
}

function telemetryErrorObservation(event: XmppErrorEvent): TelemetryErrorObservation {
  const condition = telemetryCondition(event.kind, event.condition);
  const reason = telemetryErrorDetail(event, condition);
  const errorSource = telemetryErrorSource(event.kind, condition);
  switch (event.kind) {
    case "stream": {
      const streamManagementCounts = telemetryStreamManagementCounts(event);
      return {
        kind: "xmpp.stream",
        reason: reason as XmppStreamReason,
        recoverable: event.recoverable,
        ...(condition ? { condition } : {}),
        ...(event.errorType ? { errorType: event.errorType } : {}),
        ...(errorSource ? { errorSource } : {}),
        ...(streamManagementCounts ? { sm: streamManagementCounts } : {}),
      } satisfies TelemetryErrorObservation;
    }
    case "auth":
      return {
        kind: "xmpp.auth",
        reason: "auth-error",
        recoverable: event.recoverable,
      } satisfies TelemetryErrorObservation;
    case "connect-timeout":
      return {
        kind: "xmpp.disconnect",
        reason: "connect-timeout",
        recoverable: event.recoverable,
        errorSource: "local-timeout",
      } satisfies TelemetryErrorObservation;
    case "muc-join-timeout":
      return {
        kind: "xmpp.disconnect",
        reason: "room-self-presence-timeout",
        recoverable: event.recoverable,
        errorSource: "local-timeout",
      } satisfies TelemetryErrorObservation;
    case "history":
      return {
        kind: "xmpp.history",
        reason: reason as XmppOperationReason,
        recoverable: event.recoverable,
        ...(condition ? { condition } : {}),
        ...(event.errorType ? { errorType: event.errorType } : {}),
        ...(errorSource ? { errorSource } : {}),
      } satisfies TelemetryErrorObservation;
    case "member-query":
      return {
        kind: "xmpp.member-query",
        reason: reason as XmppOperationReason,
        recoverable: event.recoverable,
        ...(condition ? { condition } : {}),
        ...(event.errorType ? { errorType: event.errorType } : {}),
        ...(errorSource ? { errorSource } : {}),
      } satisfies TelemetryErrorObservation;
    case "muc-join":
      return {
        kind: "xmpp.muc-join",
        reason: reason as XmppOperationReason,
        recoverable: event.recoverable,
        ...(condition ? { condition } : {}),
        ...(event.errorType ? { errorType: event.errorType } : {}),
        ...(errorSource ? { errorSource } : {}),
      } satisfies TelemetryErrorObservation;
  }
}
