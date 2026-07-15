package social.waddle.android.feature.dm

import androidx.lifecycle.ViewModelProvider
import social.waddle.android.AppGraph
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.feature.conversation.AttachmentUploader
import social.waddle.android.feature.conversation.ConversationIo
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleMamPage

/** 1:1 DM conversation: DM MAM paging and chat sends (no join step). */
class DmViewModel(
    sessionManager: XmppSessionManager,
    peerJid: String,
    uploader: AttachmentUploader? = null,
    onConversationRead: suspend (String) -> Unit = {},
) : ConversationViewModel(
    conversationJid = peerJid,
    isGroupchat = false,
    timeline = sessionManager.timelineStore.timeline(peerJid),
    events = sessionManager.events,
    unreadStore = sessionManager.unreadStore,
    io = DmIo(sessionManager, peerJid),
    typingNames = sessionManager.chatStateStore.composingNames(peerJid),
    uploader = uploader,
    onConversationRead = onConversationRead,
) {
    companion object {
        fun factory(graph: AppGraph, peerJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf {
                DmViewModel(
                    sessionManager = graph.sessionManager,
                    peerJid = peerJid,
                    uploader = graph.attachmentUploader,
                    onConversationRead = graph.messageNotifier::clearConversationNotification,
                )
            }
    }
}

internal class DmIo(
    private val sessionManager: XmppSessionManager,
    private val peerJid: String,
) : ConversationIo {
    override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        sessionManager.fetchDmHistory(peerJid, maxMessages, beforeId)

    override suspend fun send(body: String, extras: MessageSendExtras?): SendResult =
        sessionManager.sendChatMessage(peerJid, body, extras)

    override fun recordConversationSeen() {
        sessionManager.recordDmSeen(peerJid)
    }

    override suspend fun sendChatState(state: WaddleChatState) {
        sessionManager.sendChatState(peerJid, isGroupchat = false, state = state)
    }

    override suspend fun markDisplayed() {
        sessionManager.markConversationDisplayed(peerJid, isGroupchat = false)
    }

    override suspend fun sendReaction(targetId: String, emojis: List<String>, previousEmojis: List<String>): Boolean =
        sessionManager.sendReaction(peerJid, isGroupchat = false, targetStanzaId = targetId, emojis = emojis, previousEmojis = previousEmojis)

    override suspend fun sendCorrection(targetId: String, newBody: String): Boolean =
        sessionManager.sendCorrection(peerJid, isGroupchat = false, targetId = targetId, newBody = newBody)

    override suspend fun sendRetraction(targetId: String): Boolean =
        sessionManager.sendRetraction(peerJid, isGroupchat = false, targetStanzaId = targetId)
}
