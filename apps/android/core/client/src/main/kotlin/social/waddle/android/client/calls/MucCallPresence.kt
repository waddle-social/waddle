package social.waddle.android.client.calls

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.client.ffi.WaddlePresence

/** Aggregated media the room's group call currently advertises. */
data class MucCallMedia(val audio: Boolean, val video: Boolean)

/** `room@muc.host/nick` → `room@muc.host` normalized for map keys. */
internal fun normalizeMucCallRoomJid(roomJid: String): String =
    roomJid.substringBefore('/').trim().lowercase()

/**
 * Case-normalized full-JID identity (web `fullJidIdentityKey`): bare
 * part lowercased, resource kept case-sensitive per RFC 7622.
 */
internal fun fullJidIdentityKey(fullJid: String?): String {
    val trimmed = fullJid?.trim().orEmpty()
    if (trimmed.isEmpty()) return ""
    val separator = trimmed.indexOf('/')
    if (separator < 0) return trimmed.lowercase()
    return trimmed.substring(0, separator).lowercase() + "/" + trimmed.substring(separator + 1)
}

/**
 * One tracked occupant of a room's Muji call: the MUC nick plus the
 * XEP-0045 real full JID when the room exposes it, so same-account
 * resources sharing a nick stay distinguishable (web
 * `preparingParticipantKey` / `activeParticipantKey`).
 */
private data class ParticipantKey(val nick: String, val realJid: String)

/** A registered one-shot preparing-echo waiter (web `awaitPreparingEcho` handle). */
class PreparingEchoWaiter internal constructor(
    internal val key: ParticipantWaiterKey,
    internal val deferred: CompletableDeferred<Unit>,
)

/** Key of a preparation waiter: room + nick + optional real-JID identity. */
internal data class ParticipantWaiterKey(
    val roomJid: String,
    val nick: String,
    val realJid: String,
)

/**
 * XEP-0272 Muji presence bookkeeping, the Kotlin port of the web's
 * muc-call-presence.ts: per-room participants/owners/media derived
 * from inbound `<muji/>` MUC presence, plus the §Joining preparation
 * waiters `beginMucCall` blocks on.
 *
 * Rules (XEP-0272 §Joining and §Leaving):
 * - Available presence with active `<content/>` Muji → add nick.
 * - `<preparing/>`-only Muji is a setup signal, never membership.
 * - Available presence WITHOUT `<muji/>` → remove nick (the absence
 *   of the element IS the leave marker).
 * - Unavailable presence → remove nick (occupant left the room).
 *
 * Robust against duplicate / replayed presences: re-adding an
 * already-present occupant and removing a never-added one are no-ops.
 */
class MucCallPresence internal constructor() {
    private val lock = Any()

    private val _participants = MutableStateFlow<Map<String, Set<String>>>(emptyMap())

    /** room → nicks currently advertising active Muji (insertion order). */
    val participants: StateFlow<Map<String, Set<String>>> = _participants.asStateFlow()

    private val _owners = MutableStateFlow<Map<String, Map<String, String?>>>(emptyMap())

    /** room → (nick → XEP-0045 real full JID when the room exposes it). */
    val owners: StateFlow<Map<String, Map<String, String?>>> = _owners.asStateFlow()

    private val _media = MutableStateFlow<Map<String, MucCallMedia>>(emptyMap())

    /** room → aggregated call media across the active participants. */
    val media: StateFlow<Map<String, MucCallMedia>> = _media.asStateFlow()

    private val _raisedHands = MutableStateFlow<Map<String, Set<String>>>(emptyMap())

    /** room → nicks with the `urn:waddle:in-call:0` raised-hand marker. */
    val raisedHands: StateFlow<Map<String, Set<String>>> = _raisedHands.asStateFlow()

    private val _mutedNicks = MutableStateFlow<Map<String, Set<String>>>(emptyMap())

    /** room → nicks self-reporting the `urn:waddle:in-call:0` muted marker. */
    val mutedNicks: StateFlow<Map<String, Set<String>>> = _mutedNicks.asStateFlow()

    // Guarded by [lock]: raw per-room active/preparing sets keyed by
    // (nick, realJid) plus per-participant media, from which the public
    // flows above are re-projected after every mutation.
    private val activeKeys = LinkedHashMap<String, MutableList<ParticipantKey>>()
    private val activeMedia = HashMap<String, MutableMap<ParticipantKey, MucCallMedia>>()
    private val preparingKeys = LinkedHashMap<String, MutableList<ParticipantKey>>()

    private val prepareEchoWaiters = HashMap<ParticipantWaiterKey, CompletableDeferred<Unit>>()
    private val noOtherPreparingWaiters = HashMap<ParticipantWaiterKey, CompletableDeferred<Unit>>()

