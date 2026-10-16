package social.waddle.android.push

import android.os.SystemClock
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
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
    /**
     * Wakes the process for the next auto-away step. The steps must fire
     * while the phone sleeps, so production uses an `AlarmManager` alarm
     * ([AlarmWakeTimer]); coroutine time stops in deep sleep.
     */
    private val wakeTimer: WakeTimer,
    /** Wall clock for the XEP-0319 idle stamp. */
    private val wallClock: () -> Instant = Instant::now,
    /** Monotonic time that keeps counting in deep sleep, for the thresholds. */
    private val elapsedRealtime: () -> Long = SystemClock::elapsedRealtime,
    private val autoAway: AutoAway = AutoAway(),
) {
    private val attention = MutableStateFlow(Attention(foreground = false, inCall = false, absentSince = null))
    private val wakeups = MutableStateFlow(0L)
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

    /** One evaluation of [presence], tagged with the wake-ups it has seen. */
    private data class Evaluation(val wakeup: Long, val presence: DesiredPresence)

    /** Auto-away, evaluated afresh on every input change and wake-up. */
    private val evaluation: StateFlow<Evaluation> = combine(
        attention,
        stayConnected,
        callActive,
        wakeups,
    ) { seen, stay, call, wakeup ->
        val holdsStream = stay || call
        val now = elapsedRealtime()
        when (val next = autoAway.nextStepAt(seen, holdsStream, now)) {
            null -> wakeTimer.cancel()
            else -> wakeTimer.schedule(next, ::reevaluate)
        }
        Evaluation(wakeup, autoAway.presenceOf(seen, holdsStream, now))
    }
        .stateIn(scope, SharingStarted.Eagerly, initialValue = Evaluation(wakeup = 0L, presence = DesiredPresence.None))

    /**
     * What this device advertises on the stream while [wanted] holds it,
     * stepping through [autoAway] while the user is away. Every evaluation
     * measures against elapsed realtime, so a step that came due while the
     * phone slept applies on the next one, and re-arms [wakeTimer] for the
     * step after it (or cancels it, e.g. on returning to the foreground).
     */
    val presence: StateFlow<DesiredPresence> = evaluation
        .map { it.presence }
        .stateIn(scope, SharingStarted.Eagerly, initialValue = DesiredPresence.None)

    /** Re-check auto-away now: the wake timer fired, or the process woke. */
    fun reevaluate() {
        wakeups.update { it + 1 }
    }

    /** The presence once every [reevaluate] so far has been evaluated. */
    suspend fun presenceAfterWake(): DesiredPresence {
        val target = wakeups.value
        return evaluation.first { it.wakeup >= target }.presence
    }

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
                old.present -> next.copy(absentSince = Departure(wallClock(), elapsedRealtime()))
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
 * When the user stopped being present: [at] stamps XEP-0319 idle, and
 * [elapsedRealtimeMillis] measures the auto-away thresholds across sleep.
 */
internal data class Departure(val at: Instant, val elapsedRealtimeMillis: Long)

/**
 * Whether the user is present on this device, and since when they are
 * not. [absentSince] is `null` while present, and also after a cold start
 * in the background, where the last interaction is unknown.
 */
internal data class Attention(
    val foreground: Boolean,
    val inCall: Boolean,
    val absentSince: Departure?,
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
    /** The presence at elapsed realtime [nowMillis]. */
    internal fun presenceOf(attention: Attention, holdsStream: Boolean, nowMillis: Long): DesiredPresence {
        if (attention.present) return DesiredPresence.Available
        if (!holdsStream) return DesiredPresence.None
        val since = attention.absentSince ?: return DesiredPresence.Away(idleSince = null)
        val idleMillis = nowMillis - since.elapsedRealtimeMillis
        return when {
            idleMillis >= extendedAwayAfter.toMillis() -> DesiredPresence.ExtendedAway(since.at)
            idleMillis >= awayAfter.toMillis() -> DesiredPresence.Away(since.at)
            else -> DesiredPresence.Available
        }
    }

    /** Elapsed realtime of the next step after [nowMillis], or `null` if none is due. */
    internal fun nextStepAt(attention: Attention, holdsStream: Boolean, nowMillis: Long): Long? {
        if (attention.present || !holdsStream) return null
        val since = attention.absentSince?.elapsedRealtimeMillis ?: return null
        return listOf(since + awayAfter.toMillis(), since + extendedAwayAfter.toMillis()).firstOrNull { it > nowMillis }
    }

    private companion object {
        /** ADR-010 defaults, shared with the web client. */
        const val AWAY_AFTER_MINUTES = 10L
        const val EXTENDED_AWAY_AFTER_MINUTES = 30L
    }
}

/** Wakes the process at an elapsed-realtime instant, even from deep sleep. */
interface WakeTimer {
    /** Call [onFire] at [atElapsedRealtimeMillis], replacing any pending wake. */
    fun schedule(atElapsedRealtimeMillis: Long, onFire: () -> Unit)

    fun cancel()
}

/**
 * Whether a call in this state needs signalling over the stream. An
 * ended call waits in its slot until the UI dismisses it, which may be
 * never; it must not hold the stream.
 */
fun CallState.needsStream(): Boolean = this !is CallState.Idle && this !is CallState.Ended
