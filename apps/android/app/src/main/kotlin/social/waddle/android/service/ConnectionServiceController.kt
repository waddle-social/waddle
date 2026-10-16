package social.waddle.android.service

import android.app.ForegroundServiceStartNotAllowedException
import android.content.Context
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import social.waddle.android.client.WaddleAppState

/**
 * Runs [WaddleConnectionService] exactly while a signed-in account stays
 * connected in the background ([stayConnected]); with push delivery the
 * service is not needed and stays down.
 */
class ConnectionServiceController(
    private val context: Context,
    private val appState: StateFlow<WaddleAppState>,
    /** Effective stay-connected mode; `null` until it is known. */
    private val stayConnected: StateFlow<Boolean?>,
    private val scope: CoroutineScope,
    /** Foreground entry: the session should prove its stream is still alive. */
    private val onForeground: () -> Unit = {},
) {
    private val wanted: Boolean
        get() = appState.value == WaddleAppState.Ready && stayConnected.value == true

    fun start() {
        scope.launch {
            combine(appState, stayConnected) { state, stay ->
                when {
                    state == WaddleAppState.Ready && stay == true -> true
                    state == WaddleAppState.SignedOut || stay == false -> false
                    else -> null
                }
            }.distinctUntilChanged().collect { run ->
                when (run) {
                    true -> startService()
                    false -> stopService()
                    null -> Unit
                }
            }
        }
        // Re-arm on every app foreground: appState is distinct-until-
        // changed, so a service that stopped itself after a denied
        // background promotion would otherwise stay down for the rest of
        // the (still Ready) session. Foreground entry is always an
        // FGS-start-eligible moment.
        ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStart(owner: LifecycleOwner) {
                if (wanted) startService()
                if (appState.value == WaddleAppState.Ready) onForeground()
            }
        })
    }

    fun startService() {
        try {
            context.startForegroundService(WaddleConnectionService.intent(context))
        } catch (_: ForegroundServiceStartNotAllowedException) {
            // The background-start window closed (OEM/battery policy).
            // The service comes up with the next foreground entry or the
            // boot receiver's exemption window instead.
        }
    }

    fun stopService() {
        context.stopService(WaddleConnectionService.intent(context))
    }
}
