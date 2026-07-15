package social.waddle.android.feature.dm

import androidx.lifecycle.ViewModelProvider
import social.waddle.android.AppGraph
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.feature.conversation.ConversationIo
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleSendMessageOutcome

/** 1:1 DM conversation: DM MAM paging and chat sends (no join step). */
class DmViewModel(
    sessionManager: XmppSessionManager,
    peerJid: String,
) : ConversationViewModel(
    conversationJid = peerJid,
    timeline = sessionManager.timelineStore.timeline(peerJid),
    events = sessionManager.events,
    unreadStore = sessionManager.unreadStore,
    io = DmIo(sessionManager, peerJid),
) {
    companion object {
        fun factory(graph: AppGraph, peerJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf {
                DmViewModel(sessionManager = graph.sessionManager, peerJid = peerJid)
            }
    }
}

private class DmIo(
    private val sessionManager: XmppSessionManager,
    private val peerJid: String,
) : ConversationIo {
    override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        sessionManager.fetchDmHistory(peerJid, maxMessages, beforeId)

    override suspend fun send(body: String): WaddleSendMessageOutcome =
        sessionManager.sendChatMessage(peerJid, body)
}
