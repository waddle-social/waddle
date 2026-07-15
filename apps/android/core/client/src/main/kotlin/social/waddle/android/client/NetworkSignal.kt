package social.waddle.android.client

import kotlinx.coroutines.flow.Flow

/**
 * Injectable connectivity source so the connection loop stays a pure,
 * testable coroutine: `true` while a default network with internet
 * capability exists. Production wiring is [ConnectivityNetworkSignal].
 */
interface NetworkSignal {
    val online: Flow<Boolean>
}
