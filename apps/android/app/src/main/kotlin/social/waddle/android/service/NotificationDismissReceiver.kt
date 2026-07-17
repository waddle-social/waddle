package social.waddle.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import social.waddle.android.WaddleApplication

/** Swiped-away message notification: drop its MessagingStyle history. */
class NotificationDismissReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != ACTION_DISMISS) return
        val ownerBareJid = intent.getStringExtra(EXTRA_OWNER_BARE_JID) ?: return
        val conversationJid = intent.getStringExtra(EXTRA_CONVERSATION_JID) ?: return
        (context.applicationContext as WaddleApplication)
            .graph
            .messageNotifier
            .clearConversation(ownerBareJid, conversationJid)
    }

    companion object {
        const val ACTION_DISMISS = "social.waddle.android.action.DISMISS_MESSAGES"
        const val EXTRA_OWNER_BARE_JID = "social.waddle.android.extra.OWNER_BARE_JID"
        const val EXTRA_CONVERSATION_JID = "social.waddle.android.extra.CONVERSATION_JID"
    }
}
