package social.waddle.android.client

import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map

/**
 * [NetworkSignal] over `ConnectivityManager.registerDefaultNetworkCallback`.
 *
 * The callback reports the default network's identity, not just a
 * Boolean: a Wi-Fi → cellular handoff is online → online, but every
 * socket bound to the old network is now dead. [networkChanges] exposes
 * exactly those identity changes so the connection loop can verify (or
 * replace) its stream instead of trusting a half-open WebSocket.
 */
class ConnectivityNetworkSignal(
    private val connectivityManager: ConnectivityManager,
) : NetworkSignal {
    /** The current default network with internet, or `null` when offline. */
    private val defaultNetwork: Flow<Network?> = callbackFlow {
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                trySend(network)
            }

            override fun onLost(network: Network) {
                trySend(null)
            }

            override fun onUnavailable() {
                trySend(null)
            }
        }
        trySend(currentNetwork())
        connectivityManager.registerDefaultNetworkCallback(callback)
        awaitClose { connectivityManager.unregisterNetworkCallback(callback) }
    }.distinctUntilChanged()

    override val online: Flow<Boolean> = defaultNetwork.map { it != null }.distinctUntilChanged()

    override val networkChanges: Flow<Unit> = handoffsOf(defaultNetwork)

    private fun currentNetwork(): Network? {
        val network = connectivityManager.activeNetwork ?: return null
        val capabilities = connectivityManager.getNetworkCapabilities(network) ?: return null
        return network.takeIf { capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) }
    }
}
