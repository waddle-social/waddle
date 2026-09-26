import Foundation

/// What ingesting one stanza did to the timeline.
public enum TimelineIngestResult: Equatable, Sendable {
    /// A new content row was inserted.
    case inserted(TimelineItem)
    /// The stanza duplicated an existing row (XEP-0198 replay, MAM refetch,
    /// MUC reflection of a local echo).
    case duplicate
    /// The stanza mutated (or was parked to mutate) another row.
    case mutation
    /// The stanza carries nothing the timeline renders.
    case ignored
}

/// Per-conversation ordered timelines.
///
/// - Rows dedupe on the primary id, or on a shared XEP-0359 id from the
///   same sender. Cross-sender collisions stay distinct rows; accepting
///   them would let one sender suppress another's message.
/// - Rows sort by wire timestamp, then insertion order. Live stanzas carry
///   no timestamp and sort after all timestamped history.
/// - Mutations apply latest-wins per kind. A mutation's rank is its
///   timestamp; a live mutation ranks at the newest wire timestamp seen in
///   the conversation, with insertion order breaking ties. Mutations that
///   arrive before their target (backwards MAM paging loads a reaction
///   before its message) are parked, bounded, and applied on insert.
/// - Only live inserts trim to `maxItemsPerConversation`, from the oldest
///   end; explicitly requested history is never evicted as it merges. A
///   trim that drops archived rows reports the oldest archived row left, so
///   paging can refetch what was dropped instead of skipping past it.
@MainActor
public final class TimelineStore {
    public var account: AccountIdentity?
    /// Called after a live insert trimmed archived rows, with the MAM id of
    /// the oldest archived row still loaded, or nil when none is. (A live
    /// row's stanza-id is no cursor: live rows sort after all archived
    /// ones, so one can be older than archived rows that were trimmed.)
    var onArchiveTrimmed: (@MainActor (ConversationID, String?) -> Void)?

    private let maxItemsPerConversation: Int
    private let maxPendingMutations: Int
    private var timelines: [ConversationID: ConversationTimeline] = [:]
    private var entries: [ConversationID: [Entry]] = [:]
    private var parked: [ConversationID: [RankedMutation]] = [:]
    private var newestWireDate: [ConversationID: Date] = [:]
    private var insertionCounter: Int64 = 0

    /// Rows a conversation keeps once live inserts start trimming.
    public var capacity: Int { maxItemsPerConversation }

    public init(maxItemsPerConversation: Int = 500, maxPendingMutations: Int = 200) {
        self.maxItemsPerConversation = maxItemsPerConversation
        self.maxPendingMutations = maxPendingMutations
    }

    /// The observable timeline for `conversation`, created on first use.
    public func timeline(for conversation: ConversationID) -> ConversationTimeline {
        if let existing = timelines[conversation] {
            return existing
        }
        let created = ConversationTimeline(conversation: conversation)
        timelines[conversation] = created
        return created
    }

    /// Routes and ingests a live or archived stanza.
    @discardableResult
    public func ingest(_ message: WireMessage, receivedAt: Date = Date()) -> TimelineIngestResult {
        guard let account,
              let route = account.route(from: message.from, to: message.to, isGroupchat: message.isGroupchat)
        else { return .ignored }
        return ingest(message, route: route, receivedAt: receivedAt)
    }

    /// Ingests a stanza whose route is already known.
    @discardableResult
    public func ingest(_ message: WireMessage, route: MessageRoute, receivedAt: Date = Date()) -> TimelineIngestResult {
        let conversation = route.conversation
        if let mutation = MessageMutation.of(message, isMine: route.isMine) {
            apply(mutation, in: conversation, timestamp: message.timestamp)
            return .mutation
        }
        // An archived XEP-0424 tombstone may carry no body; it still takes
        // (or marks) its row so history shows the deletion.
        guard let body = message.body ?? (message.isRetracted ? "" : nil),
              let id = primaryID(of: message, in: conversation)
        else {
            return .ignored
        }
        let item = TimelineItem(
            id: id,
            conversation: conversation,
            isMine: route.isMine,
            message: message,
            body: ReplyFallback.strip(body, range: message.reply?.fallback),
            receivedAt: receivedAt
        )
        let tombstone: Tombstone? = message.isRetracted ? .retracted : nil
        return insert(item, tombstone: tombstone, isLocalEcho: false)
    }

