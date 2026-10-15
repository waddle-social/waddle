package social.waddle.android.client

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.flow

/**
 * Injectable connectivity source so the connection loop stays a pure,
 * testable coroutine. Production wiring is [ConnectivityNetworkSignal].
 */
interface NetworkSignal {
    /** `true` while a default network with internet capability exists. */
    val online: Flow<Boolean>

    /**
     * Emits when the default network is replaced by a different one
     * without an offline gap (Wi-Fi ↔ cellular, VPN up/down). Sockets on
     * the previous network may be dead while still looking open.
     */
    val networkChanges: Flow<Unit> get() = emptyFlow()
}

/**
 * One emission per change from one non-null identity to a different
 * non-null identity. Online/offline transitions are not handoffs: the
 * `online` flow already covers them.
 */
fun <T : Any> handoffsOf(identities: Flow<T?>): Flow<Unit> = flow {
    var last: T? = null
    identities.collect { current ->
        val previous = last
        last = current
        if (previous != null && current != null && previous != current) emit(Unit)
    }
}
