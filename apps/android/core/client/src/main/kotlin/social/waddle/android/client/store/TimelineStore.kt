package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.client.bareJid
import social.waddle.android.client.conversationKeyOf
import social.waddle.android.client.liveOwnNickOf
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.resourcepart
import social.waddle.android.client.stripReplyFallback
import social.waddle.android.client.withLiveAuthor
import social.waddle.client.ffi.WaddleArchivedMessage
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleSafetyScores
import java.time.Instant
import java.time.OffsetDateTime

/**
 * Per-conversation ordered message lists: live messages insert as they
 * arrive, MAM pages merge in, and duplicates (XEP-0198 replays, MAM
 * refetch of a live message) collapse on the XEP-0359 stanza id when
 * present, else origin id, else message id. Ordering is by timestamp,
 * with insertion order breaking ties; items without a timestamp sort
 * after timestamped history (live messages are the newest).
 *
 * Mutation messages (XEP-0444 reactions, XEP-0308 corrections, XEP-0424
 * retractions, XEP-0425 moderation, XEP-0422 safety-score fastenings —
 * see [MessageMutation]) never insert
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
    /** Our actual occupant nick per room (self-presence); see [liveOwnNickOf]. */
    private val actualOwnNickIn: (roomJid: String) -> String? = { null },
) {
    private val lock = Any()
    private val flows = HashMap<String, MutableStateFlow<List<TimelineItem>>>()
    private val entries = HashMap<String, MutableList<Entry>>()
    private val pendingMutations = HashMap<String, ArrayDeque<RankedMutation>>()

    /** Newest wire timestamp seen per conversation (the rank anchor). */
    private val newestWireInstant = HashMap<String, Instant>()
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

    /** Called only after the manager correlates the error with its outbound send. */
    fun rejectOutbound(conversationJid: String, stanzaId: String) {
        synchronized(lock) {
            val conversation = bareJid(conversationJid)
            val list = entries[conversation] ?: return
            for (index in list.indices) {
                val entry = list[index]
                if (entry.item.isMine && stanzaId in entry.item.identityIds) {
                    list[index] = entry.copy(item = entry.item.copy(rejected = true))
                }
            }
            publish(conversation, list)
        }
    }

    /**
     * Returns true only when the message was a genuinely NEW timeline
     * row — XEP-0198 replays, live/archive twins, and mutation messages
     * all return false so callers (the unread counter) don't count what
     * the timeline itself never renders as new content.
     */
    fun onLiveMessage(message: WaddleMessage, authorJid: String? = null): Boolean {
        // `muc#user` traffic is never shown (see onArchivedMessage).
        if (message.mucUser) return false
        val isGroupchat = message.isMuc || message.messageType == "groupchat"
        // The avatar identity of this row, fixed at ingest: the disclosed
        // occupant JID, else — for OUR undelayed reflection only, proven
        // by the room's own self-presence — the account. The configured
        // nick alone never earns our face (see [verifiedOwnReflection]).
        val stamp = authorJid ?: ownBareJid?.let(::normalizedBareJid)?.takeIf {
            isGroupchat && verifiedOwnReflection(message)
        }
        val key = conversationKeyOf(
            ownBareJid = ownBareJid,
            ownNick = if (isGroupchat) liveOwnNickOf(message.from, ownNick, actualOwnNickIn) else ownNick,
            from = message.from,
            to = message.to,
            isGroupchat = isGroupchat,
        )?.withLiveAuthor(isGroupchat, authorJid, ownBareJid) ?: return false
        mutationOf(message, isGroupchat = isGroupchat, mine = key.isMine)?.let { mutation ->
            applyMutation(key.jid, mutation, isGroupchat, timestamp = message.timestamp)
            return false
        }
        // Call anchors (`urn:waddle:call-thread:0`) are rendered as a
        // dedicated call row even when the server enriches a bodyless
        // stanza — drop only rows with neither body nor call payload.
        val body = message.body
            ?: if (message.callThread != null || message.callThreadEnded != null) "" else return false
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
                authorJid = stamp,
            ),
            isGroupchat = isGroupchat,
            initialTombstone = null,
        )
    }

    /**
     * An undelayed room message from the nick the room's self-presence
     * (XEP-0045 110/210) says is ours right now. Without that presence
     * (e.g. right after a fresh session) the configured nick proves
     * nothing: someone else may hold it.
     */
    private fun verifiedOwnReflection(message: WaddleMessage): Boolean {
        if (message.timestamp != null) return false
        val from = message.from ?: return false
        val nick = resourcepart(from) ?: return false
        return actualOwnNickIn(bareJid(from)) == nick
    }

    fun onArchivedMessage(message: WaddleArchivedMessage) {
        // XEP-0045 `muc#user` rows (room PMs, invites) are never shown: no
        // native surface answers them privately, and in the room they
        // would read as public. History pages reach this store directly.
        if (message.mucUser) return
        val isGroupchat = message.messageType == "groupchat"
        val key = conversationKeyOf(
            ownBareJid = ownBareJid,
            ownNick = ownNick,
            from = message.from,
            to = message.to,
            isGroupchat = isGroupchat,
        ) ?: return
        val authorJid = message.authorRealJid?.let(::normalizedBareJid)?.takeIf { '@' in it }
        // A room row the archive attributes is ours only if that real JID
        // is — our nick may have been someone else's when it was written.
        // Nick equality decides only rows without one.
        val isMine = if (isGroupchat && authorJid != null) {
            authorJid == ownBareJid?.let(::normalizedBareJid)
        } else {
            key.isMine
        }
        mutationOf(message, isGroupchat = isGroupchat, mine = isMine)?.let { mutation ->
            applyMutation(key.jid, mutation, isGroupchat, timestamp = message.timestamp)
            return
        }
        // Same bodyless-call-anchor exception as the live path.
        val body = message.body
            ?: if (message.callThread != null || message.callThreadEnded != null) "" else return
        insert(
            conversation = key.jid,
            item = TimelineItem(
                id = message.stanzaId ?: message.originId ?: message.id ?: message.mamId,
                conversationJid = key.jid,
                from = message.from,
                body = stripReplyFallback(body, message.replyFallbackStart, message.replyFallbackEnd),
                timestamp = message.timestamp,
                isMine = isMine,
                source = TimelineSource.Archived(message),
                authorJid = authorJid,
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
            newestWireInstant.clear()
            insertionCounter = 0L
            flows.values.forEach { it.value = emptyList() }
        }
    }

    /** Must hold [lock]. */
    private fun recordWireInstant(conversation: String, timestamp: String?) {
        val instant = timestamp?.let(::parseInstant) ?: return
        val current = newestWireInstant[conversation]
        if (current == null || instant > current) newestWireInstant[conversation] = instant
    }

    private fun insert(
        conversation: String,
        item: TimelineItem,
        isGroupchat: Boolean,
        initialTombstone: MessageTombstone?,
    ): Boolean {
        synchronized(lock) {
            val list = entries.getOrPut(conversation) { mutableListOf() }
            // Dedupe on the collapsed primary id OR — for the SAME
            // sender only — a shared XEP-0359 identity: the MAM copy of
            // an own DM echo keys on the server-assigned stanza id while
            // the echo keys on the client origin id, overlapping only
            // through the origin id. Cross-sender id collisions are NOT
            // merged (dropping a message because another sender reused
            // an id would be an injection vector); they stay as distinct
            // rows that the ambiguous-alias guard refuses to mutate.
            val incomingUnique = uniqueWireIds(item)
            val incomingSender = senderKeyOf(item.from, isGroupchat)
            // Sender continuity gates BOTH disjuncts: a different sender
            // reusing a primary id must not suppress or overwrite the
            // original row (cross-sender collisions stay distinct and
            // un-mutatable via the ambiguous-alias guard).
            val existingIndex = list.indexOfFirst { entry ->
                incomingSender != null &&
                    senderKeyOf(entry.item.from, isGroupchat) == incomingSender &&
                    (entry.item.id == item.id || uniqueWireIds(entry.item).any { it in incomingUnique })
            }
            if (existingIndex >= 0) {
                val existing = list[existingIndex]
                mergedTwin(existing, item)?.let { merged ->
                    list[existingIndex] = merged
                    list.sortWith(ENTRY_ORDER)
                    publish(conversation, list)
                }
                recordWireInstant(conversation, item.timestamp)
                return false
            }
            var entry = Entry(
                item = item,
                sortInstant = item.timestamp?.let { parseInstant(it) },
                order = insertionCounter++,
                mutations = MutationState(tombstone = initialTombstone),
            )
            recordWireInstant(conversation, item.timestamp)
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

    /**
     * The same message seen twice (live + archive, or a replay): the
     * updated entry, or `null` when [existing] already has everything.
     * A live record supersedes its archived twin (richer payload);
     * otherwise the first record wins. Applied mutations live on the
     * entry and survive either way.
     */
    private fun mergedTwin(existing: Entry, item: TimelineItem): Entry? {
        if (item.source is TimelineSource.Live && existing.item.source is TimelineSource.Archived) {
            val merged = item.copy(
                timestamp = item.timestamp ?: existing.item.timestamp,
                rejected = existing.item.rejected,
                // A stamp is never replaced by a later copy's, and a
                // missing one is filled only from a room-vouched archive
                // twin — never from this live copy.
                authorJid = existing.item.authorJid ?: vouchedStamp(item, existing.item),
            )
            // The sort key must follow the adopted timestamp or the row
            // keeps its stale placement forever.
            return existing.copy(
                item = merged,
                sortInstant = merged.timestamp?.let(::parseInstant) ?: existing.sortInstant,
            )
        }
        // The archived copy of a timestampless local echo brings the
        // server timestamp; adopt it in place — including the sort key,
        // else the echo stays pinned at the newest edge above
        // later-arriving messages. It also attributes a row that arrived
        // unstamped (delayed, or no occupant presence yet) — but only
        // from its room-vouched archive twin.
        val adoptedTimestamp = item.timestamp?.takeIf { existing.item.timestamp == null }
        val adoptedAuthor = if (existing.item.authorJid == null) vouchedStamp(item, existing.item) else null
        if (adoptedTimestamp == null && adoptedAuthor == null) return null
        return existing.copy(
            item = existing.item.copy(
                timestamp = adoptedTimestamp ?: existing.item.timestamp,
                authorJid = adoptedAuthor ?: existing.item.authorJid,
            ),
            sortInstant = adoptedTimestamp?.let(::parseInstant) ?: existing.sortInstant,
        )
    }

    /**
     * The author stamp [existing] may take from its twin [incoming]: only
     * an ARCHIVE copy carrying the row's own room-assigned XEP-0359
     * stanza id. Rows also merge on the sender-controlled origin id,
     * which a later holder of the nick could reuse to lend the row their
     * identity; a live copy's stamp is just whoever holds the nick now.
     */
    private fun vouchedStamp(incoming: TimelineItem, existing: TimelineItem): String? {
        if (incoming.source !is TimelineSource.Archived) return null
        val room = existing.conversationJid
        val roomId = existing.assignedStanzaId(room)?.id ?: return null
        return incoming.authorJid.takeIf { incoming.assignedStanzaId(room)?.id == roomId }
    }

    private fun applyMutation(
        conversation: String,
        mutation: MessageMutation,
        isGroupchat: Boolean,
        timestamp: String?,
    ) {
        synchronized(lock) {
            val instant = timestamp?.let { parseInstant(it) }
            // Web `appliedAfterWire` anchor parity: a live (unstamped)
            // mutation ranks AT the newest wire instant this conversation
            // has seen, with insertion order breaking ties — an archived
            // replay stamped LATER than everything seen so far still
            // wins (an Instant.MAX rank would reject genuinely newer
            // mutations replayed via MAM after a stream drop, freezing
            // stale reactions/bodies forever).
            recordWireInstant(conversation, timestamp)
            val ranked = RankedMutation(
                mutation = mutation,
                rank = Rank(
                    instant = instant ?: newestWireInstant[conversation],
                    order = insertionCounter++,
                ),
                isGroupchat = isGroupchat,
            )
            val list = entries[conversation]
            val index = list?.let { resolveTargetIndex(it, mutation.targetId, mutation, isGroupchat) } ?: -1
            if (list != null && index >= 0) {
                if (mutation is MessageMutation.SafetyScores && !scoreRevisionMatches(list[index], mutation)) {
                    val queue = pendingMutations.getOrPut(conversation) { ArrayDeque() }
                    queue.addLast(ranked)
                    while (queue.size > maxPendingMutationsPerConversation) queue.removeFirst()
                    return
                }
                var updated = list[index].applying(ranked)
                if (mutation is MessageMutation.Correction && updated != list[index]) {
                    updated = drainPendingMutationsInto(conversation, updated, isGroupchat)
                }
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
     * never land on an ambiguous alias. Sender-authorized mutations
     * (corrections/retractions) pre-filter candidates to the mutating
     * sender's own rows (web sender-predicate parity): another sender's
     * colliding row must neither receive the mutation nor make the
     * author's legitimate target ambiguous.
     */
    private fun resolveTargetIndex(
        list: List<Entry>,
        targetId: String,
        mutation: MessageMutation,
        isGroupchat: Boolean,
    ): Int {
        if (mutation is MessageMutation.SafetyScores) {
            val matches = list.indices.filter { safetyScoreTargets(list[it].item, mutation) }
            return matches.singleOrNull() ?: -1
        }
        val senderScoped = mutation is MessageMutation.Correction ||
            mutation is MessageMutation.Retraction
        fun eligible(entry: Entry): Boolean =
            !senderScoped || sameSender(mutation.from, entry.item.from, isGroupchat)
        // Cross-sender collisions can leave several rows sharing a
        // primary id; a mutation may only land when exactly one claims it.
        var primary = -1
        list.forEachIndexed { index, entry ->
            if (entry.item.id == targetId && eligible(entry)) {
                if (primary >= 0) return -1
                primary = index
            }
        }
        if (primary >= 0) return primary
        var found = -1
        list.forEachIndexed { index, entry ->
            if (targetId in entry.item.identityIds && eligible(entry)) {
                if (found >= 0) return -1
                found = index
            }
        }
        return found
    }

    private fun safetyScoreTargets(item: TimelineItem, mutation: MessageMutation.SafetyScores): Boolean {
        val fastening = mutation.fastening
        val roomId = item.assignedStanzaId(item.conversationJid)
        return item.conversationJid.equals(fastening.targetStanzaBy, ignoreCase = true) &&
            roomId?.id == fastening.targetStanzaId &&
            item.originId == fastening.targetOriginId
    }

    private fun scoreRevisionMatches(entry: Entry, mutation: MessageMutation.SafetyScores): Boolean =
        (entry.mutations.correctionRevisionId ?: entry.item.assignedStanzaId(entry.item.conversationJid)?.id) ==
            mutation.fastening.sourceRevisionId

    /** Apply (in rank order) every parked mutation that targets [entry]. */
    private fun drainPendingMutationsInto(
        conversation: String,
        entry: Entry,
        isGroupchat: Boolean,
    ): Entry {
        val queue = pendingMutations[conversation] ?: return entry
        val matching = queue.filter {
            val mutation = it.mutation
            if (mutation is MessageMutation.SafetyScores) {
                safetyScoreTargets(entry.item, mutation) && scoreRevisionMatches(entry, mutation)
            } else {
                mutation.targetId in entry.item.identityIds
            }
        }
        if (matching.isEmpty()) return entry
        queue.removeAll(matching.toSet())
        if (queue.isEmpty()) pendingMutations.remove(conversation)
        val applied = matching
            .sortedBy { it.rank }
            .fold(entry) { acc, ranked -> acc.applying(ranked.copy(isGroupchat = isGroupchat)) }
        return if (pendingMutations[conversation] == null) {
            applied
        } else {
            drainPendingMutationsInto(conversation, applied, isGroupchat)
        }
    }

    private fun Entry.applying(ranked: RankedMutation): Entry {
        val next = when (val mutation = ranked.mutation) {
            is MessageMutation.Reaction -> mutations.applyingReaction(mutation, ranked.rank)
            is MessageMutation.Correction -> mutations.applyingCorrection(mutation, ranked, item)
            is MessageMutation.Retraction -> mutations.applyingRetraction(mutation, ranked, item)
            is MessageMutation.Moderation -> mutations.applyingModeration(mutation, item)
            is MessageMutation.SafetyScores -> mutations.applyingSafetyScores(mutation, ranked.rank, item)
        }
        return if (next == mutations) this else copy(mutations = next)
    }

    private fun MutationState.applyingReaction(
        mutation: MessageMutation.Reaction,
        rank: Rank,
    ): MutationState {
        val existing = reactionsBySender[mutation.senderKey]
        if (existing != null && existing.rank > rank) return this
        val senders = reactionsBySender.toMutableMap()
        // An empty set KEEPS the sender entry (rendering nothing) so the
        // clear retains its rank — deleting it would let an older MAM
        // replay of the sender's earlier reaction resurrect what they
        // cleared.
        senders[mutation.senderKey] = SenderReactions(
            emojis = mutation.emojis.distinct(),
            mine = mutation.mine,
            rank = rank,
        )
        return copy(reactionsBySender = senders)
    }

    private fun MutationState.applyingCorrection(
        mutation: MessageMutation.Correction,
        ranked: RankedMutation,
        item: TimelineItem,
    ): MutationState = when {
        tombstone != null -> this
        !sameSender(mutation.from, item.from, ranked.isGroupchat) -> this
        correctionRank != null && correctionRank > ranked.rank -> this
        else -> copy(
            correctedBody = mutation.newBody,
            correctionRank = ranked.rank,
            correctionRevisionId = mutation.sourceRevisionId ?: "",
            safetyScores =
                if (safetyScoresRevisionId == mutation.sourceRevisionId) safetyScores else null,
            safetyScoresRank =
                if (safetyScoresRevisionId == mutation.sourceRevisionId) safetyScoresRank else null,
            safetyScoresRevisionId =
                if (safetyScoresRevisionId == mutation.sourceRevisionId) safetyScoresRevisionId else null,
        )
    }

    private fun MutationState.applyingRetraction(
        mutation: MessageMutation.Retraction,
        ranked: RankedMutation,
        item: TimelineItem,
    ): MutationState = when {
        tombstone != null -> this
        !sameSender(mutation.from, item.from, ranked.isGroupchat) -> this
        else -> copy(tombstone = MessageTombstone.Retracted)
    }

    private fun MutationState.applyingModeration(
        mutation: MessageMutation.Moderation,
        item: TimelineItem,
    ): MutationState = when {
        tombstone != null -> this
        // XEP-0425 authenticity: only the room service itself (the bare
        // room JID, no occupant resource) may moderate — an occupant
        // stanza claiming moderation is a spoof.
        mutation.from != item.conversationJid -> this
        else -> copy(
            tombstone = MessageTombstone.Moderated(
                moderatedBy = mutation.moderatedBy,
                reason = mutation.reason,
            ),
        )
    }

    private fun MutationState.applyingSafetyScores(
        mutation: MessageMutation.SafetyScores,
        rank: Rank,
        item: TimelineItem,
    ): MutationState = when {
        // Same authenticity rule as XEP-0425: only the room service
        // itself (bare room JID, no occupant resource) scores messages.
        mutation.from != item.conversationJid -> this
        !safetyScoreTargets(item, mutation) -> this
        tombstone != null -> this
        (correctionRevisionId ?: item.assignedStanzaId(item.conversationJid)?.id) !=
            mutation.fastening.sourceRevisionId -> this
        // A re-delivery (MAM re-page, reconnect catch-up) of a fastening
        // already applied is history, never an update — even when its
        // stamp ties the anchor of a later live replace or clear.
        mutation.fasteningIds.any { it in appliedSafetyFastenings } -> this
        // XEP-0422 replace is latest-wins; a clear keeps its rank so an
        // older MAM replay cannot resurrect scores it removed.
        safetyScoresRank != null && safetyScoresRank > rank -> this
        else -> copy(
            safetyScores = mutation.fastening.scores,
            safetyScoresRank = rank,
            safetyScoresRevisionId = mutation.fastening.sourceRevisionId,
            appliedSafetyFastenings = appliedSafetyFastenings + mutation.fasteningIds,
        )
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
            safetyScores = if (state.tombstone == null) state.safetyScores else null,
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
     * (archived mutations always are); live mutations are anchored to
     * the newest wire instant seen at apply time (see [applyMutation]),
     * so a null instant only means "nothing wire-stamped seen yet" and
     * sorts oldest. Insertion order breaks ties (a live apply outranks
     * the wire stamp it was anchored to).
     */
    private data class Rank(val instant: Instant?, val order: Long) : Comparable<Rank> {
        override fun compareTo(other: Rank): Int {
            val byInstant = (instant ?: Instant.MIN).compareTo(other.instant ?: Instant.MIN)
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
        val correctionRevisionId: String? = null,
        val tombstone: MessageTombstone? = null,
        val safetyScores: WaddleSafetyScores? = null,
        val safetyScoresRank: Rank? = null,
        val safetyScoresRevisionId: String? = null,
        /** Wire ids of every safety-score fastening applied to the row. */
        val appliedSafetyFastenings: Set<String> = emptySet(),
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

        /** XEP-0359 ids only — unique by construction, unlike `@id`. */
        fun uniqueWireIds(item: TimelineItem): Set<String> =
            setOfNotNull(item.stanzaId, item.originId)

        /** Occupant JID in MUCs (bare = the room), bare JID in 1:1. */
        fun senderKeyOf(from: String?, isGroupchat: Boolean): String? =
            from?.let { if (isGroupchat) it else bareJid(it) }

        fun parseInstant(timestamp: String): Instant? =
            runCatching { Instant.parse(timestamp) }.getOrElse {
                runCatching { OffsetDateTime.parse(timestamp).toInstant() }.getOrNull()
            }
    }
}
