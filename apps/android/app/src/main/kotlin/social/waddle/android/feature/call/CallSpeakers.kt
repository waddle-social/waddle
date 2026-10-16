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
    val lock = Mutex()
    var latest = emptySet<String>()
    var shown: Set<String>? = null
    val releases = HashMap<String, Job>()

    suspend fun publish(next: Set<String>) {
        if (next != shown) {
            shown = next
            send(next)
        }
    }

    // Release timers are children of the producer: the flow completes
    // only after the last pending release has landed.
    collect { speakers ->
        lock.withLock {
            latest = speakers
            speakers.forEach { releases.remove(it)?.cancel() }
            val current = shown.orEmpty()
            for (gone in current - speakers) {
                if (gone in releases) continue
                releases[gone] = launch {
                    delay(holdMillis)
                    lock.withLock {
                        releases.remove(gone)
                        if (gone !in latest) publish(shown.orEmpty() - gone)
                    }
                }
            }
            publish(current + speakers)
        }
    }
}
