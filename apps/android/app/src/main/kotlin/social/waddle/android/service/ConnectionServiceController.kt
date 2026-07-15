package social.waddle.android.service

import android.app.ForegroundServiceStartNotAllowedException
import android.content.Context
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import social.waddle.android.client.WaddleAppState

/**
 * Starts/stops [WaddleConnectionService] on app-state transitions:
 * `Ready` raises the foreground service, `SignedOut` tears it down.
 */
class ConnectionServiceController(
    private val context: Context,
    private val appState: StateFlow<WaddleAppState>,
    private val scope: CoroutineScope,
) {
    fun start() {
        scope.launch {
            appState.collect { state ->
                when (state) {
                    WaddleAppState.Ready -> startService()
                    WaddleAppState.SignedOut -> stopService()
                    else -> Unit
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
                if (appState.value == WaddleAppState.Ready) {
                    startService()
                }
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
