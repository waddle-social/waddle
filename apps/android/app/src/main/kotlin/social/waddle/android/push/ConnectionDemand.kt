package social.waddle.android.push

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.transformLatest
import kotlinx.coroutines.flow.update
import social.waddle.android.client.DesiredPresence
import social.waddle.android.client.calls.CallState
import java.time.Instant
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Whether anything needs the XMPP stream right now. The session holds it
 * open only while this is true, because the server suppresses DM pushes
 * while it sees a session:
 *
 * - the app is in the foreground ([setProcessStarted]);
 * - the user stays connected (no push delivery, or the explicit setting);
 * - a call is ringing or in progress;
 * - a notification action is delivering (an inline reply, a mark-read),
 *   which holds a [Lease] until it is done.
 *
 * Demand rises at once and falls after [graceMillis], so switching apps
 * briefly does not drop and rebuild the stream. Every input starts false:
 * a cold start in the background (a push wake, boot) does not connect
 * until something asks for the stream.
 *
 * The same inputs decide the device's own [presence] (ADR-010 Automatic
 * mode): see [desiredPresenceOf].
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionDemand(
    scope: CoroutineScope,
    private val graceMillis: Long = GRACE_MILLIS,
    private val now: () -> Instant = Instant::now,
) {
    private val attention = MutableStateFlow(Attention(foreground = false, inCall = false, absentSince = null))
    private val stayConnected = MutableStateFlow(false)
    private val callActive = MutableStateFlow(false)
    private val leases = MutableStateFlow(0)

    val wanted: StateFlow<Boolean> = combine(
        attention.map { it.foreground },
        stayConnected,
        callActive,
        leases,
    ) { started, stay, call, held -> started || stay || call || held > 0 }
        .distinctUntilChanged()
        .transformLatest { needed ->
            if (!needed) delay(graceMillis)
            emit(needed)
        }
        .stateIn(scope, SharingStarted.Eagerly, initialValue = false)

    /** What this device advertises on the stream while [wanted] holds it. */
    val presence: StateFlow<DesiredPresence> = combine(
        attention,
        stayConnected,
        callActive,
    ) { seen, stay, call -> desiredPresenceOf(seen, holdsStream = stay || call) }
        .stateIn(scope, SharingStarted.Eagerly, initialValue = DesiredPresence.None)

    fun setProcessStarted(started: Boolean) {
        updateAttention { it.copy(foreground = started) }
    }

    fun setStayConnected(stay: Boolean) {
        stayConnected.value = stay
    }

    /** A call is ringing or live: either needs signalling over the stream. */
    fun setCallActive(active: Boolean) {
        callActive.value = active
    }

    /**
     * A call is live (not merely ringing). ADR-010: a live call is proof
     * of presence and pauses auto-away, even with the app in the background.
     */
    fun setInCall(inCall: Boolean) {
        updateAttention { it.copy(inCall = inCall) }
    }

    /** Apply [transform], stamping the moment the user stops being present. */
    private fun updateAttention(transform: (Attention) -> Attention) {
        attention.update { old ->
            val next = transform(old)
            when {
                next.present -> next.copy(absentSince = null)
                old.present -> next.copy(absentSince = now())
                else -> next
            }
        }
    }

    /** Hold the stream until [Lease.release]; releasing twice is harmless. */
    fun acquire(): Lease {
        leases.update { it + 1 }
        return Lease()
    }

    inner class Lease internal constructor() {
        private val released = AtomicBoolean(false)

        fun release() {
            if (released.compareAndSet(false, true)) leases.update { it - 1 }
        }
    }

    companion object {
        /**
         * How long the stream outlives the last thing that needed it:
         * covers a quick app switch, and stays well under the time after
         * which Android freezes a cached process (a frozen process with an
         * open socket would keep the server from pushing).
         */
        const val GRACE_MILLIS = 5_000L
    }
}

/**
 * Whether the user is present on this device, and since when they are
 * not. [absentSince] is `null` while present, and also after a cold start
 * in the background, where the last interaction is unknown.
 */
internal data class Attention(
    val foreground: Boolean,
    val inCall: Boolean,
    val absentSince: Instant?,
) {
    val present: Boolean get() = foreground || inCall
}

/**
 * The device's own presence in ADR-010 Automatic mode (no manual status
 * on Android yet):
 *
 * - present (app in the foreground, or in a live call) → available;
 * - absent, but the stream is held for the user (stay-connected, a ringing
 *   call) → away, with XEP-0319 idle since the user left;
 * - otherwise → [DesiredPresence.None]: the stream exists only for a
 *   notification action or a queued delivery (or is about to close), and
 *   advertising it would make the user flash online.
 */
internal fun desiredPresenceOf(attention: Attention, holdsStream: Boolean): DesiredPresence = when {
    attention.present -> DesiredPresence.Available
    holdsStream -> DesiredPresence.Away(attention.absentSince)
    else -> DesiredPresence.None
}

/**
 * Whether a call in this state needs signalling over the stream. An
 * ended call waits in its slot until the UI dismisses it, which may be
 * never; it must not hold the stream.
 */
fun CallState.needsStream(): Boolean = this !is CallState.Idle && this !is CallState.Ended
