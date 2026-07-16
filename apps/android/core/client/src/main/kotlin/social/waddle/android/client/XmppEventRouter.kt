package social.waddle.android.client

import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence
import social.waddle.android.client.store.SessionStores
import social.waddle.android.client.store.isTimelineMutation
import social.waddle.client.ffi.WaddleMessage

/**
 * Single fan-out point for the serialized domain event stream: every
 * event updates the stores first, then lands on the shared [events]
 * flow the app consumes. The single-consumer ordering is what lets
 * read-modify-write consumers (unread recomputes vs live increments)
 * stay lock-free.
 */
internal class XmppEventRouter(
    private val activeSession: ActiveSession,
    private val stores: SessionStores,
    private val resume: ResumePersistence,
    private val readState: ReadStateCoordinator,
    private val persistDmSeen: (peer: String, timestamp: String) -> Unit,
) {
    private val _events = MutableSharedFlow<XmppEvent>(
        extraBufferCapacity = EVENT_BUFFER_CAPACITY,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    /** Every domain event, after store fan-out; drops oldest under burst. */
    val events: SharedFlow<XmppEvent> = _events.asSharedFlow()

    /** Emit a locally-computed event without any store fan-out. */
    fun emit(event: XmppEvent) {
        _events.tryEmit(event)
    }

    /** Single fan-out point: domain stores first, then the shared stream. */
    fun dispatch(event: XmppEvent) {
        when (event) {
            is XmppEvent.Message -> routeMessage(event.message)
            is XmppEvent.MdsEntries -> {
                event.entries.forEach(readState::applyMdsEntry)
                event.applied?.complete()
            }
            is XmppEvent.RoomPins ->
                stores.pinStore.seed(event.roomJid, event.entries, event.fetchedAtVersion)
            is XmppEvent.Presence -> stores.presenceStore.onPresence(event.presence)
            is XmppEvent.MamResult -> routeMamResult(event)
            else -> Unit
        }
        _events.tryEmit(event)
    }

    private fun routeMessage(message: WaddleMessage) {
        // A live MDS PEP event is pure read-state metadata from a
        // sibling device — apply the cursors and skip every chat
        // consumer.
        message.mdsDisplayed?.let { entries ->
            entries.forEach(readState::applyMdsEntry)
            return
        }
        // Pin/unpin room broadcasts mutate the pin set, not the
        // timeline.
        message.pinEvent?.let { pin ->
            message.from?.let { from -> stores.pinStore.onPinEvent(bareJid(from), pin) }
            return
        }
        // Mutation stanzas (reactions/corrections/retractions/
        // moderation) alter existing rows via the timeline store; they
        // are not new DM activity and must not reorder or re-persist
        // recency.
        val isMutation = message.isTimelineMutation()
        // Bodyless protocol stanzas (chat states, displayed markers)
        // are not DM activity either — a typing burst must not reorder
        // or create DM-list entries.
        val hasContent = message.body != null
        if (!isMutation && hasContent) persistDmRecency(message)
        val newlyInserted = stores.timelineStore.onLiveMessage(message)
        if (!isMutation && hasContent) {
            stores.dmStore.onChatMessage(activeSession.ownBareJid, message)
        }
        val key = conversationKeyOf(
            ownBareJid = activeSession.ownBareJid,
            ownNick = activeSession.ownBareJid?.substringBefore('@'),
            from = message.from,
            to = message.to,
            isGroupchat = message.isMuc || message.messageType == "groupchat",
        ) ?: return
        trackChatState(key, message)
        if (message.body != null) recordActivity(key, message, newlyInserted)
    }

    /** Unread + resume-cursor bookkeeping for a content-bearing message. */
    private fun recordActivity(
        key: ConversationKey,
        message: WaddleMessage,
        newlyInserted: Boolean,
    ) {
        // Replays the timeline deduped (XEP-0198 resume) must not
        // inflate the badge for a message that renders once. Thread
        // REPLIES are hidden from the feed and never count either (web
        // feed-only unread parity).
        // Must match TimelineItem.isFeedVisible exactly (all XEP-0359
        // ids, not just the first): a root whose thread id lands on a
        // secondary stanza-id is feed-visible and MUST bump unread.
        val rootIds = setOfNotNull(message.stanzaId, message.originId, message.id) +
            message.stanzaIds.map { it.id }
        val isThreadReply = message.thread != null &&
            message.thread !in rootIds &&
            message.callThread == null
        if (newlyInserted && !isThreadReply) {
            stores.unreadStore.onLiveMessage(key.jid, key.isMine)
        }
        resume.recordCursor(
            conversationJid = key.jid,
            stanzaId = message.stanzaId ?: message.originId ?: message.id,
            timestamp = message.timestamp,
        )
    }

    private fun routeMamResult(event: XmppEvent.MamResult) {
        stores.timelineStore.onArchivedMessage(event.message)
        val message = event.message
        if (message.body == null) return
        conversationKeyOf(
            ownBareJid = activeSession.ownBareJid,
            ownNick = activeSession.ownBareJid?.substringBefore('@'),
            from = message.from,
            to = message.to,
            isGroupchat = message.messageType == "groupchat",
        )?.let { key ->
            resume.recordCursor(
                conversationJid = key.jid,
                stanzaId = message.stanzaId ?: message.originId ?: message.id ?: message.mamId,
                timestamp = message.timestamp,
            )
        }
    }

    /**
     * XEP-0085 bookkeeping from the fan-out: a `composing` state adds
     * the sender to the conversation's typing set, anything else — a
     * different state or a real message — removes them. Sender display
     * name: the occupant nick in a MUC, the peer's LOCALPART in 1:1 —
     * DM states arrive from the peer's full JID, whose resource is a
     * device identifier, never a name.
     */
    private fun trackChatState(key: ConversationKey, message: WaddleMessage) {
        val from = message.from ?: return
        val isGroupchat = message.isMuc || message.messageType == "groupchat"
        val sender = if (isGroupchat) {
            resourcepart(from) ?: bareJid(from).substringBefore('@')
        } else {
            bareJid(from).substringBefore('@')
        }
        val state = message.chatState
        if (state != null) {
            stores.chatStateStore.onChatState(key.jid, sender, state, key.isMine)
        }
        if (message.body != null && !key.isMine) {
            stores.chatStateStore.onLiveMessage(key.jid, sender)
        }
    }

    /**
     * Persist DM-list recency for inbound 1:1 traffic; without a write
     * the `lastSeen`-seeded DM list is empty after every restart.
     */
    private fun persistDmRecency(message: WaddleMessage) {
        if (message.isMuc || message.messageType != "chat") return
        val from = message.from ?: return
        val peer = bareJid(from)
        if (peer == activeSession.ownBareJid) return
        persistDmSeen(peer, message.timestamp ?: nowRfc3339())
    }

    /**
     * 1s expiry ticker for incoming typing indicators — armed ONLY while
     * someone is composing. An unconditional periodic timer would keep a
     * task perpetually pending on virtual-time test schedulers (and wake
     * the process forever); this one parks on the composing flow when
     * idle and dies with the last composer.
     */
    suspend fun sweepChatStates() {
        stores.chatStateStore.composing
            .map { it.isNotEmpty() }
            .distinctUntilChanged()
            .collectLatest { anyComposing ->
                if (!anyComposing) return@collectLatest
                // Loop exit is driven ONLY by collectLatest observing the
                // empty state (sweep publishes it, StateFlow guarantees
                // delivery of the latest value) — breaking on sweep()'s
                // return raced a conflated empty→composing flip and left
                // a live composer with no ticker.
                while (true) {
                    delay(CHAT_STATE_SWEEP_MILLIS)
                    stores.chatStateStore.sweep()
                }
            }
    }

    companion object {
        /** Incoming-typing expiry tick (XEP-0085 indicator sweep). */
        const val CHAT_STATE_SWEEP_MILLIS = 1_000L

        private const val EVENT_BUFFER_CAPACITY = 256
    }
}
