package social.waddle.android.feature.channel

import androidx.lifecycle.ViewModelProvider
import social.waddle.android.AppGraph
import social.waddle.android.DEFAULT_NICK
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.feature.conversation.AttachmentUploader
import social.waddle.android.feature.conversation.ConversationIo
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleMamPage

/** MUC channel conversation: joins on open, room MAM, groupchat sends. */
class ChannelViewModel(
    sessionManager: XmppSessionManager,
    roomJid: String,
    nick: String,
    uploader: AttachmentUploader? = null,
    onConversationRead: suspend (String) -> Unit = {},
) : ConversationViewModel(
    conversationJid = roomJid,
    isGroupchat = true,
    timeline = sessionManager.timelineStore.timeline(roomJid),
    events = sessionManager.events,
    unreadStore = sessionManager.unreadStore,
    io = ChannelIo(sessionManager, roomJid, nick),
    typingNames = sessionManager.chatStateStore.composingNames(roomJid),
    pinnedIds = sessionManager.pinStore.pinnedIds(roomJid),
    uploader = uploader,
    onConversationRead = onConversationRead,
) {
    companion object {
        fun factory(graph: AppGraph, roomJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf {
                ChannelViewModel(
                    sessionManager = graph.sessionManager,
                    roomJid = roomJid,
                    nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK,
                    uploader = graph.attachmentUploader,
                    onConversationRead = graph.messageNotifier::clearConversationNotification,
                )
            }
    }
}

internal class ChannelIo(
    private val sessionManager: XmppSessionManager,
    private val roomJid: String,
    private val nick: String,
) : ConversationIo {
    /** Join only when not already joined (Home joins ahead of navigation). */
    override suspend fun ensureJoined() {
        if (roomJid !in sessionManager.roomStore.joinedRooms.value) {
            sessionManager.joinRoom(roomJid, nick)
        }
        sessionManager.refreshRoomPins(roomJid)
    }

    override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        sessionManager.fetchRoomHistory(roomJid, maxMessages, beforeId)

    override suspend fun send(body: String, extras: MessageSendExtras?): SendResult =
        sessionManager.sendGroupchatMessage(roomJid, body, extras)

    override suspend fun sendChatState(state: WaddleChatState) {
        sessionManager.sendChatState(roomJid, isGroupchat = true, state = state)
    }

    override suspend fun markDisplayed() {
        sessionManager.markConversationDisplayed(roomJid, isGroupchat = true)
    }

    override suspend fun sendReaction(targetId: String, emojis: List<String>, previousEmojis: List<String>): Boolean =
        sessionManager.sendReaction(roomJid, isGroupchat = true, targetStanzaId = targetId, emojis = emojis, previousEmojis = previousEmojis)

    override suspend fun sendCorrection(targetId: String, newBody: String, threadId: String?): Boolean =
        sessionManager.sendCorrection(roomJid, isGroupchat = true, targetId = targetId, newBody = newBody, threadId = threadId)

    override suspend fun sendRetraction(targetId: String): Boolean =
        sessionManager.sendRetraction(roomJid, isGroupchat = true, targetStanzaId = targetId)

    override val canPin: Boolean get() = true

    override suspend fun setPinned(targetId: String, pinned: Boolean): Boolean =
        sessionManager.pinRoomMessage(roomJid, targetId, pinned)
}
