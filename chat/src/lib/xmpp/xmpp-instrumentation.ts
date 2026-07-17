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
  type XmppErrorKind,
  type XmppErrorEvent,
} from "@/lib/xmpp/types";
import {
  WASM_DRIVER_TELEMETRY_CODES,
  wasmControlErrorCondition,
} from "@/lib/xmpp/wasm-control-errors";
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
  setXmppResourceForTelemetry,
  reportStreamManagement,
} from "@/lib/telemetry";

const ERROR_KIND_MAP: Record<XmppErrorKind, ErrorKind> = {
  "stream": "xmpp.stream",
  "auth": "xmpp.auth",
  "connect-timeout": "xmpp.disconnect",
  "history": "xmpp.stream",
  "member-query": "xmpp.stream",
  "muc-join": "xmpp.stream",
};
export function installInstrumentation(client: BrowserXmppClient): void {
  setXmppResourceForTelemetry(client.xmppResource);
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
  client.onStreamManagement((event) => {
    reportStreamManagement(event);
  });
  client.onError((event) => {
    const kind = ERROR_KIND_MAP[event.kind];
    const condition = telemetryEventCondition(event);
    const detail = telemetryErrorDetail(event, condition);
    const streamDetail = event.kind === "stream"
      && event.controlError.kind === "driver-error"
      ? event.controlError.reason
      : undefined;
    const smCounts = event.kind === "stream"
      ? telemetryStreamManagementCounts(event)
      : undefined;
    const errorSource = telemetryErrorSource(event.kind, condition);
    const stanzaContext = event.kind !== "stream"
      && event.kind !== "connect-timeout"
      ? event
      : undefined;
    const cause = new Error(detail);
    reportError(kind, cause, {
      recoverable: event.recoverable,
      detail,
      ...(condition ? { condition } : {}),
      ...(stanzaContext?.errorType ? { errorType: stanzaContext.errorType } : {}),
      ...(event.kind !== "muc-join" && stanzaContext?.errorText
        ? { errorText: stanzaContext.errorText }
        : {}),
      ...(event.kind === "muc-join" && event.roomLocalpart
        ? { roomLocalpart: event.roomLocalpart }
        : {}),
      ...(errorSource ? { errorSource } : {}),
      ...(streamDetail ? { streamDetail } : {}),
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
      return event.reason === "room-self-presence"
        ? "room-self-presence-timeout"
        : "connect-timeout";
    case "history":
      return "reconnect-catchup-failed";
    case "member-query":
      if (event.reason === "binding-missing") return "missing-list-room-members";
      return condition ? `member-query-${condition}` : "member-query-failed";
    case "muc-join":
      return condition ? `room-join-${condition}` : "room-join-rejected";
    case "stream":
      if (
        condition === "undefined-condition" &&
        event.controlError.kind === "stream-error" &&
        event.controlError.streamManagementError?.kind === "handled-count-too-high"
      ) {
        return "stream-handled-count-too-high";
      }
      if (condition) return `stream-${condition}`;
      return event.controlError.kind === "driver-error"
        ? WASM_DRIVER_TELEMETRY_CODES[event.controlError.reason]
        : "stream-error";
  }
}

function telemetryStreamManagementCounts(event: XmppErrorEvent): Record<string, string> | undefined {
  if (
    event.kind !== "stream"
    || event.controlError.kind !== "stream-error"
    || event.controlError.streamManagementError?.kind !== "handled-count-too-high"
  ) return undefined;
  return {
    smH: String(event.controlError.streamManagementError.h),
    smSendCount: String(event.controlError.streamManagementError.sendCount),
  };
}

/** Attribute the failure: a condition means the server answered with an
 * error stanza/stream error; `connect-timeout` events are client-side
 * timers (connect stall, room self-presence wait). Anything else is left
 * unattributed rather than guessed. */
function telemetryErrorSource(
  kind: XmppErrorKind,
  condition: string | undefined,
): "server" | "local-timeout" | undefined {
  if (condition) return "server";
  if (kind === "connect-timeout") return "local-timeout";
  return undefined;
}

type StanzaErrorKind = Exclude<
  XmppErrorKind,
  "stream" | "connect-timeout"
>;

function telemetryCondition(
  _kind: StanzaErrorKind,
  condition: string | undefined,
): string | undefined {
  if (!condition) return undefined;
  const normalized = condition.trim().toLowerCase();
  if (!normalized) return undefined;
  return XMPP_ERROR_CONDITIONS.has(normalized) ? normalized : "unknown";
}

function telemetryEventCondition(event: XmppErrorEvent): string | undefined {
  if (event.kind === "stream") {
    return wasmControlErrorCondition(event.controlError);
  }
  if (event.kind === "connect-timeout") return undefined;
  return telemetryCondition(event.kind, event.condition);
}
