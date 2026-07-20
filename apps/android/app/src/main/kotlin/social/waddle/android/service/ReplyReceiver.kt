package social.waddle.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput
import kotlinx.coroutines.launch
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
        val ownerBareJid = intent.getStringExtra(EXTRA_OWNER_BARE_JID)
        val conversationJid = intent.getStringExtra(EXTRA_CONVERSATION_JID)
        if (text.isEmpty() || ownerBareJid == null || conversationJid == null) return
        val isGroupchat = intent.getBooleanExtra(EXTRA_IS_GROUPCHAT, false)

        val graph = (context.applicationContext as WaddleApplication).graph
        val pendingResult = goAsync()
        graph.applicationScope.launch {
            try {
                // Same defensive posture as the bootstrap/login/notifier
                // paths: the offline enqueue is a DataStore write and the
                // shade re-post is a notify() — both can throw, and an
                // uncaught throw on this root coroutine kills the process
                // while leaving the RemoteInput spinner stuck.
                runCatching {
                    deliver(graph, ownerBareJid, conversationJid, isGroupchat, text)
                }
                    .onFailure {
                        runCatching {
                            graph.messageNotifier.notifyReplyFailed(
                                ownerBareJid,
                                conversationJid,
                                isGroupchat,
                            )
                        }
                    }
            } finally {
                pendingResult.finish()
            }
        }
    }

    private suspend fun deliver(
        graph: AppGraph,
        ownerBareJid: String,
        conversationJid: String,
        isGroupchat: Boolean,
        text: String,
    ) {
        val result = graph.sessionRuntime.sendDirectReply(
            expectedOwnerBareJid = ownerBareJid,
            conversationJid = conversationJid,
            isGroupchat = isGroupchat,
            body = text,
        )
        // A written send OR a queued one may echo into the shade:
        // queued replies are persisted and replayed by the session
        // manager on reconnect, so prompting the user to retype
        // (notifyReplyFailed) would produce a duplicate. Only a
        // permanent rejection (invalid recipient/options, stanza
        // error) surfaces as a visible failure — silently
        // swallowing the reply while showing it as sent loses
        // the message.
        if (result.outcome is WaddleSendMessageOutcome.Sent || result.queued) {
            graph.messageNotifier.appendOwnReply(
                ownerBareJid,
                conversationJid,
                isGroupchat,
                text,
            )
        } else {
            graph.messageNotifier.notifyReplyFailed(
                ownerBareJid,
                conversationJid,
                isGroupchat,
            )
        }
    }

    companion object {
        const val ACTION_REPLY = "social.waddle.android.action.REPLY"
        const val EXTRA_OWNER_BARE_JID = "social.waddle.android.extra.OWNER_BARE_JID"
        const val EXTRA_CONVERSATION_JID = "social.waddle.android.extra.CONVERSATION_JID"
        const val EXTRA_IS_GROUPCHAT = "social.waddle.android.extra.IS_GROUPCHAT"
        const val KEY_REPLY_TEXT = "waddle.reply.text"
    }
}
