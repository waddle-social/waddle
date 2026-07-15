package social.waddle.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput
import kotlinx.coroutines.launch
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
        val pendingResult = goAsync()
        graph.applicationScope.launch {
            try {
                val outcome = if (isGroupchat) {
                    graph.sessionManager.sendGroupchatMessage(conversationJid, text)
                } else {
                    graph.sessionManager.sendChatMessage(conversationJid, text)
                }
                // Only a server-acked send may echo into the shade as
                // delivered; anything else (NotConnected during a
                // reconnect window or process restart, transport/stanza
                // errors) must surface as a visible failure — silently
                // swallowing the reply while showing it as sent loses
                // the message.
                if (outcome is WaddleSendMessageOutcome.Sent) {
                    graph.messageNotifier.appendOwnReply(conversationJid, isGroupchat, text)
                } else {
                    graph.messageNotifier.notifyReplyFailed(conversationJid, isGroupchat)
                }
            } finally {
                pendingResult.finish()
            }
        }
    }

    companion object {
        const val ACTION_REPLY = "social.waddle.android.action.REPLY"
        const val EXTRA_CONVERSATION_JID = "social.waddle.android.extra.CONVERSATION_JID"
        const val EXTRA_IS_GROUPCHAT = "social.waddle.android.extra.IS_GROUPCHAT"
        const val KEY_REPLY_TEXT = "waddle.reply.text"
    }
}