    /// Inserts the optimistic row for an own send. The id must be the
    /// client stanza id the send stamps as both `@id` and XEP-0359
    /// `<origin-id/>`, so the reflection or archive copy supersedes it.
    public func insertLocalEcho(_ message: WireMessage, in conversation: ConversationID, receivedAt: Date = Date()) {
        guard let body = message.body, let id = message.identity.originID else { return }
        let item = TimelineItem(
            id: id,
            conversation: conversation,
            isMine: true,
            message: message,
            body: ReplyFallback.strip(body, range: message.reply?.fallback),
            isLocalEcho: true,
            receivedAt: receivedAt
        )
        _ = insert(item, tombstone: nil, isLocalEcho: true)
    }

    /// Removes a local echo that never went out (a failed send the user
    /// discards or retries under a new id).
    public func removeLocalEcho(id: String, in conversation: ConversationID) {
        guard var list = entries[conversation],
              let index = list.firstIndex(where: { $0.isLocalEcho && $0.item.id == id })
        else { return }
        list.remove(at: index)
        entries[conversation] = list
        publish(conversation)
    }

    /// Applies one of the account's own mutations optimistically. A 1:1
    /// mutation is never reflected back to the sending client; a room
    /// reflection re-applies idempotently. Same checks as wire mutations.
    public func applyLocalMutation(_ mutation: MessageMutation, in conversation: ConversationID) {
        apply(mutation, in: conversation, timestamp: nil)
    }

    public func clear() {
        entries.removeAll()
        parked.removeAll()
        newestWireDate.removeAll()
        insertionCounter = 0
        for timeline in timelines.values {
            timeline.publish([])
        }
    }

    // MARK: - Insert

    /// A room row is keyed by the room-assigned stanza id; the core's
    /// primary stanza id is merely the first `<stanza-id/>` in the stanza,
    /// which an occupant can inject.
    private func primaryID(of message: WireMessage, in conversation: ConversationID) -> String? {
        if conversation.isRoom, let roomID = message.identity.stanzaID(assignedBy: conversation.jid) {
            return roomID
        }
        let identity = message.identity
        if conversation.isRoom {
            // Never a stanza id another authority assigned: an occupant can
            // put any id there. An archive row falls back to its MAM id.
            if let authored = identity.originID ?? identity.messageID {
                return authored
            }
            if case let .archive(mamID) = message.source {
                return mamID
            }
            return nil
        }
        if let primary = identity.primary {
            return primary
        }
        if case let .archive(mamID) = message.source {
            return mamID
        }
        return nil
    }

    private func insert(_ item: TimelineItem, tombstone: Tombstone?, isLocalEcho: Bool) -> TimelineIngestResult {
        let conversation = item.conversation
        var list = entries[conversation] ?? []
        let isGroupchat = conversation.isRoom
        let incomingUnique = dedupeIDs(of: item)
        let incomingSender = senderKey(item.from, isGroupchat: isGroupchat)

        if let incomingSender,
           let index = list.firstIndex(where: { entry in
               senderKey(entry.item.from, isGroupchat: isGroupchat) == incomingSender
                   && (entry.item.id == item.id || !dedupeIDs(of: entry.item).isDisjoint(with: incomingUnique))
           }) {
            let existing = list[index]
            recordWireDate(item.timestamp, in: conversation)
            var updated = superseding(existing, with: item, isLocalEcho: isLocalEcho)
            if let tombstone, (updated ?? existing).mutations.tombstone == nil {
                // An archive tombstone for a row loaded before it was retracted.
                var marked = updated ?? existing
                marked.mutations.tombstone = tombstone
                updated = marked
            }
            if let replaced = updated {
                list[index] = replaced
                list.sort(by: Entry.precedes)
                entries[conversation] = list
                publish(conversation)
            }
            return .duplicate
        }

        recordWireDate(item.timestamp, in: conversation)
        var entry = Entry(
            item: item,
            sortDate: item.timestamp,
            order: nextOrder(),
            mutations: MutationState(tombstone: tombstone),
            isLocalEcho: isLocalEcho,
            isArchived: item.message.source != .live
        )
        entry = drainParked(into: entry, conversation: conversation)
        list.append(entry)
        list.sort(by: Entry.precedes)
        var trimmedArchive = false
        if !entry.isArchived {
            let overflow = list.count - maxItemsPerConversation
            if overflow > 0 {
                trimmedArchive = list.prefix(overflow).contains(where: \.isArchived)
                list.removeFirst(overflow)
            }
        }
        entries[conversation] = list
        publish(conversation)
        if trimmedArchive {
            onArchiveTrimmed?(conversation, list.lazy.compactMap(\.archiveID).first)
        }
        return .inserted(enriched(entry))
    }

