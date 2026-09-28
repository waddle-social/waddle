package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import social.waddle.android.client.store.ProfileStore
import social.waddle.client.ffi.WaddleAvatar

/** Outcome of one XEP-0084 avatar resolution for a bare JID. */
sealed interface AvatarLookup {
    /** An avatar is published; it is now the JID's current avatar. */
    data class Found(val avatar: WaddleAvatar) : AvatarLookup

    /** Nothing published (or §4.3 disabled); the JID shows initials. */
    data object Absent : AvatarLookup

    /** Transport/IQ failure, no session, or superseded; retried later. */
    data object Failed : AvatarLookup
}

/**
 * Read side of the peer-avatar cache for the UI. [avatars] is keyed by
 * [normalizedBareJid] only (never a nick).
 */
interface PeerAvatarSource {
    val avatars: StateFlow<Map<String, WaddleAvatar>>

    /** Fetch [jid]'s avatar unless a fresh result (or fetch) exists. */
    fun ensure(jid: String)

    /**
     * Keep [jid] fresh while a surface shows it: ensures now, then
     * revalidates on the repository's own coarse timer and on every
     * new session. Returns the release; call it when the surface leaves.
     */
    fun watch(jid: String): () -> Unit
}

/**
 * One wire resolution. [commit] runs the store projection only while
 * this attempt is still current and reports whether it did — a result
 * superseded by an `AvatarChanged` must never overwrite the newer state.
 */
internal fun interface AvatarResolver {
    suspend fun resolve(jid: String, knownId: String?, commit: (() -> Unit) -> Boolean): AvatarLookup
}

/**
 * The peer-avatar refresh policy (shared with web and iOS):
 *
 * - lazy: nothing is fetched until a surface renders the JID;
 * - one in-flight fetch per JID, at most [maxConcurrent] overall;
 * - a found avatar is revalidated after [POSITIVE_TTL_MILLIS] with the
 *   cached item ids (XEP-0084 §4.2: no data re-download when unchanged);
 * - a miss or a failure is retried after [RETRY_TTL_MILLIS];
 * - a new session ([markStale]) makes every result stale;
 * - watched JIDs (on screen) are re-checked every
 *   [REVALIDATE_TICK_MILLIS], so always-visible surfaces still pick up
 *   the TTLs; the ticker only runs while something is watched;
 * - `AvatarChanged` refetches that JID with the announced id, and a
 *   `null` id (§4.3 disable) drops to initials immediately.
 */