    /**
     * One inbound Muji presence reduced to the membership decision the
     * apply pass executes under the lock.
     */
    private class ParsedMujiPresence(
        val roomJid: String,
        val nick: String,
        val realJid: String?,
        val hasPreparing: Boolean,
        /** Non-null when the occupant advertises active call contents. */
        val activeMedia: MucCallMedia?,
        val shouldClear: Boolean,
        val handRaised: Boolean = false,
        val muted: Boolean = false,
    )

    private fun parseMujiPresence(presence: WaddlePresence): ParsedMujiPresence? {
        val from = presence.from ?: return null
        val slash = from.indexOf('/')
        if (slash < 0) return null
        val roomJid = normalizeMucCallRoomJid(from.substring(0, slash))
        val nick = from.substring(slash + 1)
        if (roomJid.isEmpty() || nick.isEmpty()) return null
        val unavailable = presence.presenceType == "unavailable"
        val muji = presence.muji
        return ParsedMujiPresence(
            roomJid = roomJid,
            nick = nick,
            realJid = presence.mucJid,
            hasPreparing = !unavailable && muji?.preparing == true,
            activeMedia = muji
                ?.takeIf { !unavailable && it.active }
                ?.let { MucCallMedia(audio = it.audio, video = it.video) },
            shouldClear = unavailable || muji == null || (!muji.preparing && !muji.active),
            handRaised = presence.handRaised,
            muted = presence.muted,
        )
    }

    /**
     * Apply one inbound MUC presence. Fires any registered preparing
     * waiters after the preparing set is updated but BEFORE the active
     * view changes, so `beginMucCall` unblocks deterministically (web
     * `applyMucCallPresence`).
     */
    fun applyMucCallPresence(presence: WaddlePresence) {
        val parsed = parseMujiPresence(presence) ?: return
        val completions = mutableListOf<CompletableDeferred<Unit>>()
        synchronized(lock) { applyLocked(parsed, completions) }
        completions.forEach { it.complete(Unit) }
    }

    private fun applyLocked(
        parsed: ParsedMujiPresence,
        completions: MutableList<CompletableDeferred<Unit>>,
    ) {
        val roomJid = parsed.roomJid
        val nick = parsed.nick
        if (parsed.hasPreparing) {
            addPreparingLocked(roomJid, nick, parsed.realJid)
            echoWaiterKey(roomJid, nick, parsed.realJid).let { key ->
                prepareEchoWaiters.remove(key)?.let(completions::add)
            }
        } else {
            removeParticipantLocked(
                preparingKeys, roomJid, nick, parsed.realJid,
                includeAggregate = parsed.realJid == null,
            )
        }
        collectSatisfiedNoOtherPreparingLocked(roomJid, completions)
        if (parsed.activeMedia != null) {
            addActiveLocked(roomJid, nick, parsed.realJid, parsed.activeMedia)
            setInCallFlagsLocked(roomJid, nick, parsed.handRaised, parsed.muted)
        } else if (parsed.shouldClear) {
            removeActiveLocked(roomJid, nick, parsed.realJid, includeAggregate = parsed.realJid == null)
            clearInCallFlagsLocked(roomJid, nick)
        }
    }

    /**
     * Drop one occupant from the active view without an inbound
     * presence — the local self-removal on leave/rollback (web
     * `clearMucCallParticipant`).
     */
    fun clearParticipant(roomJid: String, nick: String, realJid: String? = null) {
        val room = normalizeMucCallRoomJid(roomJid)
        if (room.isEmpty() || nick.isEmpty()) return
        synchronized(lock) {
            removeActiveLocked(room, nick, realJid, includeAggregate = true)
            clearInCallFlagsLocked(room, nick)
        }
    }

    /**
     * Register the XEP-0272 §Joining preparing-echo waiter. MUST be
     * called BEFORE emitting the preparing presence so a fast MUC echo
     * cannot fire before the listener exists. Only one waiter per
     * participant exists at a time; a replacement cancels the previous.
     */
    fun registerPreparingEchoWaiter(
        roomJid: String,
        nick: String,
        selfFullJid: String?,
    ): PreparingEchoWaiter {
        val key = echoWaiterKey(normalizeMucCallRoomJid(roomJid), nick, selfFullJid)
        val deferred = CompletableDeferred<Unit>()
        val previous = synchronized(lock) { prepareEchoWaiters.put(key, deferred) }
        previous?.cancel(CancellationException("replaced by a new Muji preparing echo waiter"))
        return PreparingEchoWaiter(key, deferred)
    }

