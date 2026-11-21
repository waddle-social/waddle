package social.waddle.android.feature.dm

import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import social.waddle.android.AppGraph
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.NotifySettingsResult
import social.waddle.android.client.SendResult
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.prefs.TimelineDirection
import social.waddle.android.client.store.ContactPresence
import social.waddle.android.client.store.ConversationKind
import social.waddle.android.client.store.PresenceShow
import social.waddle.android.feature.conversation.AttachmentUploader
import social.waddle.android.feature.conversation.ConversationIo
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.android.jid.bareJidOf as selfBareJidOf

/** 1:1 DM conversation: DM MAM paging and chat sends (no join step). */
class DmViewModel(
    sessionManager: XmppSessionManager,
    peerJid: String,
    uploader: AttachmentUploader? = null,
    onConversationRead: suspend (String) -> Unit = {},
    timelineDirection: StateFlow<TimelineDirection> = MutableStateFlow(TimelineDirection.CHAT),
    selfBareJid: String? = null,
) : ConversationViewModel(
    conversationJid = peerJid,
    isGroupchat = false,
    timeline = sessionManager.timelineStore.timeline(peerJid),
    events = sessionManager.events,
    unreadStore = sessionManager.unreadStore,
    io = DmIo(sessionManager, peerJid),
    typingNames = sessionManager.chatStateStore.composingNames(peerJid),
    notifyMode = sessionManager.notifySettingsStore.modeFlow(peerJid, ConversationKind.DIRECT_CHAT),
    timelineDirection = timelineDirection,
    uploader = uploader,
    onConversationRead = onConversationRead,
    selfBareJid = selfBareJid,
) {
    private val peerBare = normalizedBareJid(peerJid)

    val peerPresence: StateFlow<ContactPresence?> = sessionManager.presenceStore.contacts
        .map { contacts -> contacts[peerBare] }
        .distinctUntilChanged()
        .stateIn(
            viewModelScope,
            SharingStarted.WhileSubscribed(5_000),
            sessionManager.presenceStore.contacts.value[peerBare],
        )

    val peerDirectoryShow: StateFlow<PresenceShow?> = sessionManager.peopleDirectory
        .map { dir -> dir.people.firstOrNull { it.bareJid == peerBare }?.show }
        .distinctUntilChanged()
        .stateIn(
            viewModelScope,
            SharingStarted.WhileSubscribed(5_000),
            sessionManager.peopleDirectory.value.people
                .firstOrNull { it.bareJid == peerBare }
                ?.show,
        )

    companion object {
        fun factory(graph: AppGraph, peerJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf {
                DmViewModel(
                    sessionManager = graph.sessionManager,
                    peerJid = peerJid,
                    timelineDirection = graph.timelineDirection,
                    uploader = graph.attachmentUploader,
                    onConversationRead = graph.messageNotifier::clearConversationNotification,
                    selfBareJid = graph.currentSession.value?.jid?.let(::selfBareJidOf),
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

    override suspend fun lookupLinkPreview(url: String) =
        sessionManager.lookupLinkPreview(url, scopeJid = peerJid)

    override fun recordConversationSeen() {
        sessionManager.recordDmSeen(peerJid)
    }

    override suspend fun sendChatState(state: WaddleChatState) {
        sessionManager.sendChatState(peerJid, isGroupchat = false, state = state)
    }

    override suspend fun markDisplayed() {
        sessionManager.markConversationDisplayed(peerJid, isGroupchat = false)
    }

    override suspend fun toggleReaction(targetId: String, emoji: String): VerbResult =
        sessionManager.toggleReaction(peerJid, isGroupchat = false, targetStanzaId = targetId, emoji = emoji)

    override suspend fun sendCorrection(targetId: String, newBody: String, threadId: String?): VerbResult =
        sessionManager.sendCorrection(
            peerJid,
            isGroupchat = false,
            targetId = targetId,
            newBody = newBody,
            threadId = threadId,
        )

    override suspend fun sendRetraction(targetId: String): VerbResult =
        sessionManager.sendRetraction(peerJid, isGroupchat = false, targetStanzaId = targetId)

    override suspend fun setNotificationMode(mode: WaddleNotifyMode): NotifySettingsResult =
        sessionManager.setDmNotificationMode(peerJid, mode)
}
