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
import social.waddle.client.ffi.WaddlePresence
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
    mucSessionCache: MucCallSessionCache,
    private val outgoingTimeoutMillis: Long = OUTGOING_TIMEOUT_MILLIS,
    private val sessionAcceptTimeoutMillis: Long = SESSION_ACCEPT_TIMEOUT_MILLIS,
    private val newSid: () -> String = ::newCallSid,
) {
    private val stateLock = Any()

    /** XEP-0272 Muji presence bookkeeping (participants/owners/media). */
    val mucCallPresence: MucCallPresence = MucCallPresence()

    /** The MUC group-call flow sharing this store's single call slot. */
    val muc: MucCallEngine =
        MucCallEngine(this, signaling, mucCallPresence, mucSessionCache, ownFullJid, newSid)

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

    /**
     * Sids of Reject/Retract events the reducer could not apply (the
     * slot held a different call). The web never needs this: its
     * effects run inline, so a propose is always answered before the
     * next event reduces. Here the reducer can outpace the queued
     * effects, so a propose's abort may already have arrived by the
     * time its effect runs — such a sid is DEAD and must not be rung,
     * answered, or migrated to. Bounded LRU; guarded by [stateLock].
     */
    private val recentlyAbortedSids = object : LinkedHashMap<String, Unit>() {
        override fun removeEldestEntry(eldest: Map.Entry<String, Unit>): Boolean =
            size > ABORTED_SID_CAPACITY
    }

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
            recentlyAbortedSids.clear()
            _state.value = CallState.Idle
            _lastError.value = null
        }
        muc.clear()
        mucCallPresence.clear()
    }

    /** Inbound presence fan-out from the router: XEP-0272 Muji bookkeeping. */
    internal fun onPresence(presence: WaddlePresence) {
        mucCallPresence.applyMucCallPresence(presence)
    }

    /**
     * Inbound call event from the serialized router stream. Ports the
     * web's `on_call` wiring (client.ts): self-originated carbons only
     * touch the slot for the sibling-device transitions listed in
     * [selfOriginatedEventShouldTouchCurrentCall], and side effects
     * fire only for remote-originated events.
     */
    internal fun onCallEvent(event: WaddleCallEvent) {
        // A Muji session-accept is owned by the pending `muc.begin`
        // resolver, never the 1:1 reducer — the reducer would
        // mis-interpret it as a peer accepting a JMI ring.
        if (muc.tryFulfillAccept(event)) return
        val selfBare = ownBareJid()?.lowercase()
        val isSelfOriginated = selfBare != null && bareJid(event.from).lowercase() == selfBare
        val prev: CallState
        synchronized(stateLock) {
            prev = _state.value
            val kind = event.kind
            if ((kind is WaddleCallEventKind.Reject || kind is WaddleCallEventKind.Retract) &&
                prev.sidOrNull != event.sid
            ) {
                recentlyAbortedSids[event.sid] = Unit
            }
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
        // Arm the auto-retract only if the slot is STILL this ring: a
        // reject/hang-up that landed while the propose was in flight
        // already cancelled the timers, and re-arming would leave a
        // stale 45s job alive.
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
        // The caller side bounds the proceed→session-accept gap with
        // scheduleSessionAcceptTimeout; without a mirror here a caller
        // that dies right after our <proceed/> leaves the responder in
        // `Incoming(accepting)` forever — which now also holds the
        // phone-call foreground service.
        scheduleSessionInitiateTimeout(target.from, target.sid)
        return true
    }

    /**
     * Responder action: decline the ringing call and clear the slot —
     * `<reject/>` for an unanswered ring; once our `<proceed/>` is out
     * (accepting), the conformant abandon verb is `<finish/>` with
     * `<cancel/>` (a late reject would contradict the proceed).
     */
    suspend fun declineIncoming(): Boolean {
        val target: CallState.Incoming
        synchronized(stateLock) {
            val current = _state.value
            if (current !is CallState.Incoming) return false
            cancelCallTimersLocked()
            target = current
            _state.value = CallState.Idle
        }
        val answered = if (target.accepting) {
            signaling.finishWithReason(target.from, target.sid, WaddleJingleReason.CANCEL)
        } else {
            signaling.reject(target.from, target.sid)
        }
        if (!answered) {
            reportError(if (target.accepting) "call finish failed" else "call reject failed")
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
            is CallState.Active ->
                if (current.kind == CallKind.MUC) muc.teardownActive(current) else terminateActive(current, reason)
            is CallState.MucPending -> muc.teardownPending(current)
            is CallState.Outgoing ->
                if (!signaling.retract(bareJid(current.to), current.sid)) reportError("call retract failed")
            is CallState.Incoming ->
                if (current.accepting) {
                    // Our <proceed/> is already out — abandon with the
                    // finish bookend (<cancel/>), never a late reject.
                    if (!signaling.finishWithReason(current.from, current.sid, WaddleJingleReason.CANCEL)) {
                        reportError("call finish failed")
                    }
                } else if (!signaling.reject(current.from, current.sid)) {
                    reportError("call reject failed")
                }
            else -> Unit
        }
    }

    /**
     * Scoped teardown for NON-user callers (the media controller): acts
     * only if the slot still holds `sid` as the ACTIVE call — the media
     * plane can only speak for the session it was connected to, and by
     * the time its verdict lands the slot may already belong to a
     * migration ring, an Ended banner, or a fresh call. Plain [hangUp]
     * stays reserved for explicit user intent.
     */
    suspend fun hangUpActiveIf(sid: String, reason: WaddleJingleReason) {
        val current: CallState.Active
        synchronized(stateLock) {
            val state = _state.value
            if (state !is CallState.Active || state.sid != sid) return
            cancelCallTimersLocked()
            current = state
            _state.value = CallState.Idle
        }
        if (current.kind == CallKind.MUC) muc.teardownActive(current) else terminateActive(current, reason)
    }

    /**
     * Dismiss the `Ended` banner → `Idle`. Phase-guarded: a Close tap
     * racing a state transition (e.g. the migration takeover swapping
     * `Ended` for an accepting ring whose `<proceed/>` is already on
     * the wire) must not silently kill a live call — live phases end
     * only through the wire-answering actions.
     */
    fun dismiss() {
        synchronized(stateLock) {
            if (_state.value !is CallState.Ended) return
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
            if (leftTimerOwningPhase(before, next)) {
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
            // The DM reducer leaves MUC phases untouched: a matching
            // remote end event runs the engine-owned teardown instead.
            is WaddleCallEventKind.Reject -> muc.endFromRemote(
                event.sid,
                if (tieBreakExpired(kind.tieBreak, kind.reason)) CallEndReason.Expired else CallEndReason.Rejected,
            )
            is WaddleCallEventKind.Retract -> muc.endFromRemote(
                event.sid,
                if (tieBreakExpired(kind.tieBreak, kind.reason)) CallEndReason.Expired else CallEndReason.Retracted,
            )
            is WaddleCallEventKind.Finish -> muc.endFromRemote(event.sid, CallEndReason.Finished(kind.reason))
            is WaddleCallEventKind.SessionTerminate ->
                muc.endFromRemote(event.sid, CallEndReason.Finished(kind.reason))
            else -> Unit
        }
    }

    private suspend fun proposeSideEffect(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.Propose,
        prev: CallState,
    ) {
        // The peer's own reject/retract for this sid already arrived
        // while the effect was queued: the propose is concluded on the
        // wire — ringing, answering, or migrating to it would resurrect
        // a dead session (unbounded ghost ring).
        if (isRecentlyAborted(event.sid)) return
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
        // The effect queue runs after the reducer pass that captured
        // `prev`; a slot that moved before this effect ran still owes
        // the peer's propose an answer (XEP-0353 tolerates no silent
        // drop — the proposer rings until timeout).
        if (!slotStillMatches(prev)) {
            answerProposeAfterTieBreakSlotMoved(event, kind, prev)
            return
        }
        val ourJid = ownFullJid()
        if (ourJid == null) {
            tieBreakWithoutBoundResource(event, prev)
            return
        }
        if (incomingProposeWinsTieBreak(event.sid, prev.sid, event.from, ourJid)) {
            takeOverWonTieBreak(event, kind, prev)
        } else {
            // The receiver of the HIGHER-sid propose rejects it with
            // <tie-break/> + <expired/>; our own outgoing ring continues.
            if (!signaling.rejectTieBreak(event.from, event.sid)) {
                reportError("call tie-break reject failed")
            }
        }
    }

    /**
     * XEP-0353 tie-break-1, incoming propose wins: retract our own
     * higher sid with `<tie-break/>` + `<expired/>`, then treat the
     * incoming propose normally. If the swap is refused, our sid is
     * retracted on the wire either way: end the local ring if it still
     * holds it (the propose died mid-retract), then answer per cause —
     * [answerProposeAfterTieBreakSlotMoved] rings the winner when the
     * peer's tie-break reject retired our sid mid-send, stays silent
     * after the propose's own abort, and declines after a local
     * hang-up.
     */
    private suspend fun takeOverWonTieBreak(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.Propose,
        prev: CallState.Outgoing,
    ) {
        if (!signaling.retractTieBreak(event.from, prev.sid)) {
            reportError("call tie-break retract failed")
            return
        }
        if (acceptIncomingTieBreakPropose(event, kind.media, expectedSlot = prev)) return
        endRetractedOutgoing(prev)
        answerProposeAfterTieBreakSlotMoved(event, kind, prev)
    }

    /**
     * The slot moved between the reducer pass and the queued tie-break
     * effect — the CAUSE decides the answer. If the peer's tie-break
     * reject already retired our sid (Ended + Expired), their propose
     * is the tie-break WINNER and must ring — the web gets this
     * ordering for free by running effects inline. Anything else
     * (local hang-up/dismiss, a newer call) means we walked away:
     * decline theirs like the busy path.
     */
    private enum class RefusedMigration { PROPOSE_DIED, OLD_CONCLUDED_REMOTELY, WALKED_AWAY }

    /**
     * The migration swap was refused after `finishMigrated` + `proceed`
     * went out — the CAUSE decides the follow-up:
     * - The peer's own retract killed the re-propose (dead sid, slot
     *   still Active): nothing to send for the new sid, but
     *   `finishMigrated` already retired the OLD one on the wire, so
     *   the old Jingle session terminates (launched, not awaited — it
     *   is likely orphaned and must not stall the effects queue) and
     *   the slot ends (Active has no timer; stranding it would pin the
     *   FGS and mic forever).
     * - The OLD session concluded remotely mid-migration (peer's other
     *   resource terminated it; slot Ended with the old sid): the
     *   re-propose is live and already PROCEEDED — take it over as the
     *   accepting ring instead of killing both calls; nothing further
     *   is needed for the old sid.
     * - Otherwise a racing local hang-up (or a newer call) walked away:
     *   the freshly-proceeded new sid is abandoned with the finish
     *   bookend — a reject after our own proceed would be a
     *   contradictory double answer.
     */
    private suspend fun abandonRefusedMigration(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.Propose,
        prev: CallState.Active,
    ) {
        val outcome = synchronized(stateLock) {
            val current = _state.value
            when {
                recentlyAbortedSids.containsKey(event.sid) -> {
                    if (slotStillMatches(prev)) {
                        cancelCallTimersLocked()
                        _state.value = CallState.Ended(sid = prev.sid, reason = CallEndReason.Expired)
                        RefusedMigration.PROPOSE_DIED
                    } else {
                        RefusedMigration.WALKED_AWAY
                    }
                }
                current is CallState.Ended && current.sid == prev.sid -> {
                    cancelCallTimersLocked()
                    _state.value = CallState.Incoming(
                        from = event.from,
                        sid = event.sid,
                        media = kind.media,
                        accepting = true,
                    )
                    _lastError.value = null
                    RefusedMigration.OLD_CONCLUDED_REMOTELY
                }
                else -> RefusedMigration.WALKED_AWAY
            }
        }
        when (outcome) {
            RefusedMigration.PROPOSE_DIED -> scope?.launch {
                if (!signaling.sessionTerminate(prev.peer, prev.sid, WaddleJingleReason.EXPIRED)) {
                    reportError("call session terminate failed")
                }
            }
            RefusedMigration.OLD_CONCLUDED_REMOTELY ->
                scheduleSessionInitiateTimeout(event.from, event.sid)
            RefusedMigration.WALKED_AWAY ->
                if (!signaling.finishWithReason(event.from, event.sid, WaddleJingleReason.CANCEL)) {
                    reportError("call finish failed")
                }
        }
    }

    private enum class SlotMovedAnswer { RING_WINNER, DECLINE, ALREADY_CONCLUDED }

    private suspend fun answerProposeAfterTieBreakSlotMoved(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.Propose,
        prev: CallState.Outgoing,
    ) {
        val answer = synchronized(stateLock) {
            val current = _state.value
            when {
                // The abort for this propose raced in after the entry
                // guard: concluded on the wire, nothing to answer. The
                // dead-sid set is written under this same lock, so the
                // check is race-free here.
                recentlyAbortedSids.containsKey(event.sid) -> SlotMovedAnswer.ALREADY_CONCLUDED
                current is CallState.Ended &&
                    current.sid == prev.sid &&
                    current.reason == CallEndReason.Expired -> {
                    cancelCallTimersLocked()
                    _state.value = CallState.Incoming(from = event.from, sid = event.sid, media = kind.media)
                    _lastError.value = null
                    SlotMovedAnswer.RING_WINNER
                }
                else -> SlotMovedAnswer.DECLINE
            }
        }
        when (answer) {
            SlotMovedAnswer.RING_WINNER ->
                if (!signaling.ringing(bareJid(event.from), event.sid)) reportError("call ringing failed")
            SlotMovedAnswer.DECLINE ->
                if (!signaling.reject(event.from, event.sid)) reportError("call reject failed")
            SlotMovedAnswer.ALREADY_CONCLUDED -> Unit
        }
    }

    /**
     * Without a bound resource we cannot host the would-be session,
     * but each tie-break direction still mandates its response
     * (XEP-0353 tie-break-1) — and the sid comparison needs no JID
     * (the JID is only the equal-sid fallback): a losing incoming sid
     * gets the tie-break reject; a winning one means OUR sid loses —
     * retract it, then decline the survivor since we cannot take the
     * call.
     */
    private suspend fun tieBreakWithoutBoundResource(
        event: WaddleCallEvent,
        prev: CallState.Outgoing,
    ) {
        if (compareOctetStrings(event.sid, prev.sid) >= 0) {
            if (!signaling.rejectTieBreak(event.from, event.sid)) {
                reportError("call tie-break reject failed")
            }
            return
        }
        if (!signaling.retractTieBreak(event.from, prev.sid)) {
            reportError("call tie-break retract failed")
        }
        if (!signaling.reject(event.from, event.sid)) reportError("call reject failed")
        endRetractedOutgoing(prev)
    }

    /**
     * Our sid was retracted on the wire; the local ring must die with
     * it instead of ringing on a corpse. No-op once the slot moved on.
     */
    private fun endRetractedOutgoing(prev: CallState.Outgoing) {
        synchronized(stateLock) {
            if (slotStillMatches(prev)) {
                cancelCallTimersLocked()
                _state.value = CallState.Ended(sid = prev.sid, reason = CallEndReason.Expired)
            }
        }
    }

    private suspend fun migrateActiveToIncoming(
        event: WaddleCallEvent,
        kind: WaddleCallEventKind.Propose,
        prev: CallState.Active,
    ) {
        // Same stale-effect guard as the tie-break branch: if the user
        // hung up while this effect was queued, the teardown already
        // sent terminate+finish for the old sid — but the peer's fresh
        // propose still needs an answer, so decline it.
        if (!slotStillMatches(prev)) {
            if (!signaling.reject(event.from, event.sid)) reportError("call reject failed")
            return
        }
        val migrated = signaling.finishMigrated(event.from, prev.sid, event.sid) &&
            signaling.proceed(event.from, event.sid)
        if (!migrated) {
            // Same no-silent-drop invariant as the guard paths: answer
            // the re-propose (best effort) before failing the old call.
            if (!signaling.reject(event.from, event.sid)) reportError("call reject failed")
            failCall(prev.sid, "call migration failed")
            return
        }
        // `accepting = true`: the <proceed/> is already on the wire, so
        // the migrated ring must not re-notify (CallNotifier skips
        // accepting slots), must keep the phone-call FGS alive, and
        // must not offer a duplicate Accept. If a concurrent hang-up
        // claimed the slot after the proceed went out, actively abandon
        // the accepted sid — the same <reject/> hangUp() sends for an
        // accepting incoming — instead of ghosting the peer.
        if (!acceptIncomingTieBreakPropose(event, kind.media, expectedSlot = prev, accepting = true)) {
            abandonRefusedMigration(event, kind, prev)
            return
        }
        scheduleSessionInitiateTimeout(event.from, event.sid)
        // The old Jingle session may already be orphaned on another
        // resource; don't let its IQ round-trip block the XEP-0353
        // migration markers that keep both users' devices in sync.
        scope?.launch {
            if (!signaling.sessionTerminate(prev.peer, prev.sid, WaddleJingleReason.EXPIRED)) {
                reportError("call session terminate failed")
            }
        }
    }

    /**
     * Whether the live slot is still the call this queued effect was
     * computed against — matched on PHASE + SID, not full structure,
     * because benign flag flips (e.g. `ringing=true` landing between
     * the reducer pass and the effect run) must not drop a legitimate
     * tie-break.
     */
    private fun slotStillMatches(expected: CallState): Boolean = synchronized(stateLock) {
        val current = _state.value
        when (expected) {
            is CallState.Outgoing -> current is CallState.Outgoing && current.sid == expected.sid
            is CallState.Active -> current is CallState.Active && current.sid == expected.sid
            is CallState.Incoming -> current is CallState.Incoming && current.sid == expected.sid
            else -> current == expected
        }
    }

    private suspend fun proceedSideEffect(event: WaddleCallEvent, prev: CallState) {
        if (prev !is CallState.Outgoing || prev.sid != event.sid) return
        // A hang-up (or a hang-up plus a NEW outgoing call) that landed
        // while this effect was queued already retracted this sid; a
        // stale session-initiate must not go out for it — and its
        // failure must not clobber whatever owns the slot now.
        if (!callStillLive(event.sid)) return
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
        // Same stale-effect guard as the proceed path: a concurrent
        // hang-up already terminated this sid on the wire.
        if (!callStillLive(event.sid)) return
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

    private fun isRecentlyAborted(sid: String): Boolean = synchronized(stateLock) {
        recentlyAbortedSids.containsKey(sid)
    }

    /**
     * The slot still belongs to this sid's live (non-ended) call, in
     * WHATEVER phase the reducer moved it to — the guard for effects
     * whose expected phase legitimately advances between the reducer
     * pass and the effect run (proceed → still Outgoing;
     * session-initiate → already Active).
     */
    private fun callStillLive(sid: String): Boolean = synchronized(stateLock) {
        val current = _state.value
        current !is CallState.Ended && current.sidOrNull == sid
    }

    /**
     * Replace the slot with the tie-break winner's incoming ring —
     * only if the slot is still the state this effect was computed
     * against; a concurrent hang-up/dismiss owns it otherwise, and the
     * caller must answer the peer's propose on the wire when this
     * returns false.
     */
    private fun acceptIncomingTieBreakPropose(
        event: WaddleCallEvent,
        media: WaddleCallMedia,
        expectedSlot: CallState,
        accepting: Boolean = false,
    ): Boolean {
        synchronized(stateLock) {
            // Reentrant monitor: check-and-replace is one atomic step.
            if (!slotStillMatches(expectedSlot)) return false
            // An abort for this sid that reduced while the effect was
            // suspended in a wire send is consumed and never re-applies
            // — installing the ring anyway would resurrect it. Recorded
            // under this same lock, so the check is race-free.
            if (recentlyAbortedSids.containsKey(event.sid)) return false
            cancelCallTimersLocked()
            _state.value = CallState.Incoming(
                from = event.from,
                sid = event.sid,
                media = media,
                accepting = accepting,
            )
            _lastError.value = null
        }
        return true
    }

    private fun failCall(sid: String, message: String) {
        synchronized(stateLock) {
            // Only the call this failure belongs to may die: a stale
            // effect's send failure must not clobber a newer live slot
            // (or resurrect an Ended one on a signed-out shell).
            if (_state.value.sidOrNull != sid || _state.value is CallState.Ended) return
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
            val current = _state.value
            if (current !is CallState.Outgoing || current.sid != sid) return
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
            val current = _state.value
            if (current !is CallState.Outgoing || current.sid != sid) return
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

    /**
     * Responder-side mirror of [scheduleSessionAcceptTimeout]: after
     * our `<proceed/>`, the caller MUST follow with a Jingle
     * session-initiate. If the caller dies first, no retract ever
     * arrives — end the accepted ring and tell the caller's devices.
     * The web has no equivalent timer; on Android the accepting slot
     * pins a foreground service, so it must be bounded.
     */
    private fun scheduleSessionInitiateTimeout(peerFullJid: String, sid: String) {
        synchronized(stateLock) {
            val current = _state.value
            if (current !is CallState.Incoming || current.sid != sid) return
            sessionAcceptTimer?.cancel()
            sessionAcceptTimer = scope?.launch {
                delay(sessionAcceptTimeoutMillis)
                timeOutSessionInitiate(peerFullJid, sid)
            }
        }
    }

    private suspend fun timeOutSessionInitiate(peerFullJid: String, sid: String) {
        synchronized(stateLock) {
            val current = _state.value
            if (current !is CallState.Incoming || current.sid != sid) return
            _state.value = CallState.Ended(sid = sid, reason = CallEndReason.Timeout)
        }
        // We answered the propose with <proceed/>, so the abandon verb
        // is the <finish/> bookend with <timeout/> — not a late reject.
        if (!signaling.finishWithReason(peerFullJid, sid, WaddleJingleReason.TIMEOUT)) {
            reportError("call finish failed")
        }
    }

    /**
     * Whether a transition left the phase whose timer is armed: out of
     * Outgoing (auto-retract / session-accept) or out of Incoming (the
     * responder's session-initiate timer — Active arrival, remote
     * retract, sibling-device proceed).
     */
    private fun leftTimerOwningPhase(before: CallState, next: CallState): Boolean {
        val leftOutgoing = before is CallState.Outgoing && next !is CallState.Outgoing
        val leftIncoming = before is CallState.Incoming && next !is CallState.Incoming
        return leftOutgoing || leftIncoming
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

    /**
     * Atomic read-modify-write on the single shared call slot for the
     * MUC group-call flow ([MucCallEngine]). Any transition cancels the
     * DM timers (Idle/Ended entry states hold none, so this is a
     * no-op guard) and clears the stale error, matching the reducer
     * path's transition side-effects.
     */
    internal fun <T> updateCallSlot(block: (CallState) -> Pair<CallState, T>): T =
        synchronized(stateLock) {
            val current = _state.value
            val (next, result) = block(current)
            if (next != current) {
                cancelCallTimersLocked()
                _state.value = next
                _lastError.value = null
            }
            result
        }

    /** Error surface for the MUC engine (same slot as the DM verbs). */
    internal fun reportCallError(message: String) = reportError(message)

    companion object {
        /**
         * Outgoing-ring auto-retract (web `OUTGOING_TIMEOUT_MS`): after
         * this elapses with the slot still `Outgoing`, retract so the
         * peer's devices stop ringing when nobody answers.
         */
        const val OUTGOING_TIMEOUT_MILLIS = 45_000L

        /** Session-accept gap timeout (web `SESSION_ACCEPT_TIMEOUT_MS`). */
        const val SESSION_ACCEPT_TIMEOUT_MILLIS = 45_000L

        /**
         * LRU bound for [recentlyAbortedSids]. The reducer can run
         * arbitrarily far ahead of the effects consumer, so the bound
         * must comfortably exceed any plausible reducer→effect lag in
         * abort events; memory cost is negligible.
         */
        private const val ABORTED_SID_CAPACITY = 256
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
