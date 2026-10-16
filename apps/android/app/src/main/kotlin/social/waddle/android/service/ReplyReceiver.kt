package social.waddle.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.AppGraph
import social.waddle.android.WaddleApplication
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * Direct-reply action: sends the RemoteInput text through the session
 * manager (never launches an activity — Android 12+ trampoline rules)
 * and appends the reply to the conversation's MessagingStyle.
 */
class ReplyReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != ACTION_REPLY) return
        val results = RemoteInput.getResultsFromIntent(intent) ?: return
        val text = results.getCharSequence(KEY_REPLY_TEXT)?.toString()?.trim().orEmpty()
        val conversationJid = intent.getStringExtra(EXTRA_CONVERSATION_JID)
        if (text.isEmpty() || conversationJid == null) return
        val isGroupchat = intent.getBooleanExtra(EXTRA_IS_GROUPCHAT, false)

        val graph = (context.applicationContext as WaddleApplication).graph
        // Without push-mode demand the stream may be closed: hold it open
        // until the reply is acknowledged (or the hold times out).
        val hold = graph.holdConnection()
        val pendingResult = goAsync()
        graph.applicationScope.launch {
            var queuedId: String? = null
            try {
                // Same defensive posture as the bootstrap/login/notifier
                // paths: the offline enqueue is a DataStore write and the
                // shade re-post is a notify() — both can throw, and an
                // uncaught throw on this root coroutine kills the process
                // while leaving the RemoteInput spinner stuck.
                runCatching { queuedId = deliver(graph, conversationJid, isGroupchat, text) }
                    .onFailure {
                        runCatching {
                            graph.messageNotifier.notifyReplyFailed(conversationJid, isGroupchat)
                        }
                    }
            } finally {
                pendingResult.finish()
            }
            awaitAcknowledged(graph, queuedId)
            hold.release()
        }
    }

    /** Until the durable row for [queuedId] is acknowledged and gone, bounded. */
    private suspend fun awaitAcknowledged(graph: AppGraph, queuedId: String?) {
        queuedId ?: return
        withTimeoutOrNull(DELIVERY_HOLD_MILLIS) {
            graph.sessionPrefs.outboundQueue.first { queue -> queue.none { it.clientStanzaId == queuedId } }
        }
    }

    /** Sends the reply; returns the outbound queue id it is tracked under. */
    private suspend fun deliver(
        graph: AppGraph,
        conversationJid: String,
        isGroupchat: Boolean,
        text: String,
    ): String? {
        val result = if (isGroupchat) {
            graph.sessionManager.sendGroupchatMessage(conversationJid, text)
        } else {
            graph.sessionManager.sendChatMessage(conversationJid, text)
        }
        // A written send OR a queued one may echo into the shade:
        // queued replies are persisted and replayed by the session
        // manager on reconnect, so prompting the user to retype
        // (notifyReplyFailed) would produce a duplicate. Only a
        // permanent rejection (invalid recipient/options, stanza
        // error) surfaces as a visible failure — silently
        // swallowing the reply while showing it as sent loses
        // the message.
        if (result.outcome is WaddleSendMessageOutcome.Sent || result.queued) {
            graph.messageNotifier.appendOwnReply(conversationJid, isGroupchat, text, result.queuedId)
        } else {
            graph.messageNotifier.notifyReplyFailed(conversationJid, isGroupchat)
        }
        return result.queuedId
    }

    companion object {
        const val ACTION_REPLY = "social.waddle.android.action.REPLY"
        const val EXTRA_CONVERSATION_JID = "social.waddle.android.extra.CONVERSATION_JID"
        const val EXTRA_IS_GROUPCHAT = "social.waddle.android.extra.IS_GROUPCHAT"
        const val KEY_REPLY_TEXT = "waddle.reply.text"

        /** Longest the stream is held open for one reply. */
        private const val DELIVERY_HOLD_MILLIS = 30_000L
    }
}
