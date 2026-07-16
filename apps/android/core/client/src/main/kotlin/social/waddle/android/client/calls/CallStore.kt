package social.waddle.android.client.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleCallSessionTerminateOutcome
import social.waddle.client.ffi.WaddleJingleReason
import java.util.UUID

/**
 * Single-slot call lifecycle store: the Kotlin port of the web
 * client's call-store.ts (`applyCallEvent` + timers + actions) and
 * call-effects.ts (the inbound-event side-effect table), driven from
 * the serialized [social.waddle.android.client.XmppEventRouter] event
 * stream.
 *
 * State transitions run synchronously under [stateLock] on the
 * router's single-consumer coroutine (reducer ordering guarantee);
 * wire sends run on the session scope through a single-consumer
 * effects queue so side-effect SENDS keep the inbound event order too
 * — the web gets both for free from its single JS thread.
 */
class CallStore internal constructor(
    private val signaling: CallSignaling,
    private val ownBareJid: () -> String?,
    private val ownFullJid: () -> String?,
    private val outgoingTimeoutMillis: Long = OUTGOING_TIMEOUT_MILLIS,
    private val sessionAcceptTimeoutMillis: Long = SESSION_ACCEPT_TIMEOUT_MILLIS,
    private val newSid: () -> String = ::newCallSid,
) {
    private val stateLock = Any()

    private val _state = MutableStateFlow<CallState>(CallState.Idle)

    /** The live call slot; the UI renders ringing/in-call surfaces off this. */
    val state: StateFlow<CallState> = _state.asStateFlow()

    private val _lastError = MutableStateFlow<String?>(null)

    /** Most recent wire-send failure; cleared on the next transition. */
    val lastError: StateFlow<String?> = _lastError.asStateFlow()

    private var scope: CoroutineScope? = null
    private var effects: Channel<suspend () -> Unit>? = null
    private var outgoingTimer: Job? = null
    private var sessionAcceptTimer: Job? = null

    /** Bind the session scope; timers and wire effects run on it. */
    internal fun start(sessionScope: CoroutineScope) {
        val queue = Channel<suspend () -> Unit>(Channel.UNLIMITED)
        synchronized(stateLock) {
            scope = sessionScope
            effects = queue
        }
        sessionScope.launch {
            for (effect in queue) effect()
        }
    }

    /** Session teardown: drop the slot, timers, and queued effects. */
    internal fun clear() {
        synchronized(stateLock) {
            cancelCallTimersLocked()
            effects?.close()
            effects = null
            scope = null
            _state.value = CallState.Idle
            _lastError.value = null
        }
    }

    /**
     * Inbound call event from the serialized router stream. Ports the
     * web's `on_call` wiring (client.ts): self-originated carbons only
     * touch the slot for the sibling-device transitions listed in
     * [selfOriginatedEventShouldTouchCurrentCall], and side effects
     * fire only for remote-originated events.
     */
    internal fun onCallEvent(event: WaddleCallEvent) {
        val selfBare = ownBareJid()?.lowercase()
        val isSelfOriginated = selfBare != null && bareJid(event.from).lowercase() == selfBare
        val prev: CallState
        synchronized(stateLock) {
            prev = _state.value
            if (!isSelfOriginated || selfOriginatedEventShouldTouchCurrentCall(prev, event)) {
                applyCallEventLocked(prev, event)
            }
        }
        if (!isSelfOriginated) {
            enqueueEffect { handleCallEventSideEffect(event, prev) }
        }
    }

    // ── Actions (dm-call-actions.ts / tearDownActiveCall ports) ─────────────

    /**
     * Originator action: snapshot the outbound `<propose/>` intent
     * into the slot, send it to the peer's bare JID, and arm the
     * auto-retract so the peer's devices don't ring forever when
     * nobody answers (web `startDmCallAction`).
     */
    suspend fun startCall(peerJid: String, media: WaddleCallMedia): Boolean {
        val peerBare = bareJid(peerJid)
        val sid = newSid()
        synchronized(stateLock) {
            val current = _state.value
            if (current !is CallState.Idle && current !is CallState.Ended) return false
            cancelCallTimersLocked()
            _state.value = CallState.Outgoing(
                to = peerBare,
                sid = sid,
                media = media,
                initiator = ownFullJid(),
            )
            _lastError.value = null
        }
        if (!signaling.propose(peerBare, sid, media)) {
            synchronized(stateLock) {
                val next = _state.value
                if (next is CallState.Outgoing && next.sid == sid) _state.value = CallState.Idle
            }
            reportError("call propose failed")
            return false
        }
        scheduleOutgoingTimeout(sid)
        return true
    }

    /**
     * Responder action: send XEP-0353 `<proceed/>` to the proposer's
     * full JID. The call turns active later, when the caller's Jingle
     * session-initiate lands (web `answerIncomingDmCallActivity`).
     */
    suspend fun acceptIncoming(): Boolean {
        val target: CallState.Incoming
        synchronized(stateLock) {
            val current = _state.value
            if (current !is CallState.Incoming || current.accepting) return false
            target = current
            _state.value = current.copy(accepting = true)
        }
        if (!signaling.proceed(target.from, target.sid)) {
            synchronized(stateLock) {
                val next = _state.value
                if (next is CallState.Incoming && next.sid == target.sid) {
                    _state.value = next.copy(accepting = false)
                }
            }
            reportError("call proceed failed")
            return false
        }
        return true
    }

    /** Responder action: `<reject/>` the ringing call and clear the slot. */
    suspend fun declineIncoming(): Boolean {
        val target: CallState.Incoming
        synchronized(stateLock) {
            val current = _state.value
            if (current !is CallState.Incoming) return false
            target = current
            _state.value = CallState.Idle
        }
        if (!signaling.reject(target.from, target.sid)) {
            reportError("call reject failed")
            return false
        }
        return true
    }

    /**
     * Best-effort teardown for the live slot (web `tearDownActiveCall`):
     * active → XEP-0166 session-terminate (outcome-aware) plus the
     * XEP-0353 `<finish/>` bookend both parties SHOULD send so MAM
     * archives stay consistent; outgoing → `<retract/>`; incoming →
     * `<reject/>`.
     */
    suspend fun hangUp(reason: WaddleJingleReason = WaddleJingleReason.SUCCESS) {
        val current: CallState
        synchronized(stateLock) {
            cancelCallTimersLocked()
            current = _state.value
            _state.value = CallState.Idle
        }
        when (current) {
            is CallState.Active -> terminateActive(current, reason)
            is CallState.Outgoing ->
                if (!signaling.retract(bareJid(current.to), current.sid)) reportError("call retract failed")
            is CallState.Incoming ->
                if (!signaling.reject(current.from, current.sid)) reportError("call reject failed")
            else -> Unit
        }
    }

    /** Dismiss the `Ended` slot (or abandon any local state) → `Idle`. */
    fun dismiss() {
        synchronized(stateLock) {
            cancelCallTimersLocked()
            _state.value = CallState.Idle
            _lastError.value = null
        }
    }

    /** XEP-0215 TURN/STUN advertisement for the media layer's RTC config. */
    suspend fun fetchExternalServices() = signaling.fetchExternalServices()

    private suspend fun terminateActive(current: CallState.Active, reason: WaddleJingleReason) {
        val outcome = signaling.sessionTerminateWithOutcome(current.peer, current.sid, reason)
        if (outcome == WaddleCallSessionTerminateOutcome.ERROR) {
            reportError("call session terminate failed")
            return
        }
        // XEP-0353: both parties SHOULD send <finish/> after the call
        // ends. A classified ORPHANED terminate means the server already
        // lost the call registry, so only the message-level bookend can
        // still route (web parity).
        if (!signaling.finish(current.peer, current.sid)) {
            reportError("call finish failed")
        }
    }

    // ── applyCallEvent port ──────────────────────────────────────────────────

    private fun applyCallEventLocked(before: CallState, event: WaddleCallEvent) {
        val next = reduceCallState(before, event)
        if (next != before) {
            _lastError.value = null
            // Any transition out of outgoing cancels the auto-retract timer.
            if (before is CallState.Outgoing && next !is CallState.Outgoing) {
                cancelCallTimersLocked()
            }
        }
        if (before is CallState.Outgoing &&
            event.kind is WaddleCallEventKind.Proceed &&
            event.sid == before.sid
        ) {
            cancelOutgoingTimeoutLocked()
        }
        _state.value = next
        emitRingingOnIncomingPropose(before, next, event)
    }

    /**
     * XEP-0353 §3.2: answer a fresh incoming `<propose/>` with a
     * `<ringing/>` to the caller's BARE JID so every caller resource
     * sees the device-ring state (web `emitRingingOnIncomingPropose`).
     */
    private fun emitRingingOnIncomingPropose(before: CallState, next: CallState, event: WaddleCallEvent) {
        if (event.kind !is WaddleCallEventKind.Propose) return
        if (next !is CallState.Incoming || next.sid != event.sid) return
        if (before is CallState.Incoming && before.sid == event.sid) return
        enqueueEffect {
            if (!signaling.ringing(bareJid(event.from), event.sid)) reportError("call ringing failed")
        }
    }

    /**
     * Which SELF-ORIGINATED carbons may touch the live slot — the
     * sibling-device transitions (answered/declined/ended elsewhere)
     * from the web's on_call wiring. Everything else from our own bare
     * JID is an echo of something this device already did and MUST NOT
     * re-run transitions (or the slot flaps).
     */
    private fun selfOriginatedEventShouldTouchCurrentCall(prev: CallState, event: WaddleCallEvent): Boolean {
        val prevSid = prev.sidOrNull ?: return false
        if (prevSid != event.sid) return false
        return when (event.kind) {
            is WaddleCallEventKind.Propose -> false
            is WaddleCallEventKind.Proceed -> true
            is WaddleCallEventKind.Reject -> prev is CallState.Incoming
            is WaddleCallEventKind.SessionInitiate -> prev is CallState.Incoming
            is WaddleCallEventKind.SessionAccept -> prev is CallState.Outgoing
            is WaddleCallEventKind.SessionTerminate -> prev is CallState.Active
            is WaddleCallEventKind.Finish -> prev is CallState.Active
            else -> false
        }
    }

    // ── call-effects.ts port ─────────────────────────────────────────────────

    /**
     * Side effects for a REMOTE-originated event, invoked after the
     * reducer updated the slot. `prev` is the snapshot BEFORE the
     * reducer applied the event (web `handleCallEventSideEffect`).
     */
    private suspend fun handleCallEventSideEffect(event: WaddleCallEvent, prev: CallState) {
        when (val kind = event.kind) {
            is WaddleCallEventKind.Propose -> proposeSideEffect(event, kind, prev)
            is WaddleCallEventKind.Proceed -> proceedSideEffect(event, prev)
            is WaddleCallEventKind.SessionInitiate -> sessionInitiateSideEffect(event, kind, prev)
            else -> Unit
        }
    }

    private suspend fun proposeSideEffect(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.Propose,
        prev: CallState,
    ) {
        // Simultaneous propose tie-break (XEP-0353 §Tie Breaking,
        // anchor tie-break-1). Specific to both parties proposing to
        // the same bare peer at once; unrelated callers take the
        // busy-reject path below.
        if (prev is CallState.Outgoing && isSameCallBareJid(event.from, prev.to)) {
            tieBreakAgainstOutgoing(event, kind, prev)
            return
        }
        // Existing-session migration (XEP-0353 anchor tie-break-2): the
        // peer re-proposed while we hold an active session with them.
        if (prev is CallState.Active && isSameCallBareJid(event.from, prev.peer)) {
            migrateActiveToIncoming(event, kind, prev)
            return
        }
        // Busy-reject: a propose while mid-call is silently dropped by
        // the reducer; without an explicit reject the proposer rings
        // until timeout. XEP-0353 defines no <busy/> child — plain
        // reject to the proposer's full JID is the conformant signal.
        if (prev !is CallState.Idle && prev !is CallState.Ended) {
            if (!signaling.reject(event.from, event.sid)) reportError("call reject failed")
        }
    }

    private suspend fun tieBreakAgainstOutgoing(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.Propose,
        prev: CallState.Outgoing,
    ) {
        val ourJid = ownFullJid() ?: return
        if (incomingProposeWinsTieBreak(event.sid, prev.sid, event.from, ourJid)) {
            // XEP-0353 tie-break-1: the receiver of the LOWER-sid propose
            // retracts its own higher sid with <tie-break/> + <expired/>,
            // then treats the incoming propose normally.
            if (signaling.retractTieBreak(event.from, prev.sid)) {
                acceptIncomingTieBreakPropose(event, kind.media)
            } else {
                reportError("call tie-break retract failed")
            }
        } else {
            // The receiver of the HIGHER-sid propose rejects it with
            // <tie-break/> + <expired/>; our own outgoing ring continues.
            if (!signaling.rejectTieBreak(event.from, event.sid)) {
                reportError("call tie-break reject failed")
            }
        }
    }

    private suspend fun migrateActiveToIncoming(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.Propose,
        prev: CallState.Active,
    ) {
        val migrated = signaling.finishMigrated(event.from, prev.sid, event.sid) &&
            signaling.proceed(event.from, event.sid)
        if (!migrated) {
            failCall(prev.sid, "call migration failed")
            return
        }
        acceptIncomingTieBreakPropose(event, kind.media)
        // The old Jingle session may already be orphaned on another
        // resource; don't let its IQ round-trip block the XEP-0353
        // migration markers that keep both users' devices in sync.
        scope?.launch {
            if (!signaling.sessionTerminate(prev.peer, prev.sid, WaddleJingleReason.EXPIRED)) {
                reportError("call session terminate failed")
            }
        }
    }

    private suspend fun proceedSideEffect(event: WaddleCallEvent, prev: CallState) {
        if (prev !is CallState.Outgoing || prev.sid != event.sid) return
        // `event.from` is the responder's full JID (XEP-0353 §0.6); the
        // initiator attribute names our own resource (XEP-0166 §7.1).
        val ourJid = ownFullJid()
        if (ourJid == null) {
            failCall(event.sid, "call session initiate failed: no bound resource")
            return
        }
        if (signaling.sessionInitiate(event.from, ourJid, event.sid, prev.media)) {
            scheduleSessionAcceptTimeout(event.from, event.sid)
        } else {
            failCall(event.sid, "call session initiate failed")
        }
    }

    private suspend fun sessionInitiateSideEffect(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.SessionInitiate,
        prev: CallState,
    ) {
        if (prev !is CallState.Incoming || prev.sid != event.sid) return
        // The responder confirms the Jingle session per XEP-0166 §6.2 —
        // without this the CALLER never gets a populated transport
        // rewrite and never joins the LiveKit room.
        val ourJid = ownFullJid()
        if (ourJid == null) {
            failCall(event.sid, "call session accept failed: no bound resource")
            return
        }
        if (!signaling.sessionAccept(event.from, ourJid, event.sid, kind.media)) {
            failCall(event.sid, "call session accept failed")
        }
    }

    /** Replace the slot with the tie-break winner's incoming ring. */
    private fun acceptIncomingTieBreakPropose(event: WaddleCallEvent, media: WaddleCallMedia) {
        synchronized(stateLock) {
            cancelCallTimersLocked()
            _state.value = CallState.Incoming(from = event.from, sid = event.sid, media = media)
            _lastError.value = null
        }
    }

    private fun failCall(sid: String, message: String) {
        synchronized(stateLock) {
            cancelCallTimersLocked()
            _state.value = CallState.Ended(sid = sid, reason = CallEndReason.Error)
        }
        reportError(message)
    }

    // ── Timers ───────────────────────────────────────────────────────────────

    /**
     * Auto-retract for the most recent [startCall]: fires only if the
     * slot is still `Outgoing` with the same sid, so any later
     * transition makes the timeout a no-op (web `scheduleOutgoingTimeout`).
     */
    private fun scheduleOutgoingTimeout(sid: String) {
        synchronized(stateLock) {
            outgoingTimer?.cancel()
            outgoingTimer = scope?.launch {
                delay(outgoingTimeoutMillis)
                timeOutOutgoing(sid)
            }
        }
    }

    private suspend fun timeOutOutgoing(sid: String) {
        val target: CallState.Outgoing
        synchronized(stateLock) {
            val current = _state.value
            if (current !is CallState.Outgoing || current.sid != sid) return
            target = current
            _state.value = CallState.Ended(sid = sid, reason = CallEndReason.Timeout)
        }
        if (!signaling.retract(bareJid(target.to), sid)) reportError("call retract failed")
    }

    /**
     * After the responder's `<proceed/>` cancelled the ring timeout,
     * the call still isn't active until XEP-0166 session-accept
     * arrives. This second timeout covers a session-initiate whose
     * accept never comes back — e.g. the server forwarded it without
     * the LiveKit transport rewrite, in which case NO event ever fires
     * (web `scheduleSessionAcceptTimeout`).
     */
    private fun scheduleSessionAcceptTimeout(peerFullJid: String, sid: String) {
        synchronized(stateLock) {
            sessionAcceptTimer?.cancel()
            sessionAcceptTimer = scope?.launch {
                delay(sessionAcceptTimeoutMillis)
                timeOutSessionAccept(peerFullJid, sid)
            }
        }
    }

    private suspend fun timeOutSessionAccept(peerFullJid: String, sid: String) {
        synchronized(stateLock) {
            val current = _state.value
            if (current !is CallState.Outgoing || current.sid != sid) return
            _state.value = CallState.Ended(sid = sid, reason = CallEndReason.Timeout)
        }
        if (!signaling.sessionTerminate(peerFullJid, sid, WaddleJingleReason.TIMEOUT)) {
            reportError("call session terminate failed")
        }
    }

    private fun cancelOutgoingTimeoutLocked() {
        outgoingTimer?.cancel()
        outgoingTimer = null
    }

    private fun cancelCallTimersLocked() {
        cancelOutgoingTimeoutLocked()
        sessionAcceptTimer?.cancel()
        sessionAcceptTimer = null
    }

    // ── Plumbing ─────────────────────────────────────────────────────────────

    private fun enqueueEffect(effect: suspend () -> Unit) {
        effects?.trySend(effect)
    }

    private fun reportError(message: String) {
        _lastError.value = message
    }

    companion object {
        /**
         * Outgoing-ring auto-retract (web `OUTGOING_TIMEOUT_MS`): after
         * this elapses with the slot still `Outgoing`, retract so the
         * peer's devices stop ringing when nobody answers.
         */
        const val OUTGOING_TIMEOUT_MILLIS = 45_000L

        /** Session-accept gap timeout (web `SESSION_ACCEPT_TIMEOUT_MS`). */
        const val SESSION_ACCEPT_TIMEOUT_MILLIS = 45_000L
    }
}

/**
 * Fresh Jingle session id (web `newCallSid` parity). The server
 * namespaces sids by the caller's bare JID before scoping a LiveKit
 * room, so cross-user collisions are impossible by construction.
 */
internal fun newCallSid(): String =
    "c" + UUID.randomUUID().toString().replace("-", "").take(SID_RANDOM_LENGTH)

private const val SID_RANDOM_LENGTH = 16
