package social.waddle.android.client.store

import java.time.Instant
import java.time.OffsetDateTime
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.client.bareJid
import social.waddle.android.client.conversationKeyOf
import social.waddle.android.client.stripReplyFallback
import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleMessage

/**
 * Per-conversation ordered message lists: live messages insert as they
 * arrive, MAM pages merge in, and duplicates (XEP-0198 replays, MAM
 * refetch of a live message) collapse on the XEP-0359 stanza id when
 * present, else origin id, else message id. Ordering is by timestamp,
 * with insertion order breaking ties; items without a timestamp sort
 * after timestamped history (live messages are the newest).
 *
 * Mutation messages (XEP-0444 reactions, XEP-0308 corrections, XEP-0424
 * retractions, XEP-0425 moderation — see [MessageMutation]) never insert
 * rows; they are applied to the row whose wire identity contains their
 * target id. Latest-wins per mutation kind: a mutation's rank is its
 * timestamp (live mutations, which carry none, rank newest), insertion
 * order breaking ties. Mutations arriving before their target (backwards
 * MAM paging fetches newest-first, so a reaction loads before the
 * message it targets) are held in a bounded per-conversation pending
 * index and applied when the target inserts; overflow drops oldest —
 * recoverable the same way pruned rows are, by re-paging.
 *
 * Bounded-timeline invariant: only a LIVE insert enforces
 * [maxItemsPerConversation], trimming from the OLDEST end; archived
 * (MAM) inserts never trim. Rationale:
 * - The cap exists to stop unbounded growth from live traffic in a
 *   long-running session. MAM merges are explicitly requested history
 *   whose growth is already bounded by the screens' page budgets, and
 *   trimming on merge would either evict the page the user just asked
 *   for (oldest end) or drop unseen live rows (newest end).
 * - Consequently a backfilling conversation may temporarily exceed the
 *   cap; the next live arrival re-trims to the cap, evicting oldest
 *   (backfilled) rows first — recoverable, because backwards paging can
 *   re-fetch them.
 * - Pruned ids are deliberately forgotten (no tombstone index): the only
 *   path that re-delivers a pruned message is a backwards MAM page into
 *   the pruned region, and re-adding it there is exactly what the
 *   paging user wants. XEP-0198 replays and MAM refetches of RECENT
 *   messages land in the newest window, which is never trimmed away
 *   from under them.
 */
