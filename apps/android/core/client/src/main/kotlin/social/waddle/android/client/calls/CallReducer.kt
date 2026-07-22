package social.waddle.android.client.calls

import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind

/**
 * Pure reducer over the current call slot — the literal Kotlin port of
 * the web client's `reduceCallState` (chat/src/lib/calls/call-store.ts).
 *
 * Every session-* and JMI-ack event is guarded on the current phase
 * and a matching `sid`. A stale stanza (server replay, delayed
 * delivery, or a malicious peer) MUST NOT clobber a live call slot, so
 * unmatched events are dropped rather than collapsing to `Ended`.
 */
internal fun reduceCallState(current: CallState, event: WaddleCallEvent): CallState =
    when (val kind = event.kind) {
        is WaddleCallEventKind.Propose -> reducePropose(current, event, kind)
        is WaddleCallEventKind.Ringing -> reduceRinging(current, event)
        is WaddleCallEventKind.Proceed -> reduceProceed(current, event)
        is WaddleCallEventKind.Reject ->
            reduceAbort(current, event, tieBreakExpired(kind.tieBreak, kind.reason), CallEndReason.Rejected)
        is WaddleCallEventKind.Retract ->
            reduceAbort(current, event, tieBreakExpired(kind.tieBreak, kind.reason), CallEndReason.Retracted)
        is WaddleCallEventKind.SessionInitiate -> reduceSessionInitiate(current, event, kind)
        is WaddleCallEventKind.SessionAccept -> reduceSessionAccept(current, event, kind)
        is WaddleCallEventKind.Finish -> reduceEnd(current, event, kind.reason)
        is WaddleCallEventKind.SessionTerminate -> reduceEnd(current, event, kind.reason)
    }

/**
 * The remote peer is ringing us — surface as incoming so the UI can
 * offer accept/reject. If we're already in a call, ignore; the
 * side-effect handler emits `<reject/>` so the proposer stops ringing.
 */
private fun reducePropose(
    current: CallState,
    event: WaddleCallEvent,
    kind: WaddleCallEventKind.Propose,
): CallState =
    if (current is CallState.Idle || current is CallState.Ended) {
        CallState.Incoming(from = event.from, sid = event.sid, media = kind.media)
    } else {
        current
    }

private fun reduceRinging(current: CallState, event: WaddleCallEvent): CallState =
    if (current is CallState.Outgoing &&
        current.sid == event.sid &&
        bareJid(event.from).lowercase() == bareJid(current.to).lowercase()
    ) {
        current.copy(ringing = true)
    } else {
        current
    }

/**
 * A `<proceed/>` while WE are the ringing responder means a sibling
 * resource of ours answered (self-originated carbon) — stop ringing
 * here. When we're the caller (`Outgoing`), the reducer keeps the
 * phase; the side-effect handler reads `proceed.from` and emits the
 * session-initiate.
 */
private fun reduceProceed(current: CallState, event: WaddleCallEvent): CallState =
    if (current is CallState.Incoming && current.sid == event.sid) CallState.Idle else current

/**
 * XEP-0353 abort (`<reject/>` / `<retract/>`): only for the live slot.
 * MUC group-call phases are engine-owned — ending them here would skip
 * the leave presence, strand the preparation waiters, and retain the
 * session cache; the store's MUC end effect runs the teardown instead.
 */
private fun reduceAbort(
    current: CallState,
    event: WaddleCallEvent,
    expired: Boolean,
    kindReason: CallEndReason,
): CallState =
    if (current !is CallState.Idle && !current.isMucCallPhase && current.sidOrNull == event.sid) {
        CallState.Ended(sid = event.sid, reason = if (expired) CallEndReason.Expired else kindReason)
    } else {
        current
    }

internal fun tieBreakExpired(tieBreak: Boolean, reason: social.waddle.client.ffi.WaddleJingleReason?): Boolean =
    tieBreak && reason == social.waddle.client.ffi.WaddleJingleReason.EXPIRED

/**
 * Responder receives the Jingle session-initiate after accepting via
 * JMI proceed. Only honored while we're the matching incoming call.
 */
private fun reduceSessionInitiate(
    current: CallState,
    event: WaddleCallEvent,
    kind: WaddleCallEventKind.SessionInitiate,
): CallState {
    if (current !is CallState.Incoming || current.sid != event.sid) return current
    return CallState.Active(
        peer = event.from,
        sid = event.sid,
        media = kind.media,
        join = kind.join,
        initiator = event.from,
    )
}

/**
 * Initiator receives the Jingle session-accept after the server
 * rewrote the transport with the initiator's own LiveKit join token.
 * Only honored while we're the matching outgoing call.
 */
private fun reduceSessionAccept(
    current: CallState,
    event: WaddleCallEvent,
    kind: WaddleCallEventKind.SessionAccept,
): CallState {
    if (current !is CallState.Outgoing || current.sid != event.sid) return current
    return CallState.Active(
        peer = event.from,
        sid = event.sid,
        media = kind.media,
        join = kind.join,
        initiator = current.initiator,
    )
}

/**
 * Either side hung up; a stale terminate for another sid is a no-op.
 * MUC phases stay untouched (see [reduceAbort]) — the store's MUC end
 * effect owns their teardown.
 */
private fun reduceEnd(
    current: CallState,
    event: WaddleCallEvent,
    reason: social.waddle.client.ffi.WaddleJingleReason?,
): CallState =
    if (current !is CallState.Idle && !current.isMucCallPhase && current.sidOrNull == event.sid) {
        CallState.Ended(sid = event.sid, reason = CallEndReason.Finished(reason))
    } else {
        current
    }
