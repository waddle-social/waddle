package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
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
 * Read side of the peer-avatar cache for the UI: [avatars] is keyed by
 * bare JID only (never a nick); [ensure] is the lazy first-render
 * trigger; [epoch] ticks on every new session so on-screen avatars
 * re-ensure after a reconnect.
 */
interface PeerAvatarSource {
    val avatars: StateFlow<Map<String, WaddleAvatar>>
    val epoch: StateFlow<Long>

    fun ensure(jid: String)
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
 * - `AvatarChanged` refetches that JID with the announced id, and a
 *   `null` id (§4.3 disable) drops to initials immediately.
 */
internal class PeerAvatarRepository(
    override val avatars: StateFlow<Map<String, WaddleAvatar>>,
    private val resolver: AvatarResolver,
    private val clearAvatar: (String) -> Unit,
    private val scope: CoroutineScope,
    private val clock: () -> Long = System::currentTimeMillis,
    maxConcurrent: Int = MAX_CONCURRENT_FETCHES,
) : PeerAvatarSource {
    private class Attempt(val epoch: Long) {
        var superseded = false
        var job: Job? = null
    }

    private class Settled(val at: Long, val ttl: Long, val epoch: Long)

    private class Entry {
        var attempt: Attempt? = null
        var settled: Settled? = null
        var rerun = false
        var rerunKnownId: String? = null
    }

    private val lock = Any()
    private val entries = HashMap<String, Entry>()
    private val permits = Semaphore(maxConcurrent)
    private var currentEpoch = 0L
    private val _epoch = MutableStateFlow(0L)

    override val epoch: StateFlow<Long> = _epoch.asStateFlow()

    override fun ensure(jid: String) {
        val key = keyOf(jid) ?: return
        synchronized(lock) {
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
    }

    /** XEP-0084 metadata notification for [jid]; `null` = avatar disabled. */
    fun onAvatarChanged(jid: String, avatarId: String?) {
        val key = keyOf(jid) ?: return
        synchronized(lock) {
            val entry = entries.getOrPut(key, ::Entry)
            entry.attempt?.superseded = true
            if (avatarId == null) {
                entry.rerun = false
                entry.rerunKnownId = null
                entry.settled = Settled(clock(), RETRY_TTL_MILLIS, currentEpoch)
                clearAvatar(key)
                return
            }
            entry.settled = null
            if (entry.attempt != null) {
                entry.rerun = true
                entry.rerunKnownId = avatarId
            } else {
                start(key, entry, knownId = avatarId)
            }
        }
    }

    /** A new session bound: every settled result is stale. */
    fun markStale() {
        synchronized(lock) {
            currentEpoch++
            _epoch.value = currentEpoch
        }
    }

    /** Sign-out/relogin: drop all state and cancel in-flight fetches. */
    fun clear() {
        synchronized(lock) {
            entries.values.forEach { it.attempt?.job?.cancel() }
            entries.clear()
            currentEpoch++
            _epoch.value = currentEpoch
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

    private fun commit(key: String, attempt: Attempt, projection: () -> Unit): Boolean = synchronized(lock) {
        if (entries[key]?.attempt !== attempt || attempt.superseded) return@synchronized false
        projection()
        true
    }

    private fun settle(key: String, attempt: Attempt, outcome: AvatarLookup) {
        synchronized(lock) {
            val entry = entries[key] ?: return
            if (entry.attempt !== attempt) return
            entry.attempt = null
            if (!attempt.superseded) {
                val ttl = if (outcome is AvatarLookup.Found) POSITIVE_TTL_MILLIS else RETRY_TTL_MILLIS
                entry.settled = Settled(clock(), ttl, attempt.epoch)
            }
            if (entry.rerun) {
                val knownId = entry.rerunKnownId
                entry.rerun = false
                entry.rerunKnownId = null
                start(key, entry, knownId)
            }
        }
    }

    private fun keyOf(jid: String): String? = bareJid(jid.trim()).takeIf { it.isNotEmpty() }

    companion object {
        const val MAX_CONCURRENT_FETCHES = 4

        /** Found avatars are revalidated (id-only when unchanged) after 45 min. */
        const val POSITIVE_TTL_MILLIS = 45L * 60 * 1000

        /** Misses and failures are retried after 10 min. */
        const val RETRY_TTL_MILLIS = 10L * 60 * 1000
    }
}
