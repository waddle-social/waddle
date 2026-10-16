package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import social.waddle.android.client.DesiredPresence
import java.time.temporal.ChronoUnit

/**
 * Advertises this device's own presence (RFC 6121 §4) on every ready
 * stream, following [desired].
 *
 * Android never resumes a stream, so each ready stream starts without
 * presence: until it sends some, the server routes bare-JID messages
 * to it only when no other resource is available (RFC 6121 §8.5.2.1.1)
 * and holds the XEP-0160 offline queue back (§3 step 5 flushes it on
 * the first non-negative available presence). [start] therefore runs
 * at the head of the ready pipeline, before queued sends and room
 * joins.
 *
 * Each attempt gets ONE collector, so presence stanzas leave in order
 * ([ActiveSession.invokeIfCurrent] does not serialise its callers).
 * It uses `collect`, not `collectLatest`: cancelling an FFI send part
 * way could leave it unknown whether the stanza went out. [desired] is
 * a [StateFlow], so values that change during a send conflate and the
 * collector always ends on the latest one.
 */
internal class SelfPresence(
    private val activeSession: ActiveSession,
    private val desired: StateFlow<DesiredPresence>,
) {
    /**
     * Launch [lease]'s collector in [attemptScope] and return once its
     * first value has been handled (sent, skipped or refused), so the
     * caller can order the rest of the ready pipeline behind it. The
     * collector ends with the attempt scope; a stale lease sends nothing.
     */
    suspend fun start(attemptScope: CoroutineScope, lease: ActiveSession.OwnerLease) {
        val firstHandled = CompletableDeferred<Unit>()
        attemptScope.launch {
            try {
                var advertised: DesiredPresence = DesiredPresence.None
                desired.collect { next ->
                    // None keeps whatever this stream last advertised. The FFI
                    // cannot send `type='unavailable'`, and a lease-only stream
                    // must not flash an away presence either; it closes soon,
                    // and the server then broadcasts unavailable (RFC 6121
                    // §4.5.2). Repeats are not re-broadcast to contacts.
                    if (next != DesiredPresence.None && next != advertised && advertise(lease, next)) {
                        advertised = next
                    }
                    firstHandled.complete(Unit)
                }
            } finally {
                firstHandled.complete(Unit)
            }
        }
        firstHandled.await()
    }

    /** One presence stanza on [lease]'s transport; `true` once it was handed to the client. */
    private suspend fun advertise(lease: ActiveSession.OwnerLease, presence: DesiredPresence): Boolean = try {
        val (show, idleSince) = wireShapeOf(presence)
        activeSession.invokeIfCurrent(lease) { client ->
            client.sendPresence(status = null, show = show, idleSince = idleSince)
        } is ActiveSession.LeaseInvocation.Completed
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        false
    }

    /**
     * `(show, idleSince)` for the FFI: an RFC 6121 §4.7.2.1 `<show/>`
     * token (`null` is plain available) and a XEP-0319 `since`, a
     * XEP-0082 DateTime in UTC. Returning to available omits `<idle/>`
     * (XEP-0319 §2.2).
     */
    private fun wireShapeOf(presence: DesiredPresence): Pair<String?, String?> = when (presence) {
        DesiredPresence.None, DesiredPresence.Available -> null to null
        is DesiredPresence.Away -> SHOW_AWAY to presence.idleSince?.truncatedTo(ChronoUnit.SECONDS)?.toString()
    }

    private companion object {
        const val SHOW_AWAY = "away"
    }
}
