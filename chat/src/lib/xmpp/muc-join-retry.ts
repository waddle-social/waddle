import type { StanzaErrorContext } from "./stanza-error-context";
import type { XmppStanzaErrorType } from "./types";

/**
 * A server-rejected XEP-0045 room join.
 *
 * The stanza context stays attached to the thrown error so callers can
 * distinguish an RFC 6120 `wait` error from terminal auth/cancel/modify
 * failures instead of relying on user-facing message text.
 */
export class MucJoinRejectedError extends Error {
  readonly condition?: string;
  readonly errorType?: XmppStanzaErrorType;
  readonly text?: string;

  constructor(context: StanzaErrorContext) {
    super(
      context.errorType === "wait"
        ? "Channel presence is temporarily unavailable. Try again in a moment."
        : "Channel presence was rejected.",
    );
    this.name = "MucJoinRejectedError";
    this.condition = context.condition;
    this.errorType = context.errorType;
    this.text = context.errorText;
  }
}

/**
 * The room join produced no terminal stanza and no XEP-0045 status-110
 * self-presence before the attempt deadline.
 *
 * Ordered-relay joins deliberately suppress a local error when the remote
 * owner may already have committed the occupancy. Keeping this outcome typed
 * lets the caller retry/resynchronize that ambiguous commit without treating
 * unrelated transport and lifecycle failures as retryable.
 */
export class MucJoinSelfPresenceTimeoutError extends Error {
  constructor() {
    super("Channel presence did not finish syncing. Try again in a moment.");
    this.name = "MucJoinSelfPresenceTimeoutError";
  }
}

/**
 * Bounded retries after the initial join attempt. The cumulative schedule
 * deliberately includes an attempt after the server's five-second pending
 * room-ownership reconciliation tick. Fixed delays keep the policy
 * deterministic in production and make the state machine testable without
 * random-number seams.
 */
export const MUC_JOIN_RETRY_DELAYS_MS = [250, 750, 1_500, 3_000, 6_000] as const;
