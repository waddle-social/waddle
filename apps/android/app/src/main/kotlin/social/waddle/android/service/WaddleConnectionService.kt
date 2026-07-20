package social.waddle.android.service

import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import social.waddle.android.MainActivity
import social.waddle.android.R
import social.waddle.android.WaddleApplication
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.SaslRetryDisposition

/**
 * `specialUse` foreground service keeping the XMPP socket alive without
 * Google Play push. It owns nothing — the DI-singleton session manager
 * does — it only raises process priority and renders the persistent
 * connection-status notification.
 */
class WaddleConnectionService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var stateJob: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val graph = (application as WaddleApplication).graph
        promoteToForeground(graph.sessionRuntime.connectionState.value)
        if (stateJob == null) {
            stateJob = scope.launch {
                graph.sessionRuntime.connectionState.collect(::promoteToForeground)
            }
        }
        return START_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    /**
     * (Re-)issue `startForeground` on every state change: it updates
     * the notification and, unlike `notify()`, never needs the
     * POST_NOTIFICATIONS runtime permission.
     */
    private fun promoteToForeground(state: ConnectionState) {
        try {
            startForeground(
                NOTIFICATION_ID,
                buildNotification(state),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
            )
        } catch (_: IllegalStateException) {
            // START_STICKY restarts re-enter here with the process in a
            // background state; when the system denies the promotion
            // (ForegroundServiceStartNotAllowedException), degrade to a
            // stop instead of crashing — the app-state collector re-arms
            // the service the next time the app is eligible.
            stopSelf()
        } catch (_: SecurityException) {
            stopSelf()
        }
    }

    private fun buildNotification(state: ConnectionState): Notification {
        val contentIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, NotificationChannels.CONNECTION)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(statusText(state))
            .setOngoing(true)
            .setSilent(true)
            .setPriority(NotificationCompat.PRIORITY_MIN)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setContentIntent(contentIntent)
            .build()
    }

    private fun statusText(state: ConnectionState): String = when (state) {
        ConnectionState.Ready -> getString(R.string.connection_status_connected)
        ConnectionState.Connecting -> getString(R.string.connection_connecting)
        is ConnectionState.Reconnecting ->
            getString(R.string.connection_reconnecting, state.attempt)
        ConnectionState.Offline -> getString(R.string.connection_offline)
        is ConnectionState.AuthenticationStopped -> when (state.disposition) {
            SaslRetryDisposition.RETRY -> getString(R.string.connection_failed)
            SaslRetryDisposition.STOP_CREDENTIAL ->
                getString(R.string.connection_auth_failed)
            SaslRetryDisposition.STOP_CONFIGURATION ->
                getString(R.string.connection_auth_configuration_failed)
            SaslRetryDisposition.STOP_ABORTED ->
                getString(R.string.connection_auth_aborted)
            SaslRetryDisposition.STOP_UNKNOWN ->
                getString(R.string.connection_auth_unknown)
        }
        ConnectionState.Failed -> getString(R.string.connection_failed)
        ConnectionState.Idle -> getString(R.string.connection_status_idle)
    }

    companion object {
        private const val NOTIFICATION_ID = 1

        fun intent(context: Context): Intent =
            Intent(context, WaddleConnectionService::class.java)
    }
}
