/**
 * RFC 6120 stanza error conditions an XEP-0198 `<failed/>` can carry, as the
 * Rust runtime names them in its stream-management failure event.
 */
const SM_FAILURE_CONDITIONS = [
  "bad-request",
  "conflict",
  "feature-not-implemented",
  "forbidden",
  "gone",
  "internal-server-error",
  "item-not-found",
  "jid-malformed",
  "not-acceptable",
  "not-allowed",
  "not-authorized",
  "policy-violation",
  "recipient-unavailable",
  "redirect",
  "registration-required",
  "remote-server-not-found",
  "remote-server-timeout",
  "resource-constraint",
  "service-unavailable",
  "subscription-required",
  "undefined-condition",
  "unexpected-request",
] as const;

export type SmFailureCondition = (typeof SM_FAILURE_CONDITIONS)[number];

const KNOWN_CONDITIONS: ReadonlySet<string> = new Set(SM_FAILURE_CONDITIONS);

/** Whether `value` is one of the closed condition names, not arbitrary text. */
export function isSmFailureCondition(value: unknown): value is SmFailureCondition {
  return typeof value === "string" && KNOWN_CONDITIONS.has(value);
}
