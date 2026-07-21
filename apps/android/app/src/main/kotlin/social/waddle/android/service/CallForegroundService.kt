package social.waddle.android.service

import android.Manifest
import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import social.waddle.android.MainActivity
import social.waddle.android.R
import social.waddle.android.WaddleApplication
import social.waddle.android.client.calls.CallState
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf

/**
 * In-call foreground service: raised for outgoing/active calls so the
 * process keeps mic (and camera) access while backgrounded, rendering
 * the ongoing-call notification with a hang-up action. It owns
 * nothing — the call slot lives in the session manager — it only
 * raises priority and shows call status.
 */
class CallForegroundService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var stateJob: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val graph = (application as WaddleApplication).graph
        promoteToForeground(graph.sessionManager.callStore.state.value)
        if (stateJob == null) {
            stateJob = scope.launch {
                graph.sessionManager.callStore.state.collectLatest { state ->
                    when {
                        state is CallState.Outgoing || state is CallState.Active ->
                            promoteToForeground(state)
                        // Accept in flight: keep the service up so the
                        // mic capture starting on Active is covered.
                        state is CallState.Incoming && state.accepting ->
                            promoteToForeground(state)
                        // The controller also stopService()s; stopping
                        // here too covers a service that outlives it.
                        // Grace before stopping: a migration takeover
                        // can pass through Ended on its way to the
                        // accepting ring, and Android 12+ forbids
                        // re-raising a dropped FGS from the background
                        // — collectLatest cancels the stop when a newer
                        // state lands inside the grace.
                        else -> {
                            delay(STOP_GRACE_MILLIS)
                            stopSelf()
                        }
                    }
                }
            }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    private fun promoteToForeground(state: CallState) {
        try {
            startForeground(NOTIFICATION_ID, buildNotification(state), foregroundTypes(state))
        } catch (_: IllegalStateException) {
            stopSelf()
        } catch (_: SecurityException) {
            stopSelf()
        }
    }

    /**
     * `phoneCall` is always legitimate (MANAGE_OWN_CALLS is an
     * install-time permission); `microphone`/`camera` may only be
     * declared once their runtime permission is granted or the
     * promotion throws SecurityException.
     */
    private fun foregroundTypes(state: CallState): Int {
        var types = ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL
        if (hasPermission(Manifest.permission.RECORD_AUDIO)) {
            types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        }
        val video = when (state) {
            is CallState.Outgoing -> state.media.video
            is CallState.Active -> state.media.video
            is CallState.Incoming -> state.media.video
            else -> false
        }
        if (video && hasPermission(Manifest.permission.CAMERA)) {
            types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
        }
        return types
    }

    private fun hasPermission(permission: String): Boolean =
        ContextCompat.checkSelfPermission(this, permission) == PackageManager.PERMISSION_GRANTED

    private fun buildNotification(state: CallState): Notification {
        val peerJid = when (state) {
            is CallState.Outgoing -> state.to
            is CallState.Active -> state.peer
            is CallState.Incoming -> state.from
            // A start command can race call death (fast propose
            // failure, remote conclusion): the mandatory promote then
            // shows a neutral wrap-up body for the stop grace instead
            // of a bogus ongoing call with a dead hang-up action.
            CallState.Idle, is CallState.Ended -> return buildEndedNotification()
        }
        val peerName = localpartOf(bareJidOf(peerJid)).ifEmpty { getString(R.string.call_unknown_peer) }
        val person = Person.Builder().setName(peerName).setKey(peerJid).build()
        val contentIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val hangUpIntent = PendingIntent.getBroadcast(
            this,
            0,
            Intent(this, CallActionReceiver::class.java)
                .setAction(CallActionReceiver.ACTION_HANG_UP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, NotificationChannels.ONGOING_CALLS)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle(peerName)
            .setContentText(
                getString(
                    if (state is CallState.Outgoing) {
                        R.string.call_status_calling
                    } else {
                        R.string.call_status_in_progress
                    },
                ),
            )
            .setStyle(NotificationCompat.CallStyle.forOngoingCall(person, hangUpIntent))
            .setOngoing(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setContentIntent(contentIntent)
            .build()
    }

    /** Neutral body for the stop grace when no call is live. */
    private fun buildEndedNotification(): Notification {
        val contentIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, NotificationChannels.ONGOING_CALLS)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle(getString(R.string.call_ended))
            .setOngoing(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setContentIntent(contentIntent)
            .build()
    }

    companion object {
        /** Distinct from the connection (1) and message (2) ids. */
        private const val NOTIFICATION_ID = 3

        /**
         * Grace before the phone-call FGS stops on a non-call state: a
         * migration takeover can pass through Ended on its way to the
         * accepting ring, and Android 12+ forbids re-raising a dropped
         * FGS from the background. Long enough for the in-flight
         * migration sends, short enough that the ongoing-call
         * notification doesn't linger noticeably after a real hang-up.
         */
        const val STOP_GRACE_MILLIS = 5_000L

        fun intent(context: Context): Intent =
            Intent(context, CallForegroundService::class.java)
    }
}
