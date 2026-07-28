package social.waddle.android.client.calls

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleCallEvent
import social.waddle.client.ffi.WaddleCallEventKind
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleInCallPresenceFlags
import social.waddle.client.ffi.WaddleLiveKitJoin

/**
 * XEP-0272 Muji group-call flow over the SAME single call slot the DM
 * engine owns — the Kotlin port of the web's `beginMucCall` /
 * `tearDownActiveCall` MUC branches plus muc-call-actions.ts. There is
 * deliberately no second store: mutual exclusion between DM and group
 * calls falls out of the shared slot's Idle/Ended entry guard.
 *
 * §Joining order (each step gated on the slot still holding this
 * attempt): register the preparing-echo waiter BEFORE the preparing
 * presence goes out → await the MUC echo → await other preparing
 * occupants → content-declaring active presence → register the accept
 * resolver BEFORE the Jingle session-initiate → await the mixer's
 * separate session-accept → promote the slot to Active. Every failure
 * rolls back presence-first (§Leaving), terminating the mixer half
 * only once the active presence became room-visible.
 */
class MucCallEngine internal constructor(
    private val store: CallStore,
    private val signaling: CallSignaling,
    private val presence: MucCallPresence,
    private val sessionCache: MucCallSessionCache,
    private val ownFullJid: () -> String?,
    private val newSid: () -> String,
) {
    private val lock = Any()

    /**
     * Pending session-accept resolvers keyed by per-attempt sid. An
     * inbound accept matching an entry is consumed by [tryFulfillAccept]
     * and NEVER reaches the 1:1 reducer.
     */
    private val pendingAccepts = HashMap<String, PendingMujiAccept>()

    /**
     * Serializes EVERY muji presence send — setup phases, in-call
     * re-stamps, and leave clears — so WIRE order matches decision
     * order: a teardown's leave can never be overtaken by a stalled
     * setup presence, and a parked re-stamp can never resurrect the
     * active advertisement after a leave (each re-stamp re-checks the
     * slot INSIDE the lock). kotlinx's [Mutex] is fair, so waiters
     * send in FIFO order.
     */
    private val presenceSendMutex = Mutex()

    private val _selfHandRaised = MutableStateFlow(false)

    /**
     * Our own optimistic `urn:waddle:in-call:0` raised-hand marker
     * (web `$mucCallSelfHandRaised` parity): flipped BEFORE the
     * presence send, rolled back on failure unless a newer toggle
     * superseded it. The UI reads THIS, never the room's echo.
     */
    val selfHandRaised: StateFlow<Boolean> = _selfHandRaised.asStateFlow()

    private val _selfMuted = MutableStateFlow(false)

    /** Our own optimistic self-mute marker; the local mic toggle owns it. */
    val selfMuted: StateFlow<Boolean> = _selfMuted.asStateFlow()

    private class PendingMujiAccept(
        val roomJid: String,
        /** Normalized bare mixer JID (`calls.<domain>`), when known. */
        val expectedFrom: String?,
        val join: CompletableDeferred<WaddleLiveKitJoin>,
    )

    /** One group-call attempt's immutable identity, threaded through the setup steps. */
    private data class MucAttempt(
        val room: String,
        val sid: String,
        val media: WaddleCallMedia,
        val selfNick: String,
        val selfFullJid: String?,
        val expectedMixerJid: String?,
        val connection: CallConnection,
    )

    /** Originator action for MUC group calls (web `beginMucCall`). */
    suspend fun begin(
        roomJid: String,
        media: WaddleCallMedia,
        selfNick: String?,
        selfFullJid: String?,
        expectedMixerJid: String? = null,
    ): Boolean {
        val room = normalizeMucCallRoomJid(roomJid)
        if (room.isEmpty() || selfNick.isNullOrEmpty()) {
            store.reportCallError("muc call needs a room jid and a nick")
            return false
        }
        val connection = signaling.captureActiveConnection() ?: return false
        val attempt = MucAttempt(room, newSid(), media, selfNick, selfFullJid, expectedMixerJid, connection)
        var claimed = false
        connection.applyIfCurrent {
            claimed = store.updateCallSlot { current ->
                if (current is CallState.Idle || current is CallState.Ended) {
                    CallState.MucPending(room, attempt.sid, media, selfNick, selfFullJid, connection = connection) to true
                } else {
                    current to false
                }
            }
        }
        if (!claimed) return false
        _selfHandRaised.value = false
        // Joining without mic capture advertises muted; a live mic
        // toggle re-broadcasts the authoritative state post-connect.
        _selfMuted.value = !media.audio
        return runSetup(attempt)
    }

    /**
     * Route an inbound `session-accept` to the pending resolver, if
     * any. `true` means the event is CONSUMED (matched sid) — even a
     * wrong-room / wrong-mixer accept never falls through to the 1:1
     * reducer; it just leaves the waiter to its timeout.
     */
    internal fun tryFulfillAccept(event: WaddleCallEvent): Boolean {
        val kind = event.kind as? WaddleCallEventKind.SessionAccept ?: return false
        val pending = synchronized(lock) { pendingAccepts[event.sid] } ?: return false
        if (normalizeMucCallRoomJid(kind.join.room) != pending.roomJid) return true
        if (pending.expectedFrom != null && bareJid(event.from).lowercase() != pending.expectedFrom) {
            return true
        }
        synchronized(lock) { pendingAccepts.remove(event.sid) }
        pending.join.complete(kind.join)
        return true
    }

    /**
     * Wire teardown for a live MUC call whose slot the caller already
     * cleared. XEP-0272 §Leaving order: the bare-presence leave marker
     * first ("updating the presence first reduces the likelihood of
     * situations where new participants initiate sessions with
     * participants who are leaving"), then the mixer terminate. Media
     * disconnect happens via the controller on the slot change.
     */
    suspend fun teardownActive(current: CallState.Active) {
        val connection = current.connection ?: return
        teardownActiveWith(current, connection, ownFullJid())
    }

    /** Internal logout path with a captured call-only sender. */
    internal suspend fun teardownActiveWith(
        current: CallState.Active,
        teardownSignaling: CallConnection,
        teardownOwnFullJid: String?,
    ) {
        current.selfNick?.let { nick -> leavePresence(current.peer, nick, teardownOwnFullJid, teardownSignaling) }
        terminate(current.peer, current.sid, teardownOwnFullJid, teardownSignaling)
    }

    /**
     * A remote end event (mixer session-terminate, or a stray
     * reject/retract/finish) with the sid a MUC phase holds, routed
     * from the store's effect queue: claim the slot → `Ended(reason)`,
     * then run the full teardown — leave presence, waiter/resolver
     * cancellation, (idempotent) mixer terminate, cache forget —
     * keeping the XEP-0272 §Leaving order even for remote-initiated
     * ends. No-op once the slot moved on.
     */
    internal suspend fun endFromRemote(sid: String, reason: CallEndReason) {
        val claimed = store.updateCallSlot { current ->
            if (current.isMucCallPhase && current.sidOrNull == sid) {
                CallState.Ended(sid = sid, reason = reason) to current
            } else {
                current to null
            }
        } ?: return
        when (claimed) {
            is CallState.Active -> teardownActive(claimed)
            is CallState.MucPending -> teardownPending(claimed)
            else -> Unit
        }
    }

    /** Teardown for an abandoned setup whose slot the caller already cleared. */
    suspend fun teardownPending(current: CallState.MucPending) {
        val connection = current.connection ?: return
        teardownPendingWith(current, connection)
    }

    /** Internal logout path with a captured call-only sender. */
    internal suspend fun teardownPendingWith(
        current: CallState.MucPending,
        teardownSignaling: CallConnection,
    ) {
        presence.cancelPreparationWaiters(current.roomJid, current.selfNick)
        cancelPendingAccept(current.sid)
        leavePresence(current.roomJid, current.selfNick, current.selfFullJid, teardownSignaling)
        if (current.activePresencePublished) {
            terminate(current.roomJid, current.sid, current.selfFullJid, teardownSignaling)
        }
    }

    /**
     * Raise/lower our hand in the active MUC call: re-emit the active
     * presence with BOTH in-call flags re-stamped. Optimistic local
     * flip; a send failure reverts unless a newer toggle superseded it.
     */
    suspend fun setHandRaised(raised: Boolean): Boolean {
        val target = mucActiveOrNull() ?: return false
        val nick = target.selfNick ?: return false
        _selfHandRaised.value = raised
        return when (restampActivePresence(target, nick)) {
            RestampOutcome.SENT -> true
            // A teardown claimed the slot while this re-stamp was
            // parked; its leave already cleared every in-call marker.
            RestampOutcome.SLOT_MOVED -> false
            RestampOutcome.FAILED -> {
                _selfHandRaised.compareAndSet(expect = raised, update = !raised)
                store.reportCallError("muji hand-raise update failed")
                false
            }
        }
    }

    /**
     * Broadcast our mute marker to the active MUC call, re-stamping the
     * CURRENT raised hand so a mute toggle never drops it. The local
     * mic toggle is the source of truth — no rollback on failure.
     */
    suspend fun broadcastSelfMute(muted: Boolean): Boolean {
        val target = mucActiveOrNull() ?: return false
        val nick = target.selfNick ?: return false
        _selfMuted.value = muted
        return when (restampActivePresence(target, nick)) {
            RestampOutcome.SENT -> true
            RestampOutcome.SLOT_MOVED -> false
            RestampOutcome.FAILED -> {
                store.reportCallError("muji mute update failed")
                false
            }
        }
    }

    /** One serialized active-presence re-stamp attempt's outcome. */
    private enum class RestampOutcome { SENT, FAILED, SLOT_MOVED }

    /**
     * Re-emit the active presence with BOTH in-call flags, serialized
     * on [presenceSendMutex]. The flags are snapshotted INSIDE the
     * lock, so racing toggles send the identical final optimistic
     * state whatever order they acquire it in — and the slot is
     * re-checked INSIDE the lock, so a re-stamp parked behind a
     * teardown's leave skips silently instead of resurrecting the
     * active advertisement on the wire.
     */
    private suspend fun restampActivePresence(
        target: CallState.Active,
        nick: String,
    ): RestampOutcome = presenceSendMutex.withLock {
        val current = mucActiveOrNull()
        if (current == null || current.peer != target.peer || current.sid != target.sid) {
            RestampOutcome.SLOT_MOVED
        } else {
            val connection = current.connection ?: return@withLock RestampOutcome.SLOT_MOVED
            val sent = connection.updateMujiPresence(
                MujiPresenceUpdate(
                    roomJid = target.peer, nick = nick,
                    active = true, preparing = false, video = target.media.video,
                    flags = WaddleInCallPresenceFlags(
                        handRaised = _selfHandRaised.value,
                        muted = _selfMuted.value,
                    ),
                ),
            )
            if (sent) RestampOutcome.SENT else RestampOutcome.FAILED
        }
    }

    /**
     * Process-death recovery (web `resumeMucCallActivity`): promote the
     * slot straight to Active with the CACHED LiveKit join — no fresh
     * Jingle attempt, no new sid, no doubled Muji presence. Best-effort
     * re-publish of the active advertisement under the current
     * resource; LiveKit identity-uniqueness displaces the orphan.
     */
    suspend fun resume(
        roomJid: String,
        selfNick: String?,
        selfFullJid: String?,
        nowMillis: Long = System.currentTimeMillis(),
    ): Boolean {
        val connection = signaling.captureActiveConnection() ?: return false
        val room = normalizeMucCallRoomJid(roomJid)
        val self = selfFullJid?.trim().orEmpty()
        if (room.isEmpty() || selfNick.isNullOrEmpty() || self.isEmpty()) return false
        if (!sessionCache.canResume(room, self, nowMillis)) return false
        val session = sessionCache.read(room, self, nowMillis) ?: return false
        val join = session.join() ?: return false
        val media = session.media()
        val promotedState = CallState.Active(
            peer = session.roomJid,
            sid = session.sid,
            media = media,
            join = join,
            kind = CallKind.MUC,
            selfNick = selfNick,
        )
        var promoted = false
        connection.applyIfCurrent {
            promoted = store.updateCallSlot { current ->
                if (current is CallState.Idle || current is CallState.Ended) {
                    promotedState.copy(connection = connection) to true
                } else {
                    current to false
                }
            }
        }
        if (!promoted) return false
        // Claim-time flag initialization, exactly like begin(): a
        // stale hand/mute from a previous room's teardown (whose
        // leavePresence skips the reset once this attempt owns the
        // slot) must not ride into the resumed call's re-stamps; the
        // post-process-death resume has both at defaults anyway, and
        // joining without mic capture stays advertised muted. The
        // re-stamp serializes on the presence mutex and re-checks the
        // slot, so it can never land after a leave that claimed it.
        _selfHandRaised.value = false
        _selfMuted.value = !media.audio
        restampActivePresence(promotedState.copy(connection = connection), selfNick)
        return true
    }

    /**
     * Hard-refresh recovery for a room this resource still advertises
     * but whose local call state is gone (web
     * `leaveRetainedMucCallAction`): presence-clear first (§Leaving),
     * then the cached-sid terminate; a failed terminate flags the entry
     * so cleanup can retry.
     */
    suspend fun leaveRetained(
        roomJid: String,
        selfNick: String?,
        selfFullJid: String?,
        nowMillis: Long = System.currentTimeMillis(),
    ): Boolean {
        val connection = signaling.captureActiveConnection() ?: return false
        val room = normalizeMucCallRoomJid(roomJid)
        if (room.isEmpty() || selfNick.isNullOrEmpty()) return false
        // A live local call owns this room: its presence must not be
        // cleared, and the room's freshest cache entry is the LIVE
        // session's — terminating that sid would kill the call.
        if (slotHoldsMucRoom(room)) return false
        val self = selfFullJid?.trim().orEmpty()
        // Exact-resource entry first; otherwise any entry this ACCOUNT
        // owes the mixer for the room (the resource suffix may have
        // changed across the process death that orphaned the call).
        val cached = if (self.isEmpty()) {
            null
        } else {
            sessionCache.read(room, self, nowMillis) ?: sessionCache.retainedEntry(room, self, nowMillis)
        }
        val cleared = presenceSendMutex.withLock {
            // Re-check under the lock: the suspending cache reads above
            // are long enough for a fresh begin() to claim this room
            // (the Join pill is visible alongside the ghost banner),
            // and its marker must not be wiped — same stand-down as
            // [leavePresence].
            if (slotHoldsMucRoom(room)) return false
            connection.updateMujiPresence(
                MujiPresenceUpdate(
                    roomJid = room, nick = selfNick,
                    active = false, preparing = false, video = false,
                    flags = WaddleInCallPresenceFlags(handRaised = false, muted = false),
                ),
            )
        }
        if (!cleared) {
            store.reportCallError("muji leave presence failed")
            return false
        }
        presence.markSelfLeaveEchoPending(room, selfNick)
        presence.clearParticipant(room, selfNick, self.ifEmpty { null })
        if (cached == null) return true
        // Same (room, identity) guard as [retryPendingTerminates]: a
        // fresh call claimed mid-flow keeps the entry flagged instead
        // of firing the stale terminate onto the live session.
        if (slotHoldsMucRoom(cached.roomJid)) {
            sessionCache.markTerminatePending(cached.roomJid, cached.sid, cached.selfFullJid, nowMillis)
            return false
        }
        if (!connection.mujiSessionTerminate(cached.roomJid, cached.sid)) {
            sessionCache.markTerminatePending(cached.roomJid, cached.sid, cached.selfFullJid, nowMillis)
            store.reportCallError("muji session terminate failed")
            return false
        }
        return if (slotHoldsMucRoom(cached.roomJid)) {
            // Claimed while the terminate IQ was in flight: it may have
            // raced onto the fresh session — keep the debt recorded so
            // a later connect settles it once the room is free.
            sessionCache.markTerminatePending(cached.roomJid, cached.sid, cached.selfFullJid, nowMillis)
            false
        } else {
            sessionCache.forget(cached.roomJid, cached.selfFullJid, cached.sid)
            true
        }
    }

    /**
     * Best-effort once-per-connect retry of the XEP-0166 terminates a
     * previous leave still owes the mixer ([MucCallSessionCache]
     * `terminatePending` entries for this account). Driven from the
     * session-ready hook; a still-failing send keeps the entry flagged
     * for the next connect.
     */
    suspend fun retryPendingTerminates(
        selfFullJid: String?,
        nowMillis: Long = System.currentTimeMillis(),
    ) {
        val connection = signaling.captureActiveConnection() ?: return
        val self = selfFullJid?.trim().orEmpty()
        if (self.isEmpty()) return
        for (entry in sessionCache.terminatePendingEntries(self, nowMillis)) {
            // Mixer sessions are (room, identity)-scoped: while the
            // slot holds a call in this room, the stale terminate could
            // fire onto the FRESH session — keep the entry flagged for
            // the next connect instead.
            if (slotHoldsMucRoom(entry.roomJid)) continue
            val sent = connection.mujiSessionTerminate(entry.roomJid, entry.sid)
            // Re-check AFTER the send returns (the IQ round-trip is
            // long enough for the user to begin a call here): if the
            // room was claimed mid-retry the terminate may have raced
            // onto the fresh session, so keep the flag and let a later
            // cleanup settle it once the room is free again.
            if (sent && !slotHoldsMucRoom(entry.roomJid)) {
                sessionCache.forget(entry.roomJid, entry.selfFullJid, entry.sid)
            }
        }
    }

    /** Session teardown: drop pending resolvers and in-call flags. */
    internal fun clear() {
        _selfHandRaised.value = false
        _selfMuted.value = false
        val cancelled = synchronized(lock) {
            val pending = pendingAccepts.values.toList()
            pendingAccepts.clear()
            pending
        }
        cancelled.forEach {
            it.join.cancel(CancellationException("Muji session-accept wait cancelled while clearing call state"))
        }
    }

    // ── Setup steps ──────────────────────────────────────────────────────────

    private suspend fun runSetup(attempt: MucAttempt): Boolean {
        if (!prepare(attempt)) return rollback(attempt, "muc call preparation failed")
        if (!publishActivePresence(attempt)) return rollback(attempt, "muji active presence failed")
        val join = initiate(attempt) ?: return rollback(attempt, "muji session initiate failed")
        return activate(attempt, join)
    }

    /**
     * Step 1 — preparing presence. The echo waiter registers BEFORE the
     * emit so a fast MUC echo can't fire before the listener exists;
     * the MUC MUST rebroadcast it before we may declare contents.
     */
    private suspend fun prepare(attempt: MucAttempt): Boolean {
        val room = attempt.room
        val selfNick = attempt.selfNick
        val selfFullJid = attempt.selfFullJid
        val waiter = presence.registerPreparingEchoWaiter(room, selfNick, selfFullJid)
        val sent = presenceSendMutex.withLock {
            attempt.connection.updateMujiPresence(
                MujiPresenceUpdate(
                    roomJid = room, nick = selfNick,
                    active = false, preparing = true, video = false,
                    // in-call state isn't advertised before joining
                    flags = WaddleInCallPresenceFlags(handRaised = false, muted = false),
                ),
            )
        }
        if (!sent) return false
        if (!stillPending(attempt)) return preparingFailed(attempt)
        if (!presence.awaitPreparingEcho(waiter, PREPARING_ECHO_TIMEOUT_MILLIS)) return preparingFailed(attempt)
        if (!stillPending(attempt)) return preparingFailed(attempt)
        if (!presence.awaitNoOtherPreparing(room, selfNick, selfFullJid, PREPARING_PEERS_TIMEOUT_MILLIS)) {
            return preparingFailed(attempt)
        }
        return stillPending(attempt) || preparingFailed(attempt)
    }

    /**
     * A prepare step failed after the preparing presence hit the wire.
     * When the slot no longer holds this attempt, a concurrent hangUp
     * owned the cleanup — re-clear our room-visible advertisement here
     * (idempotent bare presence), mirroring [publishActivePresence]'s
     * `!marked` branch; when the slot is still ours, [rollback] sends
     * the leave. [leavePresence] stands down when a NEWER same-room
     * attempt already owns the room's presence, so an abandoned
     * attempt can never wipe its successor's preparing/active marker.
     */
    private suspend fun preparingFailed(attempt: MucAttempt): Boolean {
        if (!stillPending(attempt)) {
            leavePresence(attempt.room, attempt.selfNick, attempt.selfFullJid, attempt.connection)
        }
        return false
    }

    /**
     * Step 2 — content-declaring presence (XEP-0272 §Joining: contents
     * are advertised in MUC presence BEFORE the Jingle session).
     */
    private suspend fun publishActivePresence(attempt: MucAttempt): Boolean {
        val sent = presenceSendMutex.withLock {
            attempt.connection.updateMujiPresence(
                MujiPresenceUpdate(
                    roomJid = attempt.room, nick = attempt.selfNick,
                    active = true, preparing = false, video = attempt.media.video,
                    flags = WaddleInCallPresenceFlags(handRaised = false, muted = !attempt.media.audio),
                ),
            )
        }
        if (!sent) return false
        val marked = store.updateCallSlot { current ->
            if (current is CallState.MucPending && current.roomJid == attempt.room && current.sid == attempt.sid) {
                current.copy(activePresencePublished = true) to true
            } else {
                current to false
            }
        }
        if (!marked) {
            // The slot moved while the active presence was in flight:
            // the teardown owner saw activePresencePublished=false, so
            // re-clear the room-visible advertisement here (web parity).
            // [leavePresence] stands down when a NEWER same-room
            // attempt already owns the room's presence.
            leavePresence(attempt.room, attempt.selfNick, attempt.selfFullJid, attempt.connection)
        }
        return marked
    }

    /**
     * Step 3 — Jingle session-initiate to the mixer. The resolver
     * registers BEFORE the send (XEP-0166 §6.3 separate-IQ accept), so
     * an accept racing the empty ack cannot be missed.
     */
    private suspend fun initiate(attempt: MucAttempt): WaddleLiveKitJoin? {
        val initiator = ownFullJid()
        if (initiator == null) {
            store.reportCallError("muji session initiate failed: no bound resource")
            return null
        }
        val deferred = CompletableDeferred<WaddleLiveKitJoin>()
        val expected = attempt.expectedMixerJid?.let { bareJid(it).trim().lowercase() }?.ifEmpty { null }
        synchronized(lock) {
            pendingAccepts[attempt.sid] = PendingMujiAccept(attempt.room, expected, deferred)
        }
        // Last gate before the wire: a teardown that claimed the slot
        // after the active-presence CAS already sent leave + terminate
        // for this sid — a fresh initiate now would re-open a mixer
        // session nothing local tracks. The teardown owner handled the
        // room-visible cleanup, so bail without further sends.
        if (!stillPending(attempt)) {
            cancelPendingAccept(attempt.sid)
            return null
        }
        if (!attempt.connection.mujiSessionInitiate(attempt.room, initiator, attempt.sid, attempt.media.video)) {
            cancelPendingAccept(attempt.sid)
            return null
        }
        val join = withTimeoutOrNull(MUJI_ACCEPT_TIMEOUT_MILLIS) {
            try {
                deferred.await()
            } catch (cancellation: CancellationException) {
                if (deferred.isCancelled) null else throw cancellation
            }
        }
        if (join == null) cancelPendingAccept(attempt.sid)
        return join
    }

    /** Step 4 — promote the slot; a concurrent teardown owns cleanup otherwise. */
    private suspend fun activate(attempt: MucAttempt, join: WaddleLiveKitJoin): Boolean {
        val activated = store.updateCallSlot { current ->
            if (current is CallState.MucPending && current.roomJid == attempt.room && current.sid == attempt.sid) {
                CallState.Active(
                    peer = attempt.room,
                    sid = attempt.sid,
                    media = attempt.media,
                    join = join,
                    kind = CallKind.MUC,
                    selfNick = attempt.selfNick,
                    connection = attempt.connection,
                ) to true
            } else {
                current to false
            }
        }
        if (activated) {
            sessionCache.remember(attempt.room, attempt.sid, attempt.selfFullJid, attempt.media, join)
        } else {
            // The mixer accepted AFTER a teardown claimed the slot: the
            // teardown's terminate provably predates the accept, so the
            // fresh mixer session is half-open with no local trace.
            // Close it here (the mixer-side terminate is idempotent).
            terminate(attempt.room, attempt.sid, attempt.selfFullJid, attempt.connection)
        }
        return activated
    }

    /**
     * Failed-setup rollback (web `rollbackMucCallSetup`): only when the
     * slot still holds this attempt — a concurrent hangUp already ran
     * the teardown otherwise. Presence-clear first; terminate only once
     * the active presence became room-visible.
     */
    private suspend fun rollback(attempt: MucAttempt, message: String): Boolean {
        val pending = store.updateCallSlot { current ->
            if (current is CallState.MucPending && current.roomJid == attempt.room && current.sid == attempt.sid) {
                CallState.Idle to current
            } else {
                current to null
            }
        } ?: return false
        presence.cancelPreparationWaiters(attempt.room, attempt.selfNick)
        cancelPendingAccept(attempt.sid)
        leavePresence(attempt.room, attempt.selfNick, attempt.selfFullJid, attempt.connection)
        if (pending.activePresencePublished) terminate(attempt.room, attempt.sid, attempt.selfFullJid, attempt.connection)
        store.reportCallError(message)
        return false
    }

    /**
     * Room-visible presence clear (XEP-0272 §Leaving marker),
     * serialized on [presenceSendMutex] so it keeps wire order with
     * every other muji presence send. Stands down entirely — checked
     * INSIDE the lock — when the slot holds a MUC phase for [room]: a
     * NEWER same-room attempt owns the room's presence, its markers
     * must survive this stale clear, and its own rollback/teardown
     * sends the leave if it fails.
     */
    private suspend fun leavePresence(
        room: String,
        selfNick: String,
        selfFullJid: String?,
        teardownSignaling: CallConnection,
    ) {
        val cleared = presenceSendMutex.withLock {
            if (slotHoldsMucRoom(room)) return
            teardownSignaling.updateMujiPresence(
                MujiPresenceUpdate(
                    roomJid = room, nick = selfNick,
                    active = false, preparing = false, video = false,
                    // leaving the call clears all in-call state
                    flags = WaddleInCallPresenceFlags(handRaised = false, muted = false),
                ),
            )
        }
        if (!cleared) store.reportCallError("muji leave presence failed")
        // Marker only for a leave that actually reached the wire: a
        // failed send gets no echo, and a stuck marker would hide the
        // retained-leave recovery for the durable ghost it leaves.
        if (cleared) presence.markSelfLeaveEchoPending(room, selfNick)
        presence.clearParticipant(room, selfNick, selfFullJid)
        // The optimistic flags belong to whatever MUC attempt owns the
        // slot NOW: a concurrently-begun different-room attempt already
        // re-initialized them at claim time, and wiping them here would
        // clobber its join-muted state.
        if (!store.state.value.isMucCallPhase) {
            _selfHandRaised.value = false
            _selfMuted.value = false
        }
    }

    private suspend fun terminate(
        room: String,
        sid: String,
        selfFullJid: String?,
        teardownSignaling: CallConnection,
    ) {
        if (teardownSignaling.mujiSessionTerminate(room, sid)) {
            sessionCache.forget(room, selfFullJid, sid)
        } else {
            store.reportCallError("muji session terminate failed")
        }
    }

    private fun cancelPendingAccept(sid: String) {
        val pending = synchronized(lock) { pendingAccepts.remove(sid) } ?: return
        pending.join.cancel(CancellationException("Muji session-accept wait cancelled"))
    }

    private fun stillPending(attempt: MucAttempt): Boolean {
        val current = store.state.value
        return current is CallState.MucPending &&
            current.roomJid == attempt.room &&
            current.sid == attempt.sid
    }

    private fun mucActiveOrNull(): CallState.Active? =
        (store.state.value as? CallState.Active)?.takeIf { it.kind == CallKind.MUC }

    /**
     * Whether the live slot holds ANY MUC phase for [room] — the
     * signal that a (possibly newer) local attempt owns the room's
     * presence and mixer session, so stale clears and terminates for
     * the room must stand down.
     */
    private fun slotHoldsMucRoom(room: String): Boolean =
        when (val current = store.state.value) {
            is CallState.MucPending -> current.roomJid == room
            is CallState.Active ->
                current.kind == CallKind.MUC && normalizeMucCallRoomJid(current.peer) == room
            else -> false
        }

    companion object {
        /**
         * XEP-0272 §Joining MUST: bound wait for the MUC's rebroadcast
         * of our preparing presence (web `PREPARING_ECHO_TIMEOUT_MS`).
         */
        const val PREPARING_ECHO_TIMEOUT_MILLIS = 2_000L

        /** Bound wait for other preparing occupants (web parity). */
        const val PREPARING_PEERS_TIMEOUT_MILLIS = 2_000L

        /**
         * XEP-0166 §6.3 separate-IQ accept: bound wait for the mixer's
         * session-accept after the initiate's empty ack (web
         * `MUJI_ACCEPT_TIMEOUT_MS`; covers slow SFU token mints).
         */
        const val MUJI_ACCEPT_TIMEOUT_MILLIS = 10_000L
    }
}
