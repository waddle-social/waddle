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
     * Serializes the muji presence re-stamps (hand raise / mute) so
     * WIRE order matches snapshot order — two racing toggles must land
     * on the last-writer-wins presence in the order they were taken.
     * kotlinx's [Mutex] is fair, so waiters send in FIFO order.
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
        val attempt = MucAttempt(room, newSid(), media, selfNick, selfFullJid, expectedMixerJid)
        val claimed = store.updateCallSlot { current ->
            if (current is CallState.Idle || current is CallState.Ended) {
                CallState.MucPending(room, attempt.sid, media, selfNick, selfFullJid) to true
            } else {
                current to false
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
        current.selfNick?.let { nick -> leavePresence(current.peer, nick, ownFullJid()) }
        terminate(current.peer, current.sid, ownFullJid())
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
        presence.cancelPreparationWaiters(current.roomJid, current.selfNick)
        cancelPendingAccept(current.sid)
        leavePresence(current.roomJid, current.selfNick, current.selfFullJid)
        if (current.activePresencePublished) {
            terminate(current.roomJid, current.sid, current.selfFullJid)
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
        val sent = presenceSendMutex.withLock {
            signaling.updateMujiPresence(
                MujiPresenceUpdate(
                    roomJid = target.peer, nick = nick,
                    active = true, preparing = false, video = target.media.video,
                    // Mute read INSIDE the send mutex: any earlier mute
                    // broadcast has already landed, so the re-stamp
                    // carries the newest marker.
                    flags = WaddleInCallPresenceFlags(handRaised = raised, muted = _selfMuted.value),
                ),
            )
        }
        if (!sent) {
            _selfHandRaised.compareAndSet(expect = raised, update = !raised)
            store.reportCallError("muji hand-raise update failed")
        }
        return sent
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
        val sent = presenceSendMutex.withLock {
            signaling.updateMujiPresence(
                MujiPresenceUpdate(
                    roomJid = target.peer, nick = nick,
                    active = true, preparing = false, video = target.media.video,
                    flags = WaddleInCallPresenceFlags(handRaised = _selfHandRaised.value, muted = muted),
                ),
            )
        }
        if (!sent) store.reportCallError("muji mute update failed")
        return sent
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
        val room = normalizeMucCallRoomJid(roomJid)
        val self = selfFullJid?.trim().orEmpty()
        if (room.isEmpty() || selfNick.isNullOrEmpty() || self.isEmpty()) return false
        if (!sessionCache.canResume(room, self, nowMillis)) return false
        val session = sessionCache.read(room, self, nowMillis) ?: return false
        val join = session.join() ?: return false
        val media = session.media()
        val promoted = store.updateCallSlot { current ->
            if (current is CallState.Idle || current is CallState.Ended) {
                CallState.Active(
                    peer = session.roomJid,
                    sid = session.sid,
                    media = media,
                    join = join,
                    kind = CallKind.MUC,
                    selfNick = selfNick,
                ) to true
            } else {
                current to false
            }
        }
        if (!promoted) return false
        // The re-publish must not wipe in-call state: joining without
        // mic capture stays advertised muted, and any flags a live
        // engine still carries survive (post-process-death both flows
        // are at their false defaults anyway).
        val muted = _selfMuted.value || !media.audio
        val handRaised = _selfHandRaised.value
        _selfMuted.value = muted
        signaling.updateMujiPresence(
            MujiPresenceUpdate(
                roomJid = room, nick = selfNick,
                active = true, preparing = false, video = media.video,
                flags = WaddleInCallPresenceFlags(handRaised = handRaised, muted = muted),
            ),
        )
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
        val room = normalizeMucCallRoomJid(roomJid)
        if (room.isEmpty() || selfNick.isNullOrEmpty()) return false
        val self = selfFullJid?.trim().orEmpty()
        // Exact-resource entry first; otherwise any entry this ACCOUNT
        // owes the mixer for the room (the resource suffix may have
        // changed across the process death that orphaned the call).
        val cached = if (self.isEmpty()) {
            null
        } else {
            sessionCache.read(room, self, nowMillis) ?: sessionCache.retainedEntry(room, self, nowMillis)
        }
        val cleared = signaling.updateMujiPresence(
            MujiPresenceUpdate(
                roomJid = room, nick = selfNick,
                active = false, preparing = false, video = false,
                flags = WaddleInCallPresenceFlags(handRaised = false, muted = false),
            ),
        )
        if (!cleared) {
            store.reportCallError("muji leave presence failed")
            return false
        }
        presence.clearParticipant(room, selfNick, self.ifEmpty { null })
        if (cached == null) return true
        return if (signaling.mujiSessionTerminate(cached.roomJid, cached.sid)) {
            sessionCache.forget(cached.roomJid, cached.selfFullJid, cached.sid)
            true
        } else {
            sessionCache.markTerminatePending(cached.roomJid, cached.sid, cached.selfFullJid, nowMillis)
            store.reportCallError("muji session terminate failed")
            false
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
        val self = selfFullJid?.trim().orEmpty()
        if (self.isEmpty()) return
        for (entry in sessionCache.terminatePendingEntries(self, nowMillis)) {
            if (signaling.mujiSessionTerminate(entry.roomJid, entry.sid)) {
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
        val sent = signaling.updateMujiPresence(
            MujiPresenceUpdate(
                roomJid = room, nick = selfNick,
                active = false, preparing = true, video = false,
                // in-call state isn't advertised before joining
                flags = WaddleInCallPresenceFlags(handRaised = false, muted = false),
            ),
        )
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
     * owned the cleanup — but its leave presence may have raced AHEAD
     * of our still-in-flight preparing presence, leaving a permanent
     * room-visible `<preparing/>` ghost that breaks every other
     * occupant's `awaitNoOtherPreparing`. Re-clear it here (idempotent
     * bare presence), mirroring [publishActivePresence]'s `!marked`
     * branch; when the slot is still ours, [rollback] sends the leave.
     */
    private suspend fun preparingFailed(attempt: MucAttempt): Boolean {
        if (!stillPending(attempt)) {
            leavePresence(attempt.room, attempt.selfNick, attempt.selfFullJid)
        }
        return false
    }

    /**
     * Step 2 — content-declaring presence (XEP-0272 §Joining: contents
     * are advertised in MUC presence BEFORE the Jingle session).
     */
    private suspend fun publishActivePresence(attempt: MucAttempt): Boolean {
        val sent = signaling.updateMujiPresence(
            MujiPresenceUpdate(
                roomJid = attempt.room, nick = attempt.selfNick,
                active = true, preparing = false, video = attempt.media.video,
                flags = WaddleInCallPresenceFlags(handRaised = false, muted = !attempt.media.audio),
            ),
        )
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
            leavePresence(attempt.room, attempt.selfNick, attempt.selfFullJid)
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
        if (!signaling.mujiSessionInitiate(attempt.room, initiator, attempt.sid, attempt.media.video)) {
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
            terminate(attempt.room, attempt.sid, attempt.selfFullJid)
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
        leavePresence(attempt.room, attempt.selfNick, attempt.selfFullJid)
        if (pending.activePresencePublished) terminate(attempt.room, attempt.sid, attempt.selfFullJid)
        store.reportCallError(message)
        return false
    }

    private suspend fun leavePresence(room: String, selfNick: String, selfFullJid: String?) {
        val cleared = signaling.updateMujiPresence(
            MujiPresenceUpdate(
                roomJid = room, nick = selfNick,
                active = false, preparing = false, video = false,
                // leaving the call clears all in-call state
                flags = WaddleInCallPresenceFlags(handRaised = false, muted = false),
            ),
        )
        if (!cleared) store.reportCallError("muji leave presence failed")
        presence.clearParticipant(room, selfNick, selfFullJid)
        _selfHandRaised.value = false
        _selfMuted.value = false
    }

    private suspend fun terminate(room: String, sid: String, selfFullJid: String?) {
        if (signaling.mujiSessionTerminate(room, sid)) {
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
