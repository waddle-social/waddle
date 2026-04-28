package social.waddle.android.connection

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import social.waddle.android.domain.WaddleSession
import social.waddle.android.ffi.WaddleClientHandle
import social.waddle.android.ffi.WaddleEvent
import uniffi.waddle_xmpp_client.WaddleConfig

internal sealed interface ConnectionState {
    data object Disconnected : ConnectionState
    data object Connecting : ConnectionState
    data object Connected : ConnectionState
    data class Failed(val description: String) : ConnectionState
}

/**
 * Application-scoped owner of the active [WaddleClientHandle] and its
 * per-session repository bundle ([WaddleSession]). Survives configuration
 * changes for as long as the hosting [android.app.Application] is alive.
 *
 * The handle and its event listener share an Application-lifetime
 * reference cycle by design: the listener's strong ref to the manager
 * is intentional and contained.
 */
internal class WaddleConnectionManager {
    private val supervisor = SupervisorJob()
    private val rootScope = CoroutineScope(supervisor + Dispatchers.Default)

    private val mutableState = MutableStateFlow<ConnectionState>(ConnectionState.Disconnected)
    val state: StateFlow<ConnectionState> = mutableState.asStateFlow()

    private val mutableSession = MutableStateFlow<WaddleSession?>(null)
    val activeSession: StateFlow<WaddleSession?> = mutableSession.asStateFlow()

    private var handle: WaddleClientHandle? = null
    private var sessionScope: CoroutineScope? = null
    private var pumpJob: Job? = null

    /** Hot stream of low-level events. Empty until [start] has been called. */
    val events: SharedFlow<WaddleEvent>
        get() = handle?.events ?: error("WaddleConnectionManager.start() must be called first")

    fun start(config: WaddleConfig) {
        if (handle != null) return
        val newHandle = WaddleClientHandle(config)
        val scope = CoroutineScope(SupervisorJob(supervisor) + Dispatchers.Default)
        handle = newHandle
        sessionScope = scope
        mutableSession.value = WaddleSession(newHandle, scope)
        pumpJob = rootScope.launch {
            newHandle.events.collect { event ->
                mutableState.value = when (event) {
                    is WaddleEvent.Connected -> ConnectionState.Connected
                    is WaddleEvent.Disconnected -> ConnectionState.Disconnected
                    is WaddleEvent.Error -> ConnectionState.Failed(event.description)
                    else -> mutableState.value
                }
            }
        }
        mutableState.value = ConnectionState.Connecting
        rootScope.launch { newHandle.connect() }
    }

    fun stop() {
        val outgoing = handle ?: return
        // Clear local state synchronously so a fast stop→start cycle (e.g.
        // sign-out followed immediately by sign-in) doesn't see the old
        // handle and bail out of `start()`.
        pumpJob?.cancel()
        pumpJob = null
        sessionScope?.cancel()
        sessionScope = null
        mutableSession.value = null
        handle = null
        mutableState.value = ConnectionState.Disconnected
        rootScope.launch { runCatching { outgoing.disconnect() } }
    }
}
