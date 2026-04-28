package social.waddle.android.connection

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
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
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import org.koin.android.ext.android.inject
import social.waddle.android.MainActivity
import social.waddle.android.R

/**
 * Hosts the long-lived XMPP connection while the app is running. Started
 * by the auth flow once [WaddleConnectionManager.start] has succeeded;
 * stopped on sign-out or after the configured background grace period.
 *
 * `dataSync` is the correct foreground type for this workload — chat is
 * not media playback. Android 16 enforces soft daily caps on dataSync; a
 * future revision will plumb XEP-0357 + FCM for true push delivery so the
 * app no longer needs to stay foregrounded to receive messages.
 */
internal class ConnectionService : Service() {
    private val connection: WaddleConnectionManager by inject()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var observerJob: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        ensureNotificationChannel()
        startForeground(
            NOTIFICATION_ID,
            buildNotification(getString(R.string.connection_service_notification_connecting)),
            ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
        )
        observerJob = scope.launch {
            connection.state.collectLatest { state ->
                val text = when (state) {
                    ConnectionState.Connected ->
                        getString(R.string.connection_service_notification_connected)
                    ConnectionState.Connecting ->
                        getString(R.string.connection_service_notification_connecting)
                    ConnectionState.Disconnected ->
                        getString(R.string.connection_service_notification_disconnected)
                    is ConnectionState.Failed -> state.description
                }
                notificationManager().notify(NOTIFICATION_ID, buildNotification(text))
            }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onDestroy() {
        observerJob?.cancel()
        scope.cancel()
        super.onDestroy()
    }

    private fun notificationManager(): NotificationManager =
        getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    private fun ensureNotificationChannel() {
        val channel = NotificationChannel(
            CHANNEL_ID,
            getString(R.string.connection_service_channel_name),
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = getString(R.string.connection_service_channel_description)
            setShowBadge(false)
        }
        notificationManager().createNotificationChannel(channel)
    }

    private fun buildNotification(text: String): Notification {
        val launchIntent = Intent(this, MainActivity::class.java)
        val pending = android.app.PendingIntent.getActivity(
            this,
            0,
            launchIntent,
            android.app.PendingIntent.FLAG_IMMUTABLE or android.app.PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setSmallIcon(R.drawable.waddle_logo)
            .setContentIntent(pending)
            .setOngoing(true)
            .setSilent(true)
            .build()
    }

    companion object {
        private const val CHANNEL_ID = "waddle.connection"
        private const val NOTIFICATION_ID = 1001

        fun start(context: Context) {
            context.startForegroundService(Intent(context, ConnectionService::class.java))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, ConnectionService::class.java))
        }
    }
}