internal class PeerAvatarRepository(
    private val store: ProfileStore,
    private val resolver: AvatarResolver,
    private val scope: CoroutineScope,
    /** The signed-in account; its avatar bytes are never evicted. */
    private val ownJid: () -> String? = { null },
    private val clock: () -> Long = System::currentTimeMillis,
    maxConcurrent: Int = MAX_CONCURRENT_FETCHES,
) : PeerAvatarSource {
    private class Attempt(val epoch: Long) {
        var superseded = false

        /** Its JID's bytes were evicted mid-flight: record no result. */
        var evicted = false
        var job: Job? = null
    }

    private class Settled(val at: Long, val ttl: Long, val epoch: Long)

    private class Entry {
        var attempt: Attempt? = null
        var settled: Settled? = null
        var rerun = false
        var rerunKnownId: String? = null

        /** Released by its last watcher mid-flight: prune once settled. */
        var pruneWhenSettled = false
    }

    override val avatars: StateFlow<Map<String, WaddleAvatar>> = store.avatars

    private val lock = Any()
    private val entries = HashMap<String, Entry>()
    private val permits = Semaphore(maxConcurrent)
    private var currentEpoch = 0L

    /** key → number of surfaces currently watching it. */
    private val watchers = HashMap<String, Int>()
    private var ticker: Job? = null

    override fun ensure(jid: String) {
        val key = keyOf(jid) ?: return
        synchronized(lock) { ensureLocked(key) }
    }

    override fun watch(jid: String): () -> Unit {
        val key = keyOf(jid) ?: return {}
        synchronized(lock) {
            watchers[key] = (watchers[key] ?: 0) + 1
            entries[key]?.pruneWhenSettled = false
            store.markUsed(key)
            if (ticker == null) ticker = scope.launch { revalidateWatched() }
            ensureLocked(key)
        }
        var released = false
        return {
            synchronized(lock) {
                if (!released) {
                    released = true
                    unwatchLocked(key)
                }
            }
        }
    }

    private suspend fun revalidateWatched() {
        while (true) {
            delay(REVALIDATE_TICK_MILLIS)
            synchronized(lock) {
                watchers.keys.toList().forEach(::ensureLocked)
                entries.keys.toList().forEach(::pruneIfIdleLocked)
            }
        }
    }

    private fun unwatchLocked(key: String) {
        val remaining = (watchers[key] ?: return) - 1
        if (remaining > 0) {
            watchers[key] = remaining
            return
        }
        watchers -= key
        // Off screen now: its bytes may go if the cache is over budget,
        // and a result worth nothing more is forgotten.
        evictOverBudgetLocked()
        entries[key]?.let { entry -> if (entry.attempt != null) entry.pruneWhenSettled = true }
        pruneIfIdleLocked(key)
        if (watchers.isEmpty()) {
            ticker?.cancel()
            ticker = null
        }
    }

    /** Caller holds [lock]. */
    private fun ensureLocked(key: String) {
        val entry = entries.getOrPut(key, ::Entry)
        val attempt = entry.attempt
        if (attempt != null) {
            // Started before a reconnect: its answer is already stale.
            if (attempt.epoch != currentEpoch) entry.rerun = true
            return
        }
        val settled = entry.settled
        if (settled != null && settled.epoch == currentEpoch && clock() - settled.at < settled.ttl) return
        start(key, entry, knownId = null)
    }

    /**
     * XEP-0084 metadata notification for [jid]; `null` = avatar disabled.
     *
     * Lazy like everything else: only a JID that is on screen, whose
     * avatar is held, or whose fetch is in flight refetches now. For any
     * other JID (a roster contact nobody has rendered) the event only
     * marks a known result stale — a burst of contact updates must not
     * queue lookups — and the first render fetches as usual.
     */
    fun onAvatarChanged(jid: String, avatarId: String?) {
        val key = keyOf(jid) ?: return
        synchronized(lock) {
            if (avatarId == null) {
                // A disable always drops the held avatar, seen or not.
                store.clearAvatar(key)
                entries[key]?.let { entry ->
                    entry.attempt?.superseded = true
                    entry.rerun = false
                    entry.rerunKnownId = null
                    entry.settled = Settled(clock(), RETRY_TTL_MILLIS, currentEpoch)
                }
                return
            }
            val existing = entries[key]
            val live = key in watchers || key in store.avatars.value || existing?.attempt != null
            if (!live) {
                existing?.settled = null
                return
            }
            val entry = existing ?: Entry().also { entries[key] = it }
            entry.attempt?.superseded = true
            entry.settled = null
            if (entry.attempt != null) {
                entry.rerun = true
                entry.rerunKnownId = avatarId
            } else {
                start(key, entry, knownId = avatarId)
            }
        }
    }

    /** A new session bound: every settled result is stale; on-screen JIDs refetch now. */
    fun markStale() {
        synchronized(lock) {
            currentEpoch++
            watchers.keys.toList().forEach(::ensureLocked)
        }
    }

    /**
     * Sign-out/relogin: drop all results and cancel in-flight fetches.
     * Watchers belong to on-screen surfaces and survive; the next
     * [markStale] refetches them for the new session.
     */
    fun clear() {
        synchronized(lock) {
            entries.values.forEach { it.attempt?.job?.cancel() }
            entries.clear()
            currentEpoch++
        }
    }

    /** Caller holds [lock]. */
    private fun start(key: String, entry: Entry, knownId: String?) {
        val attempt = Attempt(currentEpoch)
        entry.attempt = attempt
        attempt.job = scope.launch {
            val outcome = try {
                permits.withPermit {
                    resolver.resolve(key, knownId) { projection -> commit(key, attempt, projection) }
                }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                AvatarLookup.Failed
            }
            settle(key, attempt, outcome)
        }
    }

    /**
     * Caller holds [lock]. Enforce the store's byte budget, protecting
     * watched JIDs and the own account, and forget evicted results so
     * an evicted JID is never "fresh-found" without bytes: its next
     * watch/ensure refetches.
     */
    private fun evictOverBudgetLocked() {
        val protectedJids = watchers.keys + setOfNotNull(ownJid()?.let(::normalizedBareJid))
        store.evictOverBudget(protectedJids).forEach { jid ->
            entries[jid]?.let { entry ->
                entry.settled = null
                entry.attempt?.evicted = true
            }
            pruneIfIdleLocked(jid)
        }
    }

    /**
     * Caller holds [lock]. Forget [key]'s entry when nothing depends on
     * it: no watcher, no fetch in flight or queued, and no held avatar
     * whose fresh result spares a refetch (a miss, an eviction, or an
     * expired result is worth nothing). Unshown avatarless JIDs thus do
     * not accumulate; a pruned JID simply fetches again when shown.
     */
    private fun pruneIfIdleLocked(key: String) {
        val entry = entries[key] ?: return
        if (key in watchers || entry.attempt != null || entry.rerun) return
        val settled = entry.settled
        val keepsFreshAvatar = settled != null &&
            key in store.avatars.value &&
            settled.epoch == currentEpoch &&
            clock() - settled.at < settled.ttl
        if (!keepsFreshAvatar) entries -= key
    }

    /** Test seam: JIDs the repository currently tracks. */
    internal fun trackedJidCount(): Int = synchronized(lock) { entries.size }

    private fun commit(key: String, attempt: Attempt, projection: () -> Unit): Boolean = synchronized(lock) {
        if (entries[key]?.attempt !== attempt || attempt.superseded || attempt.evicted) return@synchronized false
        projection()
        true
    }

    private fun settle(key: String, attempt: Attempt, outcome: AvatarLookup) {
        synchronized(lock) {
            val entry = entries[key] ?: return
            if (entry.attempt !== attempt) return
            entry.attempt = null
            // An eviction mid-flight leaves the JID unsettled: whatever
            // this attempt saw, its next watch/ensure must fetch again.
            if (!attempt.superseded && !attempt.evicted) {
                val ttl = if (outcome is AvatarLookup.Found) POSITIVE_TTL_MILLIS else RETRY_TTL_MILLIS
                entry.settled = Settled(clock(), ttl, attempt.epoch)
                if (outcome is AvatarLookup.Found) evictOverBudgetLocked()
            }
            if (entry.rerun) {
                val knownId = entry.rerunKnownId
                entry.rerun = false
                entry.rerunKnownId = null
                start(key, entry, knownId)
            } else if (entry.pruneWhenSettled) {
                entry.pruneWhenSettled = false
                pruneIfIdleLocked(key)
            }
        }
    }

    private fun keyOf(jid: String): String? = normalizedBareJid(jid).takeIf { it.isNotEmpty() }

    companion object {
        const val MAX_CONCURRENT_FETCHES = 4

        /** Found avatars are revalidated (id-only when unchanged) after 45 min. */
        const val POSITIVE_TTL_MILLIS = 45L * 60 * 1000

        /** Misses and failures are retried after 10 min. */
        const val RETRY_TTL_MILLIS = 10L * 60 * 1000

        /** Coarse re-check of watched JIDs; the TTLs decide what refetches. */
        const val REVALIDATE_TICK_MILLIS = 60L * 1000
    }
}
