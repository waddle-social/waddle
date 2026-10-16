package social.waddle.android.push

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.transformLatest
import kotlinx.coroutines.flow.update
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
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionDemand(
    scope: CoroutineScope,
    private val graceMillis: Long = GRACE_MILLIS,
) {
    private val processStarted = MutableStateFlow(false)
    private val stayConnected = MutableStateFlow(false)
    private val callActive = MutableStateFlow(false)
    private val leases = MutableStateFlow(0)

    val wanted: StateFlow<Boolean> = combine(
        processStarted,
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

    fun setProcessStarted(started: Boolean) {
        processStarted.value = started
    }

    fun setStayConnected(stay: Boolean) {
        stayConnected.value = stay
    }

    fun setCallActive(active: Boolean) {
        callActive.value = active
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
        /** How long the stream outlives the last thing that needed it. */
        const val GRACE_MILLIS = 30_000L
    }
}
