/** Bounded error categories that may cross the browser telemetry boundary. */
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
  | "upload"
  | "window-error"
  | "unhandled-rejection"
  | "vue-render-error";

const ERROR_KINDS = new Set<ErrorKind>([
  "xmpp.stream",
  "xmpp.auth",
  "xmpp.disconnect",
  "xmpp.send",
  "xmpp.receive",
  "storage.quota",
  "storage.read",
  "storage.write",
  "http.fetch",
  "upload",
  "window-error",
  "unhandled-rejection",
  "vue-render-error",
]);
const SAFE_XMPP_ERROR_CONDITIONS = [
  "bad-format",
  "bad-namespace-prefix",
  "conflict",
  "connection-timeout",
  "forbidden",
  "host-gone",
  "host-unknown",
  "improper-addressing",
  "internal-server-error",
  "invalid-from",
  "invalid-namespace",
  "invalid-xml",
  "item-not-found",
  "not-authorized",
  "not-well-formed",
  "policy-violation",
  "remote-connection-failed",
  "reset",
  "resource-constraint",
  "restricted-xml",
  "see-other-host",
  "service-unavailable",
  "system-shutdown",
  "undefined-condition",
  "unsupported-encoding",
  "unsupported-feature",
  "unsupported-stanza-type",
  "unsupported-version",
] as const;
const SAFE_XMPP_ERROR_CONDITION_SET = new Set<string>([
  ...SAFE_XMPP_ERROR_CONDITIONS,
  "unknown",
]);
const SAFE_ERROR_DETAILS = new Set([
  "GET /api/auth/session",
  "POST /api/auth/logout",
  "auth-error",
  "connect-timeout",
  "dm call join cache clear failed",
  "dm call join cache read failed",
  "dm call join cache write failed",
  "missing-list-room-members",
  "muc call session cache clear failed",
  "muc call session cache read failed",
  "muc call session cache write failed",
  "outbound-queue prune failed",
  "outbound-queue read failed",
  "outbound-queue write failed",
  "reconnect-catchup-failed",
  "resume-persistence consume failed (sm)",
  "room-self-presence-timeout",
  "stream-disconnected",
  "stream-empty-transport-frame",
  "stream-error",
  "stream-handled-count-too-high",
  "stream-invalid-transport-frame",
  "stream-transport-error",
  "stream-unsupported-websocket-message",
  "xep-0363-put-failed",
  "xep-0363-put-network-error",
  ...SAFE_XMPP_ERROR_CONDITIONS.map((condition) => `member-query-${condition}`),
  ...SAFE_XMPP_ERROR_CONDITIONS.map((condition) => `stream-${condition}`),
  "member-query-unknown",
]);
const SAFE_ERROR_STORAGE_AREAS = new Set([
  "catchup",
  "dm-call-join-cache",
  "joined-rooms",
  "muc-call-session-cache",
  "outbound-queue",
  "owner-handoff",
  "owner-lease",
  "sm",
  "sm-consumed",
  "sm-resume",
]);
const SAFE_ERROR_COUNT_KEYS = new Set([
  "dropped",
  "queuesize",
  "smh",
  "smsendcount",
  "status",
]);
const MAX_ERROR_COUNT = 1_000_000_000;

export function categorizedErrorForTelemetry(kind: ErrorKind, detail: string | undefined): Error {
  return telemetryError(detail ?? kind);
}

export function categorizedOperationErrorForTelemetry(): Error {
  return telemetryError("operation-error");
}

function telemetryError(message: string): Error {
  const categorized = new Error(message);
  categorized.name = "WaddleTelemetryError";
  categorized.stack = undefined;
  return categorized;
}

export function privacySafeErrorContext(
  kind: ErrorKind,
  context: Record<string, unknown>,
): Record<string, string> {
  const safe: Record<string, string> = {
    kind,
    recoverable: String(context.recoverable === true || context.recoverable === "true"),
  };

  const detail = boundedErrorDetail(context.detail);
  if (detail) safe.detail = detail;

  const condition = boundedXmppCondition(context.condition);
  if (condition) safe.condition = condition;

  const storageArea = boundedStorageArea(context.storage_area);
  if (storageArea) safe.storage_area = storageArea;

  for (const [key, value] of Object.entries(context)) {
    if (!SAFE_ERROR_COUNT_KEYS.has(normalizeAttributeKey(key))) continue;
    const count = boundedErrorCount(value);
    if (count !== undefined) safe[key] = String(count);
  }

  return safe;
}

export function boundedErrorDetail(value: unknown): string | undefined {
  return typeof value === "string" && SAFE_ERROR_DETAILS.has(value) ? value : undefined;
}

export function isErrorKind(value: unknown): value is ErrorKind {
  return typeof value === "string" && ERROR_KINDS.has(value as ErrorKind);
}

function boundedXmppCondition(value: unknown): string | undefined {
  return typeof value === "string" && SAFE_XMPP_ERROR_CONDITION_SET.has(value)
    ? value
    : undefined;
}

function boundedStorageArea(value: unknown): string | undefined {
  return typeof value === "string" && SAFE_ERROR_STORAGE_AREAS.has(value)
    ? value
    : undefined;
}

function boundedErrorCount(value: unknown): number | undefined {
  const parsed = typeof value === "number"
    ? value
    : typeof value === "string" && /^\d+$/.test(value)
      ? Number(value)
      : Number.NaN;
  if (!Number.isFinite(parsed) || parsed < 0) return undefined;
  return Math.min(Math.round(parsed), MAX_ERROR_COUNT);
}

function normalizeAttributeKey(key: string): string {
  return key.replace(/[^a-z0-9]/gi, "").toLowerCase();
}
