package social.waddle.android.feature.dm

import androidx.lifecycle.ViewModelProvider
import social.waddle.android.AppGraph
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.NotifySettingsResult
import social.waddle.android.client.SendResult
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppSessionRuntime
import social.waddle.android.client.store.ConversationKind
import social.waddle.android.feature.conversation.AttachmentUploader
import social.waddle.android.feature.conversation.ConversationIo
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleNotifyMode

/** 1:1 DM conversation: DM MAM paging and chat sends (no join step). */
class DmViewModel(
    sessionRuntime: XmppSessionRuntime,
    peerJid: String,
    uploader: AttachmentUploader? = null,
    onConversationRead: suspend (String) -> Unit = {},
) : ConversationViewModel(
    conversationJid = peerJid,
    isGroupchat = false,
    timeline = sessionRuntime.timelineStore.timeline(peerJid),
    events = sessionRuntime.events,
    unreadStore = sessionRuntime.unreadStore,
    io = DmIo(sessionRuntime, peerJid),
    typingNames = sessionRuntime.chatStateStore.composingNames(peerJid),
    notifyMode = sessionRuntime.notifySettingsStore.modeFlow(peerJid, ConversationKind.DIRECT_CHAT),
    uploader = uploader,
    onConversationRead = onConversationRead,
) {
    companion object {
        fun factory(graph: AppGraph, peerJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf {
                val ownerBareJid = graph.currentSession.value
                    ?.jid
                    ?.substringBefore('/')
                    ?.takeIf(String::isNotBlank)
                DmViewModel(
                    sessionRuntime = graph.sessionRuntime,
                    peerJid = peerJid,
                    uploader = graph.attachmentUploader,
                    onConversationRead = { conversationJid ->
                        ownerBareJid?.let {
                            graph.messageNotifier.clearConversationNotification(it, conversationJid)
                        }
                    },
                )
            }
    }
}

internal class DmIo(
    private val sessionRuntime: XmppSessionRuntime,
    private val peerJid: String,
) : ConversationIo {
    override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        sessionRuntime.fetchDmHistory(peerJid, maxMessages, beforeId)

    override suspend fun send(body: String, extras: MessageSendExtras?): SendResult =
        sessionRuntime.sendChatMessage(peerJid, body, extras)

    override fun recordConversationSeen() {
        sessionRuntime.recordDmSeen(peerJid)
    }

    override suspend fun sendChatState(state: WaddleChatState) {
        sessionRuntime.sendChatState(peerJid, isGroupchat = false, state = state)
    }

    override suspend fun markDisplayed() {
        sessionRuntime.markConversationDisplayed(peerJid, isGroupchat = false)
    }

    override suspend fun toggleReaction(targetId: String, emoji: String): VerbResult =
        sessionRuntime.toggleReaction(peerJid, isGroupchat = false, targetStanzaId = targetId, emoji = emoji)

    override suspend fun sendCorrection(targetId: String, newBody: String, threadId: String?): VerbResult =
        sessionRuntime.sendCorrection(
            peerJid,
            isGroupchat = false,
            targetId = targetId,
            newBody = newBody,
            threadId = threadId,
        )

    override suspend fun sendRetraction(targetId: String): VerbResult =
        sessionRuntime.sendRetraction(peerJid, isGroupchat = false, targetStanzaId = targetId)

    override suspend fun setNotificationMode(mode: WaddleNotifyMode): NotifySettingsResult =
        sessionRuntime.setDmNotificationMode(peerJid, mode)
}
