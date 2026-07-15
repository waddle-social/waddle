package social.waddle.android.feature.channel

import androidx.lifecycle.ViewModelProvider
import social.waddle.android.AppGraph
import social.waddle.android.DEFAULT_NICK
import social.waddle.android.client.SendResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.feature.conversation.ConversationIo
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleMamPage

/** MUC channel conversation: joins on open, room MAM, groupchat sends. */
class ChannelViewModel(
    sessionManager: XmppSessionManager,
    roomJid: String,
    nick: String,
) : ConversationViewModel(
    conversationJid = roomJid,
    timeline = sessionManager.timelineStore.timeline(roomJid),
    events = sessionManager.events,
    unreadStore = sessionManager.unreadStore,
    io = ChannelIo(sessionManager, roomJid, nick),
) {
    companion object {
        fun factory(graph: AppGraph, roomJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf {
                ChannelViewModel(
                    sessionManager = graph.sessionManager,
                    roomJid = roomJid,
                    nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK,
                )
            }
    }
}

private class ChannelIo(
    private val sessionManager: XmppSessionManager,
    private val roomJid: String,
    private val nick: String,
) : ConversationIo {
    /** Join only when not already joined (Home joins ahead of navigation). */
    override suspend fun ensureJoined() {
        if (roomJid !in sessionManager.roomStore.joinedRooms.value) {
            sessionManager.joinRoom(roomJid, nick)
        }
    }

    override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        sessionManager.fetchRoomHistory(roomJid, maxMessages, beforeId)

    override suspend fun send(body: String): SendResult =
        sessionManager.sendGroupchatMessage(roomJid, body)
}
