package social.waddle.android.feature.conversation

import androidx.lifecycle.ViewModelProvider
import social.waddle.android.AppGraph
import social.waddle.android.DEFAULT_NICK
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.feature.channel.ChannelIo
import social.waddle.android.feature.dm.DmIo
import social.waddle.android.viewModelFactoryOf

/**
 * One XEP-0201 thread of a channel or DM: the shared conversation
 * engine with the thread filter engaged — rows narrow to the thread
 * (plus its root) and every composer send targets it.
 */
class ThreadViewModel(
    sessionManager: XmppSessionManager,
    conversationJid: String,
    isGroupchat: Boolean,
    threadId: String,
    nick: String,
    onConversationRead: suspend (String) -> Unit = {},
) : ConversationViewModel(
    conversationJid = conversationJid,
    isGroupchat = isGroupchat,
    timeline = sessionManager.timelineStore.timeline(conversationJid),
    events = sessionManager.events,
    unreadStore = sessionManager.unreadStore,
    io = if (isGroupchat) {
        ChannelIo(sessionManager, conversationJid, nick)
    } else {
        DmIo(sessionManager, conversationJid)
    },
    typingNames = sessionManager.chatStateStore.composingNames(conversationJid),
    pinnedIds = sessionManager.pinStore.pinnedIds(conversationJid),
    threadId = threadId,
    onConversationRead = onConversationRead,
) {
    companion object {
        fun factory(
            graph: AppGraph,
            conversationJid: String,
            isGroupchat: Boolean,
            threadId: String,
        ): ViewModelProvider.Factory = viewModelFactoryOf {
            ThreadViewModel(
                sessionManager = graph.sessionManager,
                conversationJid = conversationJid,
                isGroupchat = isGroupchat,
                threadId = threadId,
                nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK,
                onConversationRead = graph.messageNotifier::clearConversationNotification,
            )
        }
    }
}
