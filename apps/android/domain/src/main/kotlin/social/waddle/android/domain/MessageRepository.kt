package social.waddle.android.domain

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.launchIn
import kotlinx.coroutines.flow.onEach
import social.waddle.android.ffi.WaddleClientHandle
import social.waddle.android.ffi.WaddleEvent
import uniffi.waddle_xmpp_client.WaddleArchivedMessage
import uniffi.waddle_xmpp_client.WaddleMessage
import uniffi.waddle_xmpp_client.WaddleSendOptions

public sealed interface ConversationKey {
    public data class Room(val jid: String) : ConversationKey
    public data class Dm(val peer: String) : ConversationKey
}

public data class Timeline(
    public val live: List<WaddleMessage> = emptyList(),
    public val archived: List<WaddleArchivedMessage> = emptyList(),
    public val isComplete: Boolean = false,
)

public class MessageRepository(
    private val client: WaddleClientHandle,
    scope: CoroutineScope,
) {
    private val timelines = mutableMapOf<ConversationKey, MutableStateFlow<Timeline>>()
    private val lock = Any()

    public val rawEvents: SharedFlow<WaddleEvent> get() = client.events

    init {
        client.events
            .filterIsInstance<WaddleEvent.Message>()
            .onEach { ingest(it.message) }
            .launchIn(scope)

        client.events
            .filterIsInstance<WaddleEvent.MamResult>()
            .onEach { ingestArchived(it.message) }
            .launchIn(scope)
    }

    public fun timeline(key: ConversationKey): StateFlow<Timeline> =
        timelineState(key).asStateFlow()

    public suspend fun backfill(key: ConversationKey, max: UInt = 50u, beforeId: String? = null) {
        val page = when (key) {
            is ConversationKey.Room -> client.fetchRoomHistory(key.jid, max, beforeId)
            is ConversationKey.Dm -> client.fetchDmHistory(key.peer, max, beforeId)
        }
        val state = timelineState(key)
        state.value = state.value.copy(
            archived = (page.messages + state.value.archived).distinctBy { it.mamId },
            isComplete = page.isComplete,
        )
    }

    public suspend fun sendRoom(roomJid: String, body: String, options: WaddleSendOptions? = null): Unit =
        client.sendGroupchatMessage(roomJid, body, options)

    public suspend fun sendDm(peerJid: String, body: String, options: WaddleSendOptions? = null): Unit =
        client.sendChatMessage(peerJid, body, options)

    private fun ingest(message: WaddleMessage) {
        val key = message.toConversationKey() ?: return
        val state = timelineState(key)
        state.value = state.value.copy(live = state.value.live + message)
    }

    private fun ingestArchived(message: WaddleArchivedMessage) {
        val key = message.toConversationKey() ?: return
        val state = timelineState(key)
        state.value = state.value.copy(archived = state.value.archived + message)
    }

    private fun timelineState(key: ConversationKey): MutableStateFlow<Timeline> =
        synchronized(lock) {
            timelines.getOrPut(key) { MutableStateFlow(Timeline()) }
        }

    private fun WaddleMessage.toConversationKey(): ConversationKey? {
        val from = from ?: return null
        return if (isMuc) ConversationKey.Room(from.substringBefore('/')) else ConversationKey.Dm(from)
    }

    private fun WaddleArchivedMessage.toConversationKey(): ConversationKey? {
        val from = from ?: return null
        // MAM doesn't include is_muc; conservatively bucket by bare JID.
        return ConversationKey.Dm(from.substringBefore('/'))
    }
}