    /// Decides whether a duplicate replaces the stored row. Returns nil to
    /// keep the stored row unchanged. Applied mutations survive every swap.
    private func superseding(_ existing: Entry, with incoming: TimelineItem, isLocalEcho: Bool) -> Entry? {
        if isLocalEcho {
            return nil
        }
        let incomingIsLive = incoming.message.source == .live
        // A wire copy replaces the local echo: it carries the room-assigned
        // stanza id that reactions, replies and pins need.
        // A live copy replaces its archived twin: the live payload is richer.
        if existing.isLocalEcho || (incomingIsLive && existing.isArchived) {
            var replaced = existing
            let timestamp = incoming.timestamp ?? existing.item.timestamp
            var message = incoming.message
            message.timestamp = timestamp
            replaced.item = TimelineItem(
                id: existing.item.id,
                conversation: incoming.conversation,
                isMine: incoming.isMine,
                message: message,
                body: incoming.body,
                receivedAt: existing.item.receivedAt
            )
            replaced.sortDate = timestamp
            replaced.isLocalEcho = false
            replaced.isArchived = !incomingIsLive && existing.isArchived
            return replaced
        }
        // The archive copy of a timestampless live row brings the server
        // timestamp; adopt it so the row sorts correctly.
        if existing.item.timestamp == nil, let timestamp = incoming.timestamp {
            var replaced = existing
            replaced.item.message.timestamp = timestamp
            replaced.sortDate = timestamp
            return replaced
        }
        return nil
    }

    // MARK: - Mutations

    private func apply(_ mutation: MessageMutation, in conversation: ConversationID, timestamp: Date?) {
        recordWireDate(timestamp, in: conversation)
        let ranked = RankedMutation(
            mutation: mutation,
            rank: Rank(date: timestamp ?? newestWireDate[conversation], order: nextOrder())
        )
        guard var list = entries[conversation],
              let index = resolveTarget(in: list, mutation: mutation, isGroupchat: conversation.isRoom)
        else {
            var queue = parked[conversation] ?? []
            queue.append(ranked)
            if queue.count > maxPendingMutations {
                queue.removeFirst(queue.count - maxPendingMutations)
            }
            parked[conversation] = queue
            return
        }
        if !mutationReady(list[index], mutation) {
            var queue = parked[conversation] ?? []
            queue.append(ranked)
            if queue.count > maxPendingMutations {
                queue.removeFirst(queue.count - maxPendingMutations)
            }
            parked[conversation] = queue
            return
        }
        var updated = list[index].applying(ranked, conversation: conversation)
        guard updated != list[index] else { return }
        if case .correction = mutation {
            updated = drainParked(into: updated, conversation: conversation)
        }
        list[index] = updated
        entries[conversation] = list
        publish(conversation)
    }

    /// Room rows resolve only through the room-assigned stanza id, except
    /// XEP-0308 corrections, which name the author's own `@id`. In 1:1 the
    /// primary id wins and an alias resolves only when exactly one row
    /// claims it. Author-scoped mutations only consider the author's rows,
    /// so a colliding row from someone else neither receives the mutation
    /// nor makes the real target ambiguous.
    private func resolveTarget(in list: [Entry], mutation: MessageMutation, isGroupchat: Bool) -> Int? {
        if isGroupchat {
            let matches = list.indices.filter { mutationTargets(list[$0].item, mutation) }
            return matches.count == 1 ? matches[0] : nil
        }
        let primary = list.indices.filter { list[$0].item.id == mutation.targetID && isEligible(list[$0].item, for: mutation) }
        if primary.count == 1 { return primary[0] }
        if primary.count > 1 { return nil }
        let aliases = list.indices.filter { mutationTargets(list[$0].item, mutation) }
        return aliases.count == 1 ? aliases[0] : nil
    }

    private func isEligible(_ item: TimelineItem, for mutation: MessageMutation) -> Bool {
        !mutation.isSenderScoped || isSameAuthor(mutation.from, item.from, isGroupchat: item.conversation.isRoom)
    }

    /// Whether `mutation` points at `item` under the id rules above.
    private func mutationTargets(_ item: TimelineItem, _ mutation: MessageMutation) -> Bool {
        guard isEligible(item, for: mutation) else { return false }
        let target = mutation.targetID
        if item.conversation.isRoom {
            if case .correction = mutation {
                return item.identity.messageID == target || item.identity.originID == target
            }
            if case let .safetyScores(_, _, fastening) = mutation {
                return item.roomStanzaID == fastening.targetStanzaID
                    && item.identity.originID == fastening.targetOriginID
                    && item.conversation.jid == fastening.targetStanzaBy
            }
            return item.roomStanzaID == target
        }
        return item.id == target || item.identity.all.contains(target)
    }

