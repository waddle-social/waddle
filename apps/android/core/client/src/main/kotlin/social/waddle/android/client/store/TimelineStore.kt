package social.waddle.android.client.store

import java.time.Instant
import java.time.OffsetDateTime
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.client.bareJid
import social.waddle.android.client.conversationKeyOf
import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleMessage

/**
 * Per-conversation ordered message lists: live messages insert as they
 * arrive, MAM pages merge in, and duplicates (XEP-0198 replays, MAM
 * refetch of a live message) collapse on the XEP-0359 stanza id when
 * present, else origin id, else message id. Ordering is by timestamp,
 * with insertion order breaking ties; items without a timestamp sort
 * after timestamped history (live messages are the newest).
 */
class TimelineStore {
    private val lock = Any()
    private val flows = HashMap<String, MutableStateFlow<List<TimelineItem>>>()
    private val entries = HashMap<String, MutableList<Entry>>()
    private var insertionCounter = 0L

    @Volatile
    private var ownBareJid: String? = null
    private var ownNick: String? = null

    fun setOwnBareJid(jid: String?) {
        ownBareJid = jid?.let(::bareJid)
        ownNick = ownBareJid?.substringBefore('@')
    }

    fun timeline(conversationJid: String): StateFlow<List<TimelineItem>> =
        synchronized(lock) { flowFor(bareJid(conversationJid)) }.asStateFlow()

    fun onLiveMessage(message: WaddleMessage) {
        val body = message.body ?: return
        val key = conversationKeyOf(
            ownBareJid = ownBareJid,
            ownNick = ownNick,
            from = message.from,
            to = message.to,
            isGroupchat = message.isMuc || message.messageType == "groupchat",
        ) ?: return
        insert(
            conversation = key.jid,
            item = TimelineItem(
                id = message.stanzaId ?: message.originId ?: message.id ?: return,
                conversationJid = key.jid,
                from = message.from,
                body = body,
                timestamp = message.timestamp,
                isMine = key.isMine,
                source = TimelineSource.Live(message),
            ),
        )
    }

    fun onArchivedMessage(message: WaddleArchivedMessage) {
        val body = message.body ?: return
        val key = conversationKeyOf(
            ownBareJid = ownBareJid,
            ownNick = ownNick,
            from = message.from,
            to = message.to,
            isGroupchat = message.messageType == "groupchat",
        ) ?: return
        insert(
            conversation = key.jid,
            item = TimelineItem(
                id = message.stanzaId ?: message.originId ?: message.id ?: message.mamId,
                conversationJid = key.jid,
                from = message.from,
                body = body,
                timestamp = message.timestamp,
                isMine = key.isMine,
                source = TimelineSource.Archived(message),
            ),
        )
    }

    fun clear() {
        synchronized(lock) {
            entries.clear()
            insertionCounter = 0L
            flows.values.forEach { it.value = emptyList() }
        }
    }

    private fun insert(conversation: String, item: TimelineItem) {
        synchronized(lock) {
            val list = entries.getOrPut(conversation) { mutableListOf() }
            val existingIndex = list.indexOfFirst { it.item.id == item.id }
            if (existingIndex >= 0) {
                val existing = list[existingIndex]
                // A live record supersedes its archived twin (richer
                // payload); otherwise the first record wins and the
                // replay is dropped.
                if (item.source is TimelineSource.Live && existing.item.source is TimelineSource.Archived) {
                    list[existingIndex] = existing.copy(
                        item = item.copy(timestamp = item.timestamp ?: existing.item.timestamp),
                    )
                    publish(conversation, list)
                }
                return
            }
            list += Entry(
                item = item,
                sortInstant = item.timestamp?.let { parseInstant(it) },
                order = insertionCounter++,
            )
            list.sortWith(ENTRY_ORDER)
            publish(conversation, list)
        }
    }

    private fun publish(conversation: String, list: List<Entry>) {
        flowFor(conversation).value = list.map { it.item }
    }

    private fun flowFor(conversation: String): MutableStateFlow<List<TimelineItem>> =
        flows.getOrPut(conversation) { MutableStateFlow(emptyList()) }

    private data class Entry(
        val item: TimelineItem,
        val sortInstant: Instant?,
        val order: Long,
    )

    private companion object {
        val ENTRY_ORDER: Comparator<Entry> =
            compareBy<Entry> { it.sortInstant ?: Instant.MAX }.thenBy { it.order }

        fun parseInstant(timestamp: String): Instant? =
            runCatching { Instant.parse(timestamp) }.getOrElse {
                runCatching { OffsetDateTime.parse(timestamp).toInstant() }.getOrNull()
            }
    }
}