    /**
     * Await the MUC's rebroadcast of our preparing presence (XEP-0272
     * §Joining MUST). `false` on timeout or waiter cancellation — the
     * caller rolls the setup back.
     */
    suspend fun awaitPreparingEcho(waiter: PreparingEchoWaiter, timeoutMillis: Long): Boolean {
        val completed = awaitDeferred(waiter.deferred, timeoutMillis)
        if (!completed) {
            synchronized(lock) {
                if (prepareEchoWaiters[waiter.key] === waiter.deferred) {
                    prepareEchoWaiters.remove(waiter.key)
                }
            }
        }
        return completed
    }

    /**
     * Wait until no OTHER occupant of [roomJid] advertises
     * `<preparing/>` (XEP-0272 §Joining: wait for peers to finish
     * preparation before declaring contents). Resolves immediately when
     * none is preparing; `false` on timeout or cancellation.
     */
    suspend fun awaitNoOtherPreparing(
        roomJid: String,
        selfNick: String,
        selfFullJid: String?,
        timeoutMillis: Long,
    ): Boolean {
        val room = normalizeMucCallRoomJid(roomJid)
        val key = echoWaiterKey(room, selfNick, selfFullJid)
        val deferred = CompletableDeferred<Unit>()
        val previous = synchronized(lock) {
            if (!hasOtherPreparingLocked(room, selfNick, selfFullJid)) return true
            noOtherPreparingWaiters.put(key, deferred)
        }
        previous?.cancel(CancellationException("replaced by a new Muji preparation waiter"))
        val completed = awaitDeferred(deferred, timeoutMillis)
        if (!completed) {
            synchronized(lock) {
                if (noOtherPreparingWaiters[key] === deferred) noOtherPreparingWaiters.remove(key)
            }
        }
        return completed
    }

    /**
     * Cancel any pending preparation waiters for this participant — the
     * teardown path for an abandoned call attempt, so a later presence
     * echo cannot unblock a dead setup.
     */
    fun cancelPreparationWaiters(roomJid: String, nick: String) {
        val room = normalizeMucCallRoomJid(roomJid)
        val cancelled = synchronized(lock) {
            val matching = (prepareEchoWaiters.keys + noOtherPreparingWaiters.keys)
                .filter { it.roomJid == room && it.nick == nick }
            matching.mapNotNull { key ->
                prepareEchoWaiters.remove(key) ?: noOtherPreparingWaiters.remove(key)
            }
        }
        cancelled.forEach { it.cancel(CancellationException("Muji preparation wait cancelled")) }
    }

    /** Forget everything — logout/disconnect reset (web `clearMucCallParticipants`). */
    fun clear() {
        val cancelled = synchronized(lock) {
            activeKeys.clear()
            activeMedia.clear()
            preparingKeys.clear()
            _participants.value = emptyMap()
            _owners.value = emptyMap()
            _media.value = emptyMap()
            _raisedHands.value = emptyMap()
            _mutedNicks.value = emptyMap()
            val waiters = prepareEchoWaiters.values.toList() + noOtherPreparingWaiters.values
            prepareEchoWaiters.clear()
            noOtherPreparingWaiters.clear()
            waiters
        }
        cancelled.forEach {
            it.cancel(CancellationException("Muji preparation wait cancelled while clearing call state"))
        }
    }

    // ── Internals (all *Locked helpers require [lock]) ───────────────────────

    private suspend fun awaitDeferred(deferred: CompletableDeferred<Unit>, timeoutMillis: Long): Boolean =
        withTimeoutOrNull(timeoutMillis) {
            try {
                deferred.await()
                true
            } catch (cancellation: CancellationException) {
                // A cancelled DEFERRED (waiter torn down) is a plain
                // failure; cancellation of the CALLING coroutine must
                // still propagate.
                if (deferred.isCancelled) false else throw cancellation
            }
        } ?: false

    private fun echoWaiterKey(room: String, nick: String, realJid: String?): ParticipantWaiterKey =
        ParticipantWaiterKey(room, nick, fullJidIdentityKey(realJid))

    private fun participantKey(nick: String, realJid: String?): ParticipantKey =
        ParticipantKey(nick, fullJidIdentityKey(realJid))

    private fun addPreparingLocked(room: String, nick: String, realJid: String?) {
        val keys = preparingKeys.getOrPut(room) { mutableListOf() }
        val key = participantKey(nick, realJid)
        if (key !in keys) keys += key
    }

    private fun hasOtherPreparingLocked(room: String, selfNick: String, selfFullJid: String?): Boolean {
        val selfKey = participantKey(selfNick, selfFullJid)
        return (preparingKeys[room] ?: emptyList<ParticipantKey>()).any { key ->
            if (key.nick != selfNick) return@any true
            if (selfKey.realJid.isNotEmpty() && key.realJid.isNotEmpty()) {
                key.realJid != selfKey.realJid
            } else {
                !(selfKey.realJid.isEmpty() && key.realJid.isEmpty())
            }
        }
    }

