package social.waddle.android.feature.call

import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.channelFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.client.normalizedBareJid

/**
 * How long a speaker stays lit after LiveKit drops them from the active
 * set. Speech has gaps between words; without the hold the ember ring
 * would flicker on every breath.
 */
const val SPEAKING_RELEASE_MILLIS = 300L

/**
 * LiveKit identities (full JIDs, local participant included) to the
 * normalised bare JIDs the rest of the app keys people by. Two devices
 * of one account collapse to one person; identities that are not JIDs
 * are dropped rather than guessed at.
 */
fun speakingBareJidsOf(identities: Collection<String>): Set<String> =
    identities.mapNotNullTo(LinkedHashSet()) { identity ->
        normalizedBareJid(identity).takeIf { it.isNotEmpty() && '@' in it }
    }

/**
 * Debounce only the off-transition, per member: a member entering the
 * set shows at once, a member leaving it stays for [holdMillis] and is
 * kept if it comes back inside that window. Emits only on change.
 */
fun Flow<Set<String>>.releasingAfter(holdMillis: Long): Flow<Set<String>> = channelFlow {
    val state = SpeakerRelease(emit = { send(it) })
    // Release timers are children of the producer: the flow completes
    // only after the last pending release has landed.
    collect { speakers ->
        state.lock.withLock {
            for (gone in state.take(speakers)) {
                state.track(
                    gone,
                    launch {
                        delay(holdMillis)
                        state.lock.withLock { state.release(gone) }
                    },
                )
            }
        }
    }
}

/** [releasingAfter]'s state; every call but [lock] runs under [lock]. */
private class SpeakerRelease(private val emit: suspend (Set<String>) -> Unit) {
    val lock = Mutex()
    private var latest: Set<String> = emptySet()
    private var shown: Set<String>? = null
    private val releases = HashMap<String, Job>()

    /**
     * Take the newest active set: speakers show at once, any pending
     * release for them is cancelled. Returns the members that just went
     * quiet and still need a release timer.
     */
    suspend fun take(speakers: Set<String>): Set<String> {
        latest = speakers
        speakers.forEach { releases.remove(it)?.cancel() }
        val current = shown.orEmpty()
        publish(current + speakers)
        return current - speakers - releases.keys
    }

    fun track(member: String, timer: Job) {
        releases[member] = timer
    }

    /** The hold ran out: drop [member] unless they spoke again. */
    suspend fun release(member: String) {
        releases.remove(member)
        if (member !in latest) publish(shown.orEmpty() - member)
    }

    private suspend fun publish(next: Set<String>) {
        if (next != shown) {
            shown = next
            emit(next)
        }
    }
}
