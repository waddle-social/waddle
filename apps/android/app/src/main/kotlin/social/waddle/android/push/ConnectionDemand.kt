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
import java.time.Duration
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
 * mode): see [AutoAway].
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionDemand(
    scope: CoroutineScope,
    private val graceMillis: Long = GRACE_MILLIS,
    /** Wall clock for the XEP-0319 idle stamp; tests tie it to virtual time. */
    private val now: () -> Instant = Instant::now,
    private val autoAway: AutoAway = AutoAway(),
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

    /**
     * What this device advertises on the stream while [wanted] holds it,
     * stepping through [autoAway] while the user is away. Any change of
     * input (the app returning to the foreground) cancels the pending step.
     */
    val presence: StateFlow<DesiredPresence> = combine(
        attention,
        stayConnected,
        callActive,
    ) { seen, stay, call -> seen to (stay || call) }
        .distinctUntilChanged()
        .transformLatest { (seen, holdsStream) ->
            while (true) {
                val at = now()
                emit(autoAway.presenceOf(seen, holdsStream, at))
                val next = autoAway.nextStepAt(seen, holdsStream, at) ?: break
                delay(millisUntil(at, next))
            }
        }
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
 * on Android yet), as the web's auto-away does it:
 *
 * - present (app in the foreground, or in a live call, which ADR-010 says
 *   pauses auto-away) → available;
 * - absent, but the stream is held for the user (stay-connected, a ringing
 *   call) → available for [awayAfter], then away, then extended away after
 *   [extendedAwayAfter], both stamped with XEP-0319 idle since the user
 *   left. A cold start in the background has no known departure: away at
 *   once, without an idle stamp;
 * - otherwise → [DesiredPresence.None]: the stream exists only for a
 *   notification action or a queued delivery (or is about to close), and
 *   advertising it would make the user flash online.
 */
class AutoAway(
    private val awayAfter: Duration = Duration.ofMinutes(AWAY_AFTER_MINUTES),
    private val extendedAwayAfter: Duration = Duration.ofMinutes(EXTENDED_AWAY_AFTER_MINUTES),
) {
    internal fun presenceOf(attention: Attention, holdsStream: Boolean, now: Instant): DesiredPresence {
        if (attention.present) return DesiredPresence.Available
        if (!holdsStream) return DesiredPresence.None
        val since = attention.absentSince ?: return DesiredPresence.Away(idleSince = null)
        val idle = Duration.between(since, now)
        return when {
            idle >= extendedAwayAfter -> DesiredPresence.ExtendedAway(since)
            idle >= awayAfter -> DesiredPresence.Away(since)
            else -> DesiredPresence.Available
        }
    }

    /** When [presenceOf] next changes on its own, or `null` if it will not. */
    internal fun nextStepAt(attention: Attention, holdsStream: Boolean, now: Instant): Instant? {
        if (attention.present || !holdsStream) return null
        val since = attention.absentSince ?: return null
        return listOf(since + awayAfter, since + extendedAwayAfter).firstOrNull { it.isAfter(now) }
    }

    private companion object {
        /** ADR-010 defaults, shared with the web client. */
        const val AWAY_AFTER_MINUTES = 10L
        const val EXTENDED_AWAY_AFTER_MINUTES = 30L
    }
}

/** Whole milliseconds from [from] until [to], rounded up so a step never fires early. */
private fun millisUntil(from: Instant, to: Instant): Long {
    val nanos = Duration.between(from, to).toNanos()
    return (nanos + NANOS_PER_MILLI - 1) / NANOS_PER_MILLI
}

private const val NANOS_PER_MILLI = 1_000_000L

/**
 * Whether a call in this state needs signalling over the stream. An
 * ended call waits in its slot until the UI dismisses it, which may be
 * never; it must not hold the stream.
 */
fun CallState.needsStream(): Boolean = this !is CallState.Idle && this !is CallState.Ended
