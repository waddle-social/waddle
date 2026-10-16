package social.waddle.android.service

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.R
import social.waddle.android.WaddleApplication
import java.util.concurrent.ConcurrentHashMap

/**
 * Finishes what a notification action started when its receiver's few
 * seconds were not enough: holds the stream until the queued replies are
 * acknowledged and the parked read markers have gone out.
 *
 * A short foreground service (Android allows one from a notification
 * action) keeps the process active meanwhile; a cached process could be
 * frozen with its socket open, which also keeps the server from pushing.
 * A reply still unacknowledged when the window closes is flagged on its
 * notification as waiting rather than left looking sent.
 */
class PendingDeliveryService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var work: Job? = null

    /** Queued reply id → its conversation; merged across overlapping starts. */
    private val pendingReplies = ConcurrentHashMap<String, PendingReply>()

    private data class PendingReply(val conversationJid: String, val isGroupchat: Boolean)

    /** Conversations whose read marker is parked for the next ready session. */
    private val pendingMarkers: MutableSet<String> = ConcurrentHashMap.newKeySet()

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val notification = NotificationCompat.Builder(this, NotificationChannels.CONNECTION)
            .setSmallIcon(R.drawable.ic_stat_waddle)
            .setContentTitle(getString(R.string.pending_delivery_title))
            .setSilent(true)
            .build()
        startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SHORT_SERVICE)
        val conversationJid = intent?.getStringExtra(EXTRA_CONVERSATION_JID)
        val isGroupchat = intent?.getBooleanExtra(EXTRA_IS_GROUPCHAT, false) ?: false
        if (conversationJid != null) {
            intent.getStringArrayExtra(EXTRA_QUEUED_IDS).orEmpty().forEach { id ->
                pendingReplies[id] = PendingReply(conversationJid, isGroupchat)
            }
        }
        intent?.getStringExtra(EXTRA_MARKER_CONVERSATION_JID)?.let(pendingMarkers::add)
        // One waiter over everything still pending; a new start restarts
        // its window rather than dropping what earlier starts were owed.
        work?.cancel()
        work = scope.launch { awaitDeliveries(startId) }
        return START_NOT_STICKY
    }

    private suspend fun awaitDeliveries(startId: Int) {
        val graph = (application as WaddleApplication).graph
        val hold = graph.holdConnection()
        try {
            // A failed store read counts as not delivered: flag and stop
            // rather than crash with the replies still looking sent. A
            // cancellation (a newer start took over) must propagate.
            val delivered = try {
                awaitDelivered()
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Exception) {
                false
            }
            if (!delivered) flagWaitingReplies()
            stopSelf(startId)
        } finally {
            hold.release()
        }
    }

    /** Every pending reply acknowledged and markers drained, within the window? */
    private suspend fun awaitDelivered(): Boolean {
        val graph = (application as WaddleApplication).graph
        return withTimeoutOrNull(WINDOW_MILLIS) {
            graph.sessionPrefs.outboundQueue.first { queue ->
                val queued = queue.mapTo(HashSet()) { it.clientStanzaId }
                pendingReplies.keys.removeAll { it !in queued }
                pendingReplies.isEmpty()
            }
            pendingMarkers.toList().forEach { conversationJid ->
                graph.sessionManager.awaitMarkerNotParked(conversationJid)
                pendingMarkers.remove(conversationJid)
            }
        } != null
    }

    /** Replies still unacknowledged must not keep looking sent. */
    private suspend fun flagWaitingReplies() {
        val notifier = (application as WaddleApplication).graph.messageNotifier
        pendingReplies.values.distinct().forEach { reply ->
            runCatching { notifier.notifyReplyWaiting(reply.conversationJid, reply.isGroupchat) }
        }
        pendingReplies.clear()
    }

    /** API 34: the system's short-service limit was reached. */
    @Deprecated("Superseded by onTimeout(Int, Int) on API 35+", ReplaceWith("onTimeout(startId, fgsType)"))
    override fun onTimeout(startId: Int) = timedOut(startId)

    /** API 35+: the system's short-service limit was reached. */
    override fun onTimeout(startId: Int, fgsType: Int) = timedOut(startId)

    private fun timedOut(startId: Int) {
        work?.cancel()
        // Stop at once (the system allows only seconds), then flag.
        stopSelf(startId)
        scope.launch { flagWaitingReplies() }
    }

    override fun onDestroy() {
        work?.cancel()
        super.onDestroy()
    }

    companion object {
        /** Unique among the app's ids: 1 connection, 2 messages, 3 call, 4 incoming call. */
        private const val NOTIFICATION_ID = 5
        private const val EXTRA_QUEUED_IDS = "social.waddle.android.extra.QUEUED_IDS"
        private const val EXTRA_CONVERSATION_JID = "social.waddle.android.extra.PENDING_CONVERSATION_JID"
        private const val EXTRA_IS_GROUPCHAT = "social.waddle.android.extra.PENDING_IS_GROUPCHAT"

        /** Inside the system's three-minute short-service limit. */
        private const val WINDOW_MILLIS = 150_000L
        private const val EXTRA_MARKER_CONVERSATION_JID = "social.waddle.android.extra.MARKER_CONVERSATION_JID"

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
            /** A conversation whose read marker is parked for the next ready session. */
            markerConversationJid: String? = null,
        ) {
            val intent = Intent(context, PendingDeliveryService::class.java)
                .putExtra(EXTRA_QUEUED_IDS, queuedIds.toTypedArray())
                .putExtra(EXTRA_MARKER_CONVERSATION_JID, markerConversationJid)
                .putExtra(EXTRA_CONVERSATION_JID, conversationJid)
                .putExtra(EXTRA_IS_GROUPCHAT, isGroupchat)
            val started = runCatching { context.startForegroundService(intent) }.isSuccess
            // Refused: the reply stays queued for the next connection, so it
            // must not keep looking sent.
            if (!started && conversationJid != null && queuedIds.isNotEmpty()) {
                val graph = (context.applicationContext as WaddleApplication).graph
                graph.applicationScope.launch {
                    runCatching { graph.messageNotifier.notifyReplyWaiting(conversationJid, isGroupchat) }
                }
            }
        }
    }
}
