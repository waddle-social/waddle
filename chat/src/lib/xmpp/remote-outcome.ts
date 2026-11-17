// What a failed request to a remote community means for the user.
//
// The server that hosts a room or Space, or this deployment's router on its
// behalf, answers with an RFC 6120 §8.3 stanza error. Three families must
// never be confused, because they call for different behaviour:
//
// - an outage (`remote-server-timeout`, `remote-server-not-found`, no answer
//   at all): the community may be fine and unreachable for now. Retry, with
//   backoff when the error says to wait;
// - a denial (`forbidden`, `registration-required`, `not-authorized`, …):
//   the host answered and refused. Retrying changes nothing until someone
//   grants access, so nothing retries on its own;
// - a deletion (`item-not-found`, `gone`): the host does not know the
//   address. A Waddle host gives the same answer for a room or Space that
//   exists but is not open to other servers, so the copy says "not
//   available" rather than "deleted", and nothing is removed automatically.
//
// Classification is by condition first: `remote-server-not-found` arrives
// with type `cancel` although it reports an outage.

import type { XmppStanzaErrorType } from "./types";
import { stanzaErrorContext } from "./stanza-error-context";

type RemoteFailureKind =
  | "outage"
  | "denial"
  | "deletion"
  | "conflict"
  | "unavailable"
  | "invalid"
  | "unknown";

/**
 * How a failure may be retried:
 * - `automatic`: retry on a backoff schedule, and offer a manual retry;
 * - `manual`: offer a retry the user starts, never retry alone;
 * - `none`: retrying the same request cannot succeed.
 */
type RemoteRetry = "automatic" | "manual" | "none";

export interface RemoteFailure {
  kind: RemoteFailureKind;
  retry: RemoteRetry;
  condition?: string;
  errorType?: XmppStanzaErrorType;
}

export type RemoteSubject = "room" | "space" | "server";

const OUTAGE_CONDITIONS: ReadonlySet<string> = new Set([
  "remote-server-timeout",
  "remote-server-not-found",
  "resource-constraint",
  "recipient-unavailable",
]);

const DENIAL_CONDITIONS: ReadonlySet<string> = new Set([
  "forbidden",
  "registration-required",
  "not-authorized",
  "subscription-required",
  "not-allowed",
  "policy-violation",
]);

const DELETION_CONDITIONS: ReadonlySet<string> = new Set([
  "item-not-found",
  "gone",
]);

const UNAVAILABLE_CONDITIONS: ReadonlySet<string> = new Set([
  "service-unavailable",
  "feature-not-implemented",
]);

const INVALID_CONDITIONS: ReadonlySet<string> = new Set([
  "bad-request",
  "jid-malformed",
  "not-acceptable",
  "unexpected-request",
]);

/** Classify a stanza error, or the absence of any answer (`timedOut`). */
export function classifyRemoteFailure(input: {
  condition?: string | null;
  errorType?: XmppStanzaErrorType | null;
  timedOut?: boolean;
}): RemoteFailure {
  const condition = input.condition?.trim().toLowerCase() || undefined;
  const errorType = input.errorType ?? undefined;
  const context = {
    ...(condition ? { condition } : {}),
    ...(errorType ? { errorType } : {}),
  };
  if (input.timedOut && !condition) return { kind: "outage", retry: "automatic", ...context };
  if (condition && OUTAGE_CONDITIONS.has(condition)) {
    // A server that could not be found is not retried in a loop; a server
    // that did not answer in time, or asked us to wait, is.
    const retry: RemoteRetry = condition === "remote-server-not-found" && errorType !== "wait"
      ? "manual"
      : "automatic";
    return { kind: "outage", retry, ...context };
  }
  if (condition && DENIAL_CONDITIONS.has(condition)) return { kind: "denial", retry: "manual", ...context };
  if (condition && DELETION_CONDITIONS.has(condition)) return { kind: "deletion", retry: "none", ...context };
  if (condition === "conflict") return { kind: "conflict", retry: "manual", ...context };
  if (condition && UNAVAILABLE_CONDITIONS.has(condition)) return { kind: "unavailable", retry: "manual", ...context };
  if (condition && INVALID_CONDITIONS.has(condition)) return { kind: "invalid", retry: "none", ...context };
  if (errorType === "wait") return { kind: "outage", retry: "automatic", ...context };
  if (errorType === "auth") return { kind: "denial", retry: "manual", ...context };
  return { kind: "unknown", retry: "manual", ...context };
}

/**
 * Classify a rejected request. Reads the stanza error a rejection carries
 * (`condition`, `errorType`) and treats a timeout error (any error whose
 * `timedOut` property is true, or named `DiscoTimeoutError`) as no answer.
 */
export function classifyRemoteError(error: unknown): RemoteFailure {
  const context = stanzaErrorContext(error);
  return classifyRemoteFailure({
    condition: context.condition ?? null,
    errorType: context.errorType ?? null,
    timedOut: isTimeoutError(error),
  });
}

function isTimeoutError(error: unknown): boolean {
  if (typeof error !== "object" || error === null) return false;
  if ((error as { timedOut?: unknown }).timedOut === true) return true;
  return (error as { name?: unknown }).name === "DiscoTimeoutError";
}

/** User-facing description of a failure for one kind of remote subject. */
export function describeRemoteFailure(
  failure: RemoteFailure,
  subject: RemoteSubject,
): { title: string; detail: string } {
  const noun = subject === "room" ? "room" : subject === "space" ? "Space" : "server";
  switch (failure.kind) {
    case "outage":
      return {
        title: `The ${noun}'s server can't be reached`,
        detail: failure.retry === "automatic"
          ? "Its server did not answer in time. This is usually temporary; Waddle will try again."
          : "Its server could not be found. Check the address, or try again later.",
      };
    case "denial":
      return {
        title: subject === "room" ? "You don't have access to this room" : `You don't have access to this ${noun}`,
        detail: failure.condition === "registration-required"
          ? "Only members can join. Ask one of its admins to add you, then try again."
          : failure.condition === "not-authorized"
            ? "It needs a password or an invitation. Ask one of its admins, then try again."
            : "Its server refused the request. Retrying will not help until an admin there grants access.",
      };
    case "deletion":
      return {
        title: `This ${noun} isn't available`,
        detail: `Its server doesn't know this ${noun}. It may have been removed, or it is not open to accounts on other servers.`,
      };
    case "conflict":
      return {
        title: "Your name is already in use there",
        detail: "Someone in this room already uses your nickname.",
      };
    case "unavailable":
      return {
        title: `The ${noun} can't take you right now`,
        detail: subject === "room"
          ? "The room may be full, or its server does not offer this. Try again later."
          : `Its server does not offer this ${noun} to accounts on other servers.`,
      };
    case "invalid":
      return {
        title: "That request was not accepted",
        detail: "Its server could not use the address or request. Check the address and try again.",
      };
    default:
      return {
        title: `The ${noun} could not be reached`,
        detail: "Something went wrong. Try again.",
      };
  }
}
