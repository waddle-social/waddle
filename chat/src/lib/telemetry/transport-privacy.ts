import type { TransportItem } from "@grafana/faro-web-sdk";
import {
  boundedErrorDetail,
  isErrorKind,
  privacySafeErrorContext,
} from "./error-classification";
import {
  sanitizeKnownEventPayload,
  sanitizeKnownMeasurementPayload,
  sanitizeTimestamp,
  sanitizeTraceContext,
} from "./transport-schema";
import {
  sanitizeTracePayload,
  type TracePrivacyPolicy,
} from "./trace-privacy";

/** Replace a Faro payload with its strict privacy-safe wire representation. */
export function sanitizeTransportPayload(
  item: TransportItem,
  policy: TracePrivacyPolicy,
): boolean {
  let payload: Record<string, unknown> | null = null;
  if (item.type === "trace") {
    payload = sanitizeTracePayload(item.payload, policy);
  } else if (item.type === "event") {
    payload = sanitizeKnownEventPayload(item.payload);
  } else if (item.type === "measurement") {
    payload = sanitizeKnownMeasurementPayload(item.payload);
  } else if (item.type === "exception") {
    payload = sanitizeExceptionPayload(item.payload);
  }
  if (!payload) return false;
  item.payload = payload as TransportItem["payload"];
  return true;
}

function sanitizeExceptionPayload(payload: unknown): Record<string, unknown> | null {
  if (!isRecord(payload)) return null;
  const kind = isErrorKind(payload.type) ? payload.type : "window-error";
  const detail = boundedErrorDetail(payload.value);
  const context = privacySafeErrorContext(
    kind,
    isRecord(payload.context) ? payload.context : {},
  );
  return Object.fromEntries(Object.entries({
    type: kind,
    value: detail ?? kind,
    fatal: typeof payload.fatal === "boolean" ? payload.fatal : undefined,
    timestamp: sanitizeTimestamp(payload.timestamp),
    context,
    trace: sanitizeTraceContext(payload.trace),
  }).filter(([, value]) => value !== undefined));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
