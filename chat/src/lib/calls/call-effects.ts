import { outboundCalls, type CallWireSender } from "./outbound";
import {
  acceptIncomingTieBreakPropose,
  failCallState,
  reportCallError,
  scheduleSessionAcceptTimeout,
} from "./call-store";
import {
  incomingProposeWinsTieBreak,
  isSameCallBareJid,
} from "./tie-break";
import type { CallEvent, CallState } from "./types";
import { dmCallRoomName } from "./call-correlation";
import { barePeerJid } from "../xmpp/jid";

/** The 1:1 LiveKit room name for an inbound propose: the proposer is
 *  the Jingle initiator, so `{proposer_bare}::{sid}` matches the
 *  server-side `scoped_call_id` (#1452 correlation). */
function dmRoomNameFromEvent(event: { from: string; sid: string }): string {
  return dmCallRoomName(barePeerJid(event.from) || event.from, event.sid);
}
import {
  reportDeclinedCallAttempt,
  reportFailedCallAttempt,
} from "./call-lifecycle-telemetry";

/**
 * Side-effect handler invoked by the chat-side `on_call` wrapper
 * AFTER the reducer has updated `$callState`. Keeps the reducer
 * itself pure: every wire send driven by an inbound event lives
 * here.
 *
 * Responsibilities:
 * 1. On inbound `<proceed/>` while the call is `outgoing` with a
 *    matching `sid`, fire Jingle `<session-initiate/>` to the
 *    responder's full JID stamped on `proceed.from` per XEP-0353
 *    §0.6.
 * 2. On inbound `<session-initiate/>` while the call is `incoming`
 *    with a matching `sid` — i.e. we accepted, the caller emitted
 *    session-initiate, we received it — fire Jingle
 *    `<session-accept/>` back per XEP-0166 §6.2. Without this the
 *    *caller* never gets a populated transport rewrite and never
 *    joins the LiveKit room.
 * 3. On inbound `<propose/>` while we're already in a non-idle
 *    phase, send a `<reject/>` so the caller stops ringing instead
 *    of timing out. XEP-0353 doesn't define a `<busy/>` child, so
 *    plain reject is the conformant signal.
 *
 * `prev` is the snapshot of the store BEFORE `reduceCallState`
 * applied the event. Reading `prev` makes the dependency on
 * pre-event state explicit and survives reducer changes that fold
 * additional transitions into the same kind.
 */
export async function handleCallEventSideEffect(
  event: CallEvent,
  prev: CallState,
  sender: CallWireSender,
  ourFullJid: string,
): Promise<void> {
  // Simultaneous propose tie-break (XEP-0353 §Tie Breaking). This
  // is specific to both resources proposing a call to the same bare
  // peer at the same time; unrelated callers still take the normal
  // busy-reject path below.
  if (
    event.kind === "propose" &&
    prev.phase === "outgoing" &&
    isSameCallBareJid(event.from, prev.to)
  ) {
    try {
      if (
        incomingProposeWinsTieBreak(
          event.sid,
          prev.sid,
          event.from,
          ourFullJid,
        )
      ) {
        await outboundCalls.retractTieBreak(sender, event.from, prev.sid);
        acceptIncomingTieBreakPropose(event);
      } else {
        reportDeclinedCallAttempt(event.sid, "dm", dmRoomNameFromEvent(event));
        await outboundCalls.rejectTieBreak(sender, event.from, event.sid);
      }
    } catch (err) {
      reportFailedCallAttempt(event.sid, "dm", dmRoomNameFromEvent(event));
      reportCallError(err);
    }
    return;
  }

  if (
    event.kind === "propose" &&
    prev.phase === "active" &&
    prev.kind === "dm" &&
    isSameCallBareJid(event.from, prev.peer)
  ) {
    try {
      await outboundCalls.finishMigrated(sender, event.from, prev.sid, event.sid);
      await outboundCalls.proceed(sender, event.from, event.sid);
      acceptIncomingTieBreakPropose(event);
      // The old Jingle session may already be orphaned on another
      // resource, so do not let its IQ round-trip block the XEP-0353
      // migration markers that keep both users' devices in sync.
      void outboundCalls
        .sessionTerminate(sender, prev.peer, prev.sid, "expired")
        .catch((err) => reportCallError(err));
    } catch (err) {
      reportFailedCallAttempt(event.sid, "dm", dmRoomNameFromEvent(event));
      failCallState(err, prev.sid);
    }
    return;
  }

  // Busy-reject: a propose arriving while we're mid-call (incoming
  // or active, plus outgoing calls against a different peer) is
  // silently dropped by the reducer. Without an explicit reject the
  // proposer sees no response and rings until their timeout, so
  // respond with reject addressed to their full JID (the `from` on
  // inbound propose is the proposer's full JID because the server
  // stamps it).
  if (
    event.kind === "propose" &&
    prev.phase !== "idle" &&
    prev.phase !== "ended"
  ) {
    reportDeclinedCallAttempt(event.sid, "dm", dmRoomNameFromEvent(event));
    try {
      await outboundCalls.reject(sender, event.from, event.sid);
    } catch (err) {
      reportCallError(err);
    }
    return;
  }

  if (event.kind === "proceed") {
    if (prev.phase !== "outgoing") return;
    if (prev.sid !== event.sid) return;
    // `event.from` is the responder's full JID (XEP-0353 §0.6).
    try {
      await outboundCalls.sessionInitiate(
        sender,
        event.from,
        ourFullJid,
        event.sid,
        prev.media,
      );
      scheduleSessionAcceptTimeout(sender, event.from, event.sid);
    } catch (err) {
      failCallState(err, event.sid);
    }
    return;
  }

  if (event.kind === "session-initiate") {
    if (prev.phase !== "incoming") return;
    if (prev.sid !== event.sid) return;
    // The responder confirms the Jingle session per XEP-0166 §6.2.
    // `event.from` is the initiator's full JID (the server stamps
    // the authenticated session there, and the wire-stamped
    // `initiator` attribute mirrors it). `ourFullJid` is our
    // resource — the JWT identity in the inbound session-initiate
    // already targets this resource, so the responder attribute
    // must match.
    try {
      await outboundCalls.sessionAccept(
        sender,
        event.from,
        ourFullJid,
        event.sid,
        event.media,
      );
    } catch (err) {
      failCallState(err, event.sid);
    }
    return;
  }
}
