package social.waddle.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.WaddleApplication
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.XmppSessionManager

/**
 * Notification "Mark as read": dispatches the displayed marker + MDS
 * cursor for the conversation's newest message and retires the shade
 * notification — no activity launch (Android 12+ trampoline rules).
 */
class MarkReadReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != ACTION_MARK_READ) return
        val conversationJid = intent.getStringExtra(EXTRA_CONVERSATION_JID) ?: return
        val isGroupchat = intent.getBooleanExtra(EXTRA_IS_GROUPCHAT, false)

        // The intent names the target message directly: after process
        // death the in-memory timeline is empty and a lookup-based
        // dispatch would silently no-op.
        val explicitTarget = intent.getStringExtra(EXTRA_MARKER_ID)?.let { markerId ->
            XmppSessionManager.DisplayedTarget(
                markerId = markerId,
                stanzaId = intent.getStringExtra(EXTRA_STANZA_ID),
                stanzaIdBy = intent.getStringExtra(EXTRA_STANZA_ID_BY),
                markerRequested = intent.getBooleanExtra(EXTRA_MARKER_REQUESTED, false),
            )
        }

        val graph = (context.applicationContext as WaddleApplication).graph
        // The marker goes out over the stream: hold it open while push
        // mode has it closed (a queued marker drains once ready), keeping
        // the receiver unfinished meanwhile so the process stays active.
        val hold = graph.holdConnection()
        val pendingResult = goAsync()
        graph.applicationScope.launch {
            try {
                // Same never-throw posture as the other receivers: an
                // uncaught throw on this root coroutine kills the process.
                runCatching {
                    graph.sessionManager.markConversationDisplayed(
                        conversationJid,
                        isGroupchat,
                        explicitTarget,
                    )
                }
                runCatching { graph.sessionManager.unreadStore.clear(conversationJid) }
                runCatching { graph.messageNotifier.clearConversationNotification(conversationJid) }
                withTimeoutOrNull(MARKER_HOLD_MILLIS) {
                    graph.sessionManager.connectionState.first { it == ConnectionState.Ready }
                    // The ready pipeline drains queued markers right after.
                    delay(MARKER_DRAIN_MILLIS)
                }
            } finally {
                hold.release()
                pendingResult.finish()
            }
        }
    }

    companion object {
        const val ACTION_MARK_READ = "social.waddle.android.action.MARK_READ"

        /** Within the receiver's ~10 s goAsync budget. */
        private const val MARKER_HOLD_MILLIS = 9_000L
        private const val MARKER_DRAIN_MILLIS = 1_000L
        const val EXTRA_CONVERSATION_JID = "social.waddle.android.extra.CONVERSATION_JID"
        const val EXTRA_IS_GROUPCHAT = "social.waddle.android.extra.IS_GROUPCHAT"
        const val EXTRA_MARKER_ID = "social.waddle.android.extra.MARKER_ID"
        const val EXTRA_STANZA_ID = "social.waddle.android.extra.STANZA_ID"
        const val EXTRA_STANZA_ID_BY = "social.waddle.android.extra.STANZA_ID_BY"
        const val EXTRA_MARKER_REQUESTED = "social.waddle.android.extra.MARKER_REQUESTED"
    }
}
