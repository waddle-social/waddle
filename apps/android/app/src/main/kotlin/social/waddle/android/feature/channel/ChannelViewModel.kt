package social.waddle.android.feature.channel

import androidx.lifecycle.ViewModelProvider
import kotlinx.coroutines.flow.map
import social.waddle.android.AppGraph
import social.waddle.android.DEFAULT_NICK
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.NotifySettingsResult
import social.waddle.android.client.SendResult
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppSessionRuntime
import social.waddle.android.client.mentionCandidatesOf
import social.waddle.android.client.store.ConversationKind
import social.waddle.android.feature.conversation.AttachmentUploader
import social.waddle.android.feature.conversation.ConversationIo
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.viewModelFactoryOf
import social.waddle.client.ffi.WaddleChatState
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleNotifyMode

/** MUC channel conversation: joins on open, room MAM, groupchat sends. */
class ChannelViewModel(
    sessionRuntime: XmppSessionRuntime,
    roomJid: String,
    nick: String,
    uploader: AttachmentUploader? = null,
    onConversationRead: suspend (String) -> Unit = {},
) : ConversationViewModel(
    conversationJid = roomJid,
    isGroupchat = true,
    timeline = sessionRuntime.timelineStore.timeline(roomJid),
    events = sessionRuntime.events,
    unreadStore = sessionRuntime.unreadStore,
    io = ChannelIo(sessionRuntime, roomJid, nick),
    typingNames = sessionRuntime.chatStateStore.composingNames(roomJid),
    pinnedIds = sessionRuntime.pinStore.pinnedIds(roomJid),
    mentionCandidates = sessionRuntime.presenceStore.occupants
        .map { rooms -> mentionCandidatesOf(rooms[roomJid].orEmpty()) },
    // No public/private discriminator on the topology yet (web
    // parity): every MUC resolves as a private group (§3: always).
    notifyMode = sessionRuntime.notifySettingsStore.modeFlow(roomJid, ConversationKind.PRIVATE_GROUP),
    uploader = uploader,
    onConversationRead = onConversationRead,
) {
    companion object {
        fun factory(graph: AppGraph, roomJid: String): ViewModelProvider.Factory =
            viewModelFactoryOf {
                val ownerBareJid = graph.currentSession.value
                    ?.jid
                    ?.substringBefore('/')
                    ?.takeIf(String::isNotBlank)
                ChannelViewModel(
                    sessionRuntime = graph.sessionRuntime,
                    roomJid = roomJid,
                    nick = graph.currentSession.value?.xmppLocalpart ?: DEFAULT_NICK,
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

internal class ChannelIo(
    private val sessionRuntime: XmppSessionRuntime,
    private val roomJid: String,
    private val nick: String,
) : ConversationIo {
    /** Join only when not already joined (Home joins ahead of navigation). */
    override suspend fun ensureJoined() {
        if (roomJid !in sessionRuntime.roomStore.joinedRooms.value) {
            sessionRuntime.joinRoom(roomJid, nick)
        }
        sessionRuntime.refreshRoomPins(roomJid)
    }

    override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage? =
        sessionRuntime.fetchRoomHistory(roomJid, maxMessages, beforeId)

    override suspend fun send(body: String, extras: MessageSendExtras?): SendResult =
        sessionRuntime.sendGroupchatMessage(roomJid, body, extras)

    override suspend fun sendChatState(state: WaddleChatState) {
        sessionRuntime.sendChatState(roomJid, isGroupchat = true, state = state)
    }

    override suspend fun markDisplayed() {
        sessionRuntime.markConversationDisplayed(roomJid, isGroupchat = true)
    }

    override suspend fun toggleReaction(targetId: String, emoji: String): VerbResult =
        sessionRuntime.toggleReaction(roomJid, isGroupchat = true, targetStanzaId = targetId, emoji = emoji)

    override suspend fun sendCorrection(targetId: String, newBody: String, threadId: String?): VerbResult =
        sessionRuntime.sendCorrection(
            roomJid,
            isGroupchat = true,
            targetId = targetId,
            newBody = newBody,
            threadId = threadId,
        )

    override suspend fun sendRetraction(targetId: String): VerbResult =
        sessionRuntime.sendRetraction(roomJid, isGroupchat = true, targetStanzaId = targetId)

    override val canPin: Boolean get() = true

    override suspend fun setPinned(targetId: String, pinned: Boolean): VerbResult =
        sessionRuntime.pinRoomMessage(roomJid, targetId, pinned)

    override suspend fun setNotificationMode(mode: WaddleNotifyMode): NotifySettingsResult =
        sessionRuntime.setRoomNotificationMode(roomJid, mode)
}
