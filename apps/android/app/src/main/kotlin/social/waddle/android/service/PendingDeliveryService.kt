package social.waddle.android.service

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.R
import social.waddle.android.WaddleApplication
import social.waddle.android.client.ConnectionState

/**
 * Finishes what a notification action started when its receiver's few
 * seconds were not enough: holds the stream until the queued replies are
 * acknowledged, or until it is ready and queued markers have drained.
 *
 * A short foreground service (Android allows one from a notification
 * action) keeps the process active meanwhile; a cached process could be
 * frozen with its socket open, which also keeps the server from pushing.
 * A reply still unacknowledged when the window closes is flagged on its
 * notification as waiting rather than left looking sent.
 */
class PendingDeliveryService : Service() {
    private val scope = CoroutineScope(SupervisorJob())
    private var work: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val notification = NotificationCompat.Builder(this, NotificationChannels.CONNECTION)
            .setSmallIcon(R.drawable.ic_stat_waddle)
            .setContentTitle(getString(R.string.pending_delivery_title))
            .setSilent(true)
            .build()
        startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SHORT_SERVICE)
        val queuedIds = intent?.getStringArrayExtra(EXTRA_QUEUED_IDS)?.toSet().orEmpty()
        val conversationJid = intent?.getStringExtra(EXTRA_CONVERSATION_JID)
        val isGroupchat = intent?.getBooleanExtra(EXTRA_IS_GROUPCHAT, false) ?: false
        work?.cancel()
        work = scope.launch {
            val graph = (application as WaddleApplication).graph
            val hold = graph.holdConnection()
            try {
                val delivered = withTimeoutOrNull(WINDOW_MILLIS) {
                    graph.sessionManager.connectionState.first { it == ConnectionState.Ready }
                    graph.sessionPrefs.outboundQueue.first { queue -> queue.none { it.clientStanzaId in queuedIds } }
                    // Queued markers drain right after the ready pipeline's sends.
                    delay(MARKER_DRAIN_MILLIS)
                } != null
                if (!delivered && queuedIds.isNotEmpty() && conversationJid != null) {
                    runCatching { graph.messageNotifier.notifyReplyWaiting(conversationJid, isGroupchat) }
                }
            } finally {
                hold.release()
                stopSelf(startId)
            }
        }
        return START_NOT_STICKY
    }

    /** The system's short-service limit was reached: let go now. */
    override fun onTimeout(startId: Int, fgsType: Int) {
        work?.cancel()
        stopSelf(startId)
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        private const val NOTIFICATION_ID = 3
        private const val EXTRA_QUEUED_IDS = "social.waddle.android.extra.QUEUED_IDS"
        private const val EXTRA_CONVERSATION_JID = "social.waddle.android.extra.PENDING_CONVERSATION_JID"
        private const val EXTRA_IS_GROUPCHAT = "social.waddle.android.extra.PENDING_IS_GROUPCHAT"

        /** Inside the system's three-minute short-service limit. */
        private const val WINDOW_MILLIS = 150_000L
        private const val MARKER_DRAIN_MILLIS = 1_000L

        /**
         * Start from a notification-action receiver that is still running
         * (the window in which Android allows it). Best-effort: if the start
         * is refused, the work stays queued for the next connection.
         */
        fun start(
            context: Context,
            queuedIds: Collection<String> = emptyList(),
            conversationJid: String? = null,
            isGroupchat: Boolean = false,
        ) {
            val intent = Intent(context, PendingDeliveryService::class.java)
                .putExtra(EXTRA_QUEUED_IDS, queuedIds.toTypedArray())
                .putExtra(EXTRA_CONVERSATION_JID, conversationJid)
                .putExtra(EXTRA_IS_GROUPCHAT, isGroupchat)
            runCatching { context.startForegroundService(intent) }
        }
    }
}
