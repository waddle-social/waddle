package social.waddle.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlinx.coroutines.launch
import social.waddle.android.WaddleApplication

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

        val graph = (context.applicationContext as WaddleApplication).graph
        val pendingResult = goAsync()
        graph.applicationScope.launch {
            try {
                // Same never-throw posture as the other receivers: an
                // uncaught throw on this root coroutine kills the process.
                runCatching {
                    graph.sessionManager.markConversationDisplayed(conversationJid, isGroupchat)
                }
                runCatching { graph.sessionManager.unreadStore.clear(conversationJid) }
                runCatching { graph.messageNotifier.clearConversationNotification(conversationJid) }
            } finally {
                pendingResult.finish()
            }
        }
    }

    companion object {
        const val ACTION_MARK_READ = "social.waddle.android.action.MARK_READ"
        const val EXTRA_CONVERSATION_JID = "social.waddle.android.extra.CONVERSATION_JID"
        const val EXTRA_IS_GROUPCHAT = "social.waddle.android.extra.IS_GROUPCHAT"
    }
}
