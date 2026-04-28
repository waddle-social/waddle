package social.waddle.android.connection

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.launch
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
 * Application-scoped owner of the active [WaddleClientHandle]. Survives
 * configuration changes and process backgrounding for as long as the
 * hosting [Application] is alive.
 *
 * The handle and its event listener share an Application-lifetime
 * reference cycle by design: the listener's strong-ref to the singleton
 * is intentional and contained.
 */
internal class WaddleConnectionManager {
    private val supervisor = SupervisorJob()
    private val scope = CoroutineScope(supervisor + Dispatchers.Default)

    private val mutableState = MutableStateFlow<ConnectionState>(ConnectionState.Disconnected)
    val state: StateFlow<ConnectionState> = mutableState.asStateFlow()

    private var handle: WaddleClientHandle? = null
    private var pumpJob: Job? = null

    /** Hot stream of low-level events. Empty until [start] has been called. */
    val events: SharedFlow<WaddleEvent>
        get() = handle?.events ?: error("WaddleConnectionManager.start() must be called first")

    fun hasHandle(): Boolean = handle != null

    fun handle(): WaddleClientHandle =
        handle ?: error("WaddleConnectionManager.start() must be called first")

    fun start(config: WaddleConfig) {
        if (handle != null) return
        val newHandle = WaddleClientHandle(config)
        handle = newHandle
        pumpJob = scope.launch {
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
        scope.launch { newHandle.connect() }
    }

    fun stop() {
        scope.launch {
            handle?.disconnect()
            pumpJob?.cancel()
            pumpJob = null
            handle = null
            mutableState.value = ConnectionState.Disconnected
        }
    }

    /** Coalesces possibly-empty event streams so callers don't crash before [start]. */
    fun safeEvents(): SharedFlow<WaddleEvent> = handle?.events ?: error("not started")

    fun eventStreamOrEmpty() =
        handle?.events ?: emptyFlow<WaddleEvent>().let { merge(it) }
}