    private fun collectSatisfiedNoOtherPreparingLocked(
        room: String,
        completions: MutableList<CompletableDeferred<Unit>>,
    ) {
        val satisfied = noOtherPreparingWaiters.keys.filter { key ->
            key.roomJid == room && !hasOtherPreparingLocked(room, key.nick, key.realJid)
        }
        satisfied.forEach { key -> noOtherPreparingWaiters.remove(key)?.let(completions::add) }
    }

    private fun addActiveLocked(room: String, nick: String, realJid: String?, media: MucCallMedia) {
        val keys = activeKeys.getOrPut(room) { mutableListOf() }
        val key = participantKey(nick, realJid)
        if (key !in keys) keys += key
        activeMedia.getOrPut(room) { mutableMapOf() }[key] = media
        publishRoomLocked(room)
    }

    private fun removeActiveLocked(room: String, nick: String, realJid: String?, includeAggregate: Boolean) {
        if (removeParticipantLocked(activeKeys, room, nick, realJid, includeAggregate) { removed ->
                activeMedia[room]?.let { byKey ->
                    removed.forEach(byKey::remove)
                    if (byKey.isEmpty()) activeMedia.remove(room)
                }
            }
        ) {
            publishRoomLocked(room)
        }
    }

    /**
     * Shared removal semantics (web `removeActiveParticipant` /
     * `removePreparingParticipant`): without a real JID every key with
     * the nick goes; with one, only the exact-identity key goes — plus
     * the identity-less aggregate key when [includeAggregate].
     */
    private fun removeParticipantLocked(
        byRoom: MutableMap<String, MutableList<ParticipantKey>>,
        room: String,
        nick: String,
        realJid: String?,
        includeAggregate: Boolean,
        onRemoved: (List<ParticipantKey>) -> Unit = {},
    ): Boolean {
        val keys = byRoom[room] ?: return false
        val normalizedReal = fullJidIdentityKey(realJid)
        val removed = keys.filter { key ->
            key.nick == nick && (
                normalizedReal.isEmpty() ||
                    key.realJid == normalizedReal ||
                    (includeAggregate && key.realJid.isEmpty())
                )
        }
        if (removed.isEmpty()) return false
        keys.removeAll(removed)
        if (keys.isEmpty()) byRoom.remove(room)
        onRemoved(removed)
        return true
    }

    /** Re-project the public flows for one room from the raw active view. */
    private fun publishRoomLocked(room: String) {
        val keys = activeKeys[room] ?: emptyList<ParticipantKey>()
        if (keys.isEmpty()) {
            _participants.value = _participants.value - room
            _owners.value = _owners.value - room
            _media.value = _media.value - room
            return
        }
        val nicks = LinkedHashSet<String>()
        val ownersByNick = LinkedHashMap<String, String?>()
        var audio = false
        var video = false
        val mediaByKey = activeMedia[room] ?: emptyMap<ParticipantKey, MucCallMedia>()
        for (key in keys) {
            nicks += key.nick
            ownersByNick[key.nick] = key.realJid.ifEmpty { null }
            val media = mediaByKey[key] ?: MucCallMedia(audio = true, video = false)
            audio = audio || media.audio
            video = video || media.video
        }
        _participants.value = _participants.value + (room to nicks)
        _owners.value = _owners.value + (room to ownersByNick)
        // Active Muji implies at least one audio content (XEP-0272), so
        // a non-empty room always aggregates audio=true (web parity).
        _media.value = _media.value + (room to MucCallMedia(audio = audio || keys.isNotEmpty(), video = video))
    }

    private fun setInCallFlagsLocked(room: String, nick: String, handRaised: Boolean, muted: Boolean) {
        _raisedHands.value = _raisedHands.value.withMembership(room, nick, handRaised)
        _mutedNicks.value = _mutedNicks.value.withMembership(room, nick, muted)
    }

    private fun clearInCallFlagsLocked(room: String, nick: String) {
        _raisedHands.value = _raisedHands.value.withMembership(room, nick, present = false)
        _mutedNicks.value = _mutedNicks.value.withMembership(room, nick, present = false)
    }
}

private fun Map<String, Set<String>>.withMembership(
    room: String,
    nick: String,
    present: Boolean,
): Map<String, Set<String>> {
    val current = this[room] ?: emptySet()
    val next = if (present) current + nick else current - nick
    return when {
        next == current -> this
        next.isEmpty() -> this - room
        else -> this + (room to next)
    }
}
