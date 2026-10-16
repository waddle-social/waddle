package social.waddle.android.client.session

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.DesiredPresence
import social.waddle.client.ffi.WaddleManualStatus
import social.waddle.client.ffi.WaddleStatusPreference
import java.time.Instant
import java.time.temporal.ChronoUnit

/**
 * Advertises this device's own presence (RFC 6121 §4) on every ready
 * stream: the app's [desired] automatic presence, overridden by the
 * account-wide manual status (ADR-010 decision 5).
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
 * way could leave it unknown whether the stanza went out. Its input is
 * conflated, so values that change during a send are skipped and the
 * collector always ends on the latest one.
 */
internal class SelfPresence(
    private val activeSession: ActiveSession,
    private val desired: StateFlow<DesiredPresence>,
    /** How long the first presence waits for the stored manual status. */
    private val preferenceWaitMillis: Long = PREFERENCE_WAIT_MILLIS,
) {
    /**
     * The account's presence mode, from the `urn:waddle:status-preference:0`
     * fetch on each ready stream and its live PEP updates. It outlives a
     * stream, so a reconnect advertises the last known pick at once.
     */
    private val preference = MutableStateFlow<WaddleStatusPreference>(WaddleStatusPreference.Automatic)

    private val preferenceLock = Any()

    /** Live updates seen; a fetch that started before the latest one is stale. */
    private var preferenceUpdates = 0L

    /** Collectors running, and the last automatic value one finished handling. */
    private data class Handling(val collectors: Int, val handled: DesiredPresence?)

    private val handling = MutableStateFlow(Handling(collectors = 0, handled = null))

    /**
     * Returns once a ready stream's collector has handled [automatic]
     * (advertised it, or found nothing to send), or at once while no ready
     * stream is advertising: a wake lock held for the send can then go.
     */
    suspend fun awaitHandled(automatic: DesiredPresence) {
        handling.first { it.collectors == 0 || it.handled == automatic }
    }

    /**
     * Fetch the stored manual status (waiting at most [preferenceWaitMillis]
     * for it), then launch [lease]'s collector in [attemptScope] and return
     * once its first value has been handled (sent, skipped or refused), so
     * the caller can order the rest of the ready pipeline behind it. A fetch
     * slower than the wait still lands, and the collector re-advertises.
     * Everything ends with the attempt scope; a stale lease sends nothing.
     */
    suspend fun start(attemptScope: CoroutineScope, lease: ActiveSession.OwnerLease) {
        val updatesAtFetch = synchronized(preferenceLock) { preferenceUpdates }
        val fetch = attemptScope.async { fetchPreference(lease, updatesAtFetch) }
        withTimeoutOrNull(preferenceWaitMillis) { fetch.await() }

        val firstHandled = CompletableDeferred<Unit>()
        attemptScope.launch {
            handling.update { it.copy(collectors = it.collectors + 1) }
            try {
                var advertised: DesiredPresence = DesiredPresence.None
                combine(desired, preference) { automatic, pick -> automatic to effectivePresenceOf(automatic, pick) }
                    .distinctUntilChanged()
                    .conflate()
                    .collect { (automatic, next) ->
                        // None keeps whatever this stream last advertised. The
                        // FFI cannot send `type='unavailable'`, and a lease-only
                        // stream must not flash an away presence either; it
                        // closes soon, and the server then broadcasts
                        // unavailable (RFC 6121 §4.5.2). Repeats are not
                        // re-broadcast to contacts.
                        if (next != DesiredPresence.None && next != advertised && advertise(lease, next)) {
                            advertised = next
                        }
                        handling.update { it.copy(handled = automatic) }
                        firstHandled.complete(Unit)
                    }
            } finally {
                handling.update { it.copy(collectors = it.collectors - 1) }
                firstHandled.complete(Unit)
            }
        }
        firstHandled.await()
    }

    /** Another resource published a pick (the PEP `+notify` event). */
    fun onPreferenceChanged(update: WaddleStatusPreference) {
        synchronized(preferenceLock) {
            preferenceUpdates += 1
            preference.value = update
        }
    }

    /** Sign-out: the next account starts in Automatic mode. */
    fun clear() {
        onPreferenceChanged(WaddleStatusPreference.Automatic)
    }

    private suspend fun fetchPreference(lease: ActiveSession.OwnerLease, updatesAtFetch: Long) {
        val fetched = try {
            activeSession.invokeIfCurrent(lease) { it.fetchStatusPreference() }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            // Keep the last known pick: a failed fetch is not a reset.
            return
        }
        if (fetched !is ActiveSession.LeaseInvocation.Completed) return
        synchronized(preferenceLock) {
            // A live update that arrived meanwhile is newer than this read.
            if (preferenceUpdates == updatesAtFetch) preference.value = fetched.value
        }
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
     * XEP-0082 DateTime in UTC, whole seconds. Returning to available
     * omits `<idle/>` (XEP-0319 §2.2).
     */
    private fun wireShapeOf(presence: DesiredPresence): Pair<String?, String?> = when (presence) {
        DesiredPresence.None, DesiredPresence.Available -> null to null
        DesiredPresence.Chat -> SHOW_CHAT to null
        DesiredPresence.DoNotDisturb -> SHOW_DND to null
        is DesiredPresence.Away -> SHOW_AWAY to xep0082(presence.idleSince)
        is DesiredPresence.ExtendedAway -> SHOW_EXTENDED_AWAY to xep0082(presence.idleSince)
    }

    private fun xep0082(instant: Instant?): String? = instant?.truncatedTo(ChronoUnit.SECONDS)?.toString()

    companion object {
        /** Bounds how long a slow fetch can hold up the ready pipeline. */
        const val PREFERENCE_WAIT_MILLIS = 3_000L

        private const val SHOW_CHAT = "chat"
        private const val SHOW_AWAY = "away"
        private const val SHOW_EXTENDED_AWAY = "xa"
        private const val SHOW_DND = "dnd"
    }
}

/**
 * ADR-010 precedence: a manual status wins over the automatic computation
 * on every device and carries no idle stamp (it is a choice, not
 * inactivity; Extended Away is only ever automatic). Automatic mode
 * adopts [automatic] as is. A lease-only stream ([DesiredPresence.None])
 * advertises nothing whatever the mode, so the user never flashes online
 * for a notification action.
 */
internal fun effectivePresenceOf(
    automatic: DesiredPresence,
    preference: WaddleStatusPreference,
): DesiredPresence {
    if (automatic == DesiredPresence.None) return DesiredPresence.None
    val manual = (preference as? WaddleStatusPreference.Manual)?.status ?: return automatic
    return when (manual) {
        WaddleManualStatus.AVAILABLE -> DesiredPresence.Available
        WaddleManualStatus.CHAT -> DesiredPresence.Chat
        WaddleManualStatus.AWAY -> DesiredPresence.Away(idleSince = null)
        WaddleManualStatus.DND -> DesiredPresence.DoNotDisturb
    }
}