    private func mutationReady(_ entry: Entry, _ mutation: MessageMutation) -> Bool {
        guard mutationTargets(entry.item, mutation) else { return false }
        if case let .safetyScores(_, _, fastening) = mutation {
            let revision = entry.mutations.correction?.sourceRevisionID ?? entry.item.roomStanzaID
            return revision == fastening.sourceRevisionID
        }
        return true
    }

    /// Ids that identify the same stanza from the same sender: XEP-0359
    /// ids, with the room-assigned one standing in for the stanza id in
    /// rooms (an injected foreign stanza id must not merge rows).
    private func dedupeIDs(of item: TimelineItem) -> Set<String> {
        guard item.conversation.isRoom else { return item.identity.uniqueWireIDs }
        var ids = Set<String>()
        if let roomID = item.roomStanzaID { ids.insert(roomID) }
        if let originID = item.identity.originID { ids.insert(originID) }
        return ids
    }

    private func drainParked(into entry: Entry, conversation: ConversationID) -> Entry {
        guard var queue = parked[conversation] else { return entry }
        let matching = queue.filter { mutationReady(entry, $0.mutation) }
        guard !matching.isEmpty else { return entry }
        queue.removeAll { mutationReady(entry, $0.mutation) }
        parked[conversation] = queue.isEmpty ? nil : queue
        let applied = matching
            .sorted { $0.rank < $1.rank }
            .reduce(entry) { $0.applying($1, conversation: conversation) }
        return parked[conversation] == nil ? applied : drainParked(into: applied, conversation: conversation)
    }

    // MARK: - Publishing

    private func publish(_ conversation: ConversationID) {
        timeline(for: conversation).publish((entries[conversation] ?? []).map(enriched))
    }

    private func enriched(_ entry: Entry) -> TimelineItem {
        var item = entry.item
        let state = entry.mutations
        item.isLocalEcho = entry.isLocalEcho
        if let correction = state.correction {
            item.body = correction.body
            item.isEdited = true
            item.message.markupSpans = correction.markupSpans
            item.message.references = correction.references
            // Offsets now follow the correction's wire body, whose fallback
            // (if any) replaces the original's.
            item.message.reply = item.message.reply.map {
                WireMessage.ReplyTarget(id: $0.id, author: $0.author, fallback: correction.replyFallback)
            }
            if !correction.sharedFiles.isEmpty {
                item.message.sharedFiles = correction.sharedFiles
            }
        }
        item.tombstone = state.tombstone
        item.reactions = aggregate(state.reactionsBySender, isGroupchat: item.conversation.isRoom)
        // Scores annotate content; a removed message shows none.
        item.safetyScores = state.tombstone == nil ? state.safetyScores : nil
        return item
    }

    private func aggregate(_ bySender: [String: SenderReactions], isGroupchat: Bool) -> [ReactionGroup] {
        struct Accumulated {
            var count = 0
            var includesMine = false
            var reactors: [String] = []
            var firstRank: Rank
        }
        var groups: [String: Accumulated] = [:]
        var order: [String] = []
        for sender in bySender.values.sorted(by: { $0.rank < $1.rank }) {
            for emoji in sender.emojis {
                if groups[emoji] == nil {
                    groups[emoji] = Accumulated(firstRank: sender.rank)
                    order.append(emoji)
                }
                groups[emoji]!.count += 1
                groups[emoji]!.includesMine = groups[emoji]!.includesMine || sender.isMine
                groups[emoji]!.reactors.append(sender.displayName)
            }
        }
        return order.compactMap { emoji in
            guard let group = groups[emoji], group.count > 0 else { return nil }
            return ReactionGroup(
                emoji: emoji,
                count: group.count,
                includesMine: group.includesMine,
                reactors: group.reactors
            )
        }
    }

    // MARK: - Helpers

    private func nextOrder() -> Int64 {
        insertionCounter += 1
        return insertionCounter
    }

    private func recordWireDate(_ date: Date?, in conversation: ConversationID) {
        guard let date else { return }
        if let current = newestWireDate[conversation], current >= date { return }
        newestWireDate[conversation] = date
    }

    private func senderKey(_ from: JID?, isGroupchat: Bool) -> String? {
        guard let from else { return nil }
        return isGroupchat ? from.description : from.bare.description
    }
}