class TimelineStore(
    private val maxItemsPerConversation: Int = MAX_ITEMS_PER_CONVERSATION,
    private val maxPendingMutationsPerConversation: Int = MAX_PENDING_MUTATIONS,
) {
    private val lock = Any()
    private val flows = HashMap<String, MutableStateFlow<List<TimelineItem>>>()
    private val entries = HashMap<String, MutableList<Entry>>()
    private val pendingMutations = HashMap<String, ArrayDeque<RankedMutation>>()
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

    /**
     * Returns true only when the message was a genuinely NEW timeline
     * row — XEP-0198 replays, live/archive twins, and mutation messages
     * all return false so callers (the unread counter) don't count what
     * the timeline itself never renders as new content.
     */
    fun onLiveMessage(message: WaddleMessage): Boolean {
        val isGroupchat = message.isMuc || message.messageType == "groupchat"
        val key = conversationKeyOf(
            ownBareJid = ownBareJid,
            ownNick = ownNick,
            from = message.from,
            to = message.to,
            isGroupchat = isGroupchat,
        ) ?: return false
        mutationOf(message, isGroupchat = isGroupchat, mine = key.isMine)?.let { mutation ->
            applyMutation(key.jid, mutation, isGroupchat, timestamp = message.timestamp)
            return false
        }
        val body = message.body ?: return false
        return insert(
            conversation = key.jid,
            item = TimelineItem(
                id = message.stanzaId ?: message.originId ?: message.id ?: return false,
                conversationJid = key.jid,
                from = message.from,
                body = stripReplyFallback(body, message.replyFallbackStart, message.replyFallbackEnd),
                timestamp = message.timestamp,
                isMine = key.isMine,
                source = TimelineSource.Live(message),
            ),
            isGroupchat = isGroupchat,
            initialTombstone = null,
        )
    }

    fun onArchivedMessage(message: WaddleArchivedMessage) {
        val isGroupchat = message.messageType == "groupchat"
        val key = conversationKeyOf(
            ownBareJid = ownBareJid,
            ownNick = ownNick,
            from = message.from,
            to = message.to,
            isGroupchat = isGroupchat,
        ) ?: return
        mutationOf(message, isGroupchat = isGroupchat, mine = key.isMine)?.let { mutation ->
            applyMutation(key.jid, mutation, isGroupchat, timestamp = message.timestamp)
            return
        }
        val body = message.body ?: return
        insert(
            conversation = key.jid,
            item = TimelineItem(
                id = message.stanzaId ?: message.originId ?: message.id ?: message.mamId,
                conversationJid = key.jid,
                from = message.from,
                body = stripReplyFallback(body, message.replyFallbackStart, message.replyFallbackEnd),
                timestamp = message.timestamp,
                isMine = key.isMine,
                source = TimelineSource.Archived(message),
            ),
            isGroupchat = isGroupchat,
            // The archive returns retracted originals as tombstones.
            initialTombstone = if (message.isRetracted) MessageTombstone.Retracted else null,
        )
    }

    /**
     * Apply one of the account's OWN mutations optimistically (a DM
     * send is never reflected back to the sending client, so waiting
     * for an echo would leave the sender's UI stale; the MUC reflection
     * re-applies idempotently). Same pipeline as wire mutations —
     * sender checks and ranking included — so a bad local apply cannot
     * do anything a spoofed stanza couldn't.
     */
    fun applyLocalMutation(conversationJid: String, mutation: MessageMutation, isGroupchat: Boolean) {
        applyMutation(bareJid(conversationJid), mutation, isGroupchat, timestamp = null)
    }

    fun clear() {
        synchronized(lock) {
            entries.clear()
            pendingMutations.clear()
            insertionCounter = 0L
            flows.values.forEach { it.value = emptyList() }
        }
    }

    private fun insert(
        conversation: String,
        item: TimelineItem,
        isGroupchat: Boolean,
        initialTombstone: MessageTombstone?,
    ): Boolean {
        synchronized(lock) {
            val list = entries.getOrPut(conversation) { mutableListOf() }
            val existingIndex = list.indexOfFirst { it.item.id == item.id }
            if (existingIndex >= 0) {
                val existing = list[existingIndex]
                // A live record supersedes its archived twin (richer
                // payload); otherwise the first record wins and the
                // replay is dropped. Applied mutations live on the entry
                // and survive the swap.
                if (item.source is TimelineSource.Live && existing.item.source is TimelineSource.Archived) {
                    list[existingIndex] = existing.copy(
                        item = item.copy(timestamp = item.timestamp ?: existing.item.timestamp),
                    )
                    publish(conversation, list)
                }
                return false
            }
            var entry = Entry(
                item = item,
                sortInstant = item.timestamp?.let { parseInstant(it) },
                order = insertionCounter++,
                mutations = MutationState(tombstone = initialTombstone),
            )
            entry = drainPendingMutationsInto(conversation, entry, isGroupchat)
            list += entry
            list.sortWith(ENTRY_ORDER)
            if (item.source is TimelineSource.Live) {
                // Live-append overflow only — see the class KDoc invariant.
                while (list.size > maxItemsPerConversation) list.removeAt(0)
            }
            publish(conversation, list)
        }
        return true
    }

    private fun applyMutation(
        conversation: String,
        mutation: MessageMutation,
        isGroupchat: Boolean,
        timestamp: String?,
    ) {
        val ranked = RankedMutation(
            mutation = mutation,
            rank = Rank(
                instant = timestamp?.let { parseInstant(it) },
                order = synchronized(lock) { insertionCounter++ },
            ),
            isGroupchat = isGroupchat,
        )
        synchronized(lock) {
            val list = entries[conversation]
            val index = list?.let { resolveTargetIndex(it, mutation.targetId) } ?: -1
            if (list != null && index >= 0) {
                val updated = list[index].applying(ranked)
                if (updated != list[index]) {
                    list[index] = updated
                    publish(conversation, list)
                }
            } else {
                val queue = pendingMutations.getOrPut(conversation) { ArrayDeque() }
                queue.addLast(ranked)
                while (queue.size > maxPendingMutationsPerConversation) queue.removeFirst()
            }
        }
    }

    /**
     * Collision-safe target resolution (web `findMessageIndexById`
     * parity): the primary id always wins; a XEP-0359 alias resolves
     * only when exactly one row claims it — destructive mutations must
     * never land on an ambiguous alias.
     */
    private fun resolveTargetIndex(list: List<Entry>, targetId: String): Int {
        val primary = list.indexOfFirst { it.item.id == targetId }
        if (primary >= 0) return primary
        var found = -1
        list.forEachIndexed { index, entry ->
            if (targetId in entry.item.identityIds) {
                if (found >= 0) return -1
                found = index
            }
        }
        return found
    }

    /** Apply (in rank order) every parked mutation that targets [entry]. */
    private fun drainPendingMutationsInto(
        conversation: String,
        entry: Entry,
        isGroupchat: Boolean,
    ): Entry {
        val queue = pendingMutations[conversation] ?: return entry
        val matching = queue.filter { it.mutation.targetId in entry.item.identityIds }
        if (matching.isEmpty()) return entry
        queue.removeAll(matching.toSet())
        if (queue.isEmpty()) pendingMutations.remove(conversation)
        return matching
            .sortedBy { it.rank }
            .fold(entry) { acc, ranked -> acc.applying(ranked.copy(isGroupchat = isGroupchat)) }
    }

    private fun Entry.applying(ranked: RankedMutation): Entry {
        val state = mutations
        val next = when (val mutation = ranked.mutation) {
            is MessageMutation.Reaction -> {
                val existing = state.reactionsBySender[mutation.senderKey]
                if (existing != null && existing.rank > ranked.rank) {
                    state
                } else {
                    val senders = state.reactionsBySender.toMutableMap()
                    // An empty set KEEPS the sender entry (rendering
                    // nothing) so the clear retains its rank — deleting
                    // it would let an older MAM replay of the sender's
                    // earlier reaction resurrect what they cleared.
                    senders[mutation.senderKey] = SenderReactions(
                        emojis = mutation.emojis.distinct(),
                        mine = mutation.mine,
                        rank = ranked.rank,
                    )
                    state.copy(reactionsBySender = senders)
                }
            }
            is MessageMutation.Correction -> when {
                state.tombstone != null -> state
                !sameSender(mutation.from, item.from, ranked.isGroupchat) -> state
                state.correctionRank != null && state.correctionRank > ranked.rank -> state
                else -> state.copy(correctedBody = mutation.newBody, correctionRank = ranked.rank)
            }
            is MessageMutation.Retraction -> when {
                state.tombstone != null -> state
                !sameSender(mutation.from, item.from, ranked.isGroupchat) -> state
                else -> state.copy(tombstone = MessageTombstone.Retracted)
            }
            is MessageMutation.Moderation -> when {
                state.tombstone != null -> state
                // XEP-0425 authenticity: only the room service itself
                // (the bare room JID, no occupant resource) may moderate —
                // an occupant stanza claiming moderation is a spoof.
                mutation.from != item.conversationJid -> state
                else -> state.copy(
                    tombstone = MessageTombstone.Moderated(
                        moderatedBy = mutation.moderatedBy,
                        reason = mutation.reason,
                    ),
                )
            }
        }
        return if (next == state) this else copy(mutations = next)
    }

    private fun publish(conversation: String, list: List<Entry>) {
        flowFor(conversation).value = list.map { it.enriched() }
    }

    private fun Entry.enriched(): TimelineItem {
        val state = mutations
        if (state == MutationState()) return item
        // Cleared senders linger as empty entries (rank retention);
        // aggregation naturally renders them as no chips.
        return item.copy(
            body = state.correctedBody ?: item.body,
            edited = state.correctedBody != null,
            tombstone = state.tombstone,
            reactions = aggregateReactions(state.reactionsBySender),
        )
    }

    private fun flowFor(conversation: String): MutableStateFlow<List<TimelineItem>> =
        flows.getOrPut(conversation) { MutableStateFlow(emptyList()) }

    private data class Entry(
        val item: TimelineItem,
        val sortInstant: Instant?,
        val order: Long,
        val mutations: MutationState = MutationState(),
    )

    /**
     * Latest-wins ordering for mutations: by timestamp when carried
     * (archived mutations always are), with timestampless live mutations
     * ranking newest, insertion order breaking ties.
     */
    private data class Rank(val instant: Instant?, val order: Long) : Comparable<Rank> {
        override fun compareTo(other: Rank): Int {
            val byInstant = (instant ?: Instant.MAX).compareTo(other.instant ?: Instant.MAX)
            return if (byInstant != 0) byInstant else order.compareTo(other.order)
        }
    }

    private data class RankedMutation(
        val mutation: MessageMutation,
        val rank: Rank,
        val isGroupchat: Boolean,
    )

    /** One sender's complete current reaction set (XEP-0444 replace). */
    private data class SenderReactions(
        val emojis: List<String>,
        val mine: Boolean,
        val rank: Rank,
    )

    private data class MutationState(
        val reactionsBySender: Map<String, SenderReactions> = emptyMap(),
        val correctedBody: String? = null,
        val correctionRank: Rank? = null,
        val tombstone: MessageTombstone? = null,
    )

    private companion object {
        /** Per-conversation row bound; live overflow drops oldest. */
        const val MAX_ITEMS_PER_CONVERSATION = 500

        /** Per-conversation bound on mutations parked before their target. */
        const val MAX_PENDING_MUTATIONS = 200

        val ENTRY_ORDER: Comparator<Entry> =
            compareBy<Entry> { it.sortInstant ?: Instant.MAX }.thenBy { it.order }

        fun aggregateReactions(bySender: Map<String, SenderReactions>): List<ReactionGroup> {
            if (bySender.isEmpty()) return emptyList()
            // Group per emoji, ordered by the earliest contributing
            // sender's rank so chips keep first-reacted order.
            data class Accumulated(var count: Int, var mine: Boolean, var firstRank: Rank)
            val groups = LinkedHashMap<String, Accumulated>()
            bySender.values.sortedBy { it.rank }.forEach { sender ->
                sender.emojis.forEach { emoji ->
                    val group = groups.getOrPut(emoji) { Accumulated(0, false, sender.rank) }
                    group.count += 1
                    group.mine = group.mine || sender.mine
                }
            }
            return groups.entries
                .sortedBy { it.value.firstRank }
                .map { (emoji, acc) -> ReactionGroup(emoji = emoji, count = acc.count, mine = acc.mine) }
        }

        fun parseInstant(timestamp: String): Instant? =
            runCatching { Instant.parse(timestamp) }.getOrElse {
                runCatching { OffsetDateTime.parse(timestamp).toInstant() }.getOrNull()
            }
    }
}