// MARK: - Entry state

private struct Rank: Comparable, Hashable {
    let date: Date?
    let order: Int64

    static func < (lhs: Rank, rhs: Rank) -> Bool {
        let left = lhs.date ?? .distantPast
        let right = rhs.date ?? .distantPast
        if left != right { return left < right }
        return lhs.order < rhs.order
    }
}

private struct RankedMutation {
    let mutation: MessageMutation
    let rank: Rank
}

private struct SenderReactions: Hashable {
    let emojis: [String]
    let isMine: Bool
    let displayName: String
    let rank: Rank
}

private struct MutationState: Hashable {
    var reactionsBySender: [String: SenderReactions] = [:]
    var correction: CorrectedContent?
    var correctionRank: Rank?
    var tombstone: Tombstone?
    var safetyScores: SafetyScores?
    var safetyScoresRank: Rank?
    var safetyScoresRevisionID: String?
}

private struct Entry: Hashable {
    var item: TimelineItem
    var sortDate: Date?
    let order: Int64
    var mutations: MutationState
    var isLocalEcho: Bool
    var isArchived: Bool

    var archiveID: String? {
        guard case let .archive(mamID) = item.message.source else { return nil }
        return mamID
    }

    /// Timestamped rows first by time; timestampless (live) rows are the
    /// newest; insertion order breaks ties.
    static func precedes(_ lhs: Entry, _ rhs: Entry) -> Bool {
        let left = lhs.sortDate ?? .distantFuture
        let right = rhs.sortDate ?? .distantFuture
        if left != right { return left < right }
        return lhs.order < rhs.order
    }

    func applying(_ ranked: RankedMutation, conversation: ConversationID) -> Entry {
        var next = self
        let isGroupchat = conversation.isRoom
        switch ranked.mutation {
        case let .reaction(_, from, senderKey, isMine, emojis):
            if let existing = mutations.reactionsBySender[senderKey], existing.rank > ranked.rank {
                return self
            }
            var unique: [String] = []
            for emoji in emojis where !unique.contains(emoji) {
                unique.append(emoji)
            }
            // An empty set keeps the sender entry so the clear retains its
            // rank; deleting it would let an older replay resurrect it.
            next.mutations.reactionsBySender[senderKey] = SenderReactions(
                emojis: unique,
                isMine: isMine,
                displayName: isGroupchat ? (from.resource ?? from.bare.description) : (from.bare.localpart ?? from.bare.domain),
                rank: ranked.rank
            )
        case let .correction(_, from, content):
            guard mutations.tombstone == nil,
                  isSameAuthor(from, item.from, isGroupchat: isGroupchat)
            else { return self }
            if let current = mutations.correctionRank, current > ranked.rank {
                return self
            }
            next.mutations.correction = content
            next.mutations.correctionRank = ranked.rank
            if next.mutations.safetyScoresRevisionID != content.sourceRevisionID {
                next.mutations.safetyScores = nil
                next.mutations.safetyScoresRank = nil
                next.mutations.safetyScoresRevisionID = nil
            }
        case let .retraction(_, from):
            guard mutations.tombstone == nil,
                  isSameAuthor(from, item.from, isGroupchat: isGroupchat)
            else { return self }
            next.mutations.tombstone = .retracted
        case let .moderation(_, from, moderatedBy, reason):
            // XEP-0425: only the room itself (bare room JID, no occupant
            // resource) may moderate; an occupant claiming it is a spoof.
            guard mutations.tombstone == nil,
                  from.resource == nil,
                  from.bare == conversation.jid
            else { return self }
            next.mutations.tombstone = .moderated(by: moderatedBy, reason: reason)
        case let .safetyScores(_, from, fastening):
            // Only the room itself (bare room JID) judges its messages; an
            // occupant claiming to is a spoof. XEP-0422: the newest
            // fastening replaces the previous one, a clear included.
            guard from.resource == nil, from.bare == conversation.jid,
                  fastening.targetStanzaBy == conversation.jid,
                  (mutations.correction?.sourceRevisionID ?? item.roomStanzaID) == fastening.sourceRevisionID,
                  mutations.tombstone == nil else { return self }
            if let current = mutations.safetyScoresRank, current > ranked.rank {
                return self
            }
            next.mutations.safetyScores = fastening.scores
            next.mutations.safetyScoresRank = ranked.rank
            next.mutations.safetyScoresRevisionID = fastening.sourceRevisionID
        }
        return next
    }
}
