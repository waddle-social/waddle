import Foundation

extension SessionCoordinator {
    /// Page size for XEP-0313 history requests.
    static var historyPageSize: Int { 50 }

    /// Called when a conversation comes on screen: marks it active, loads
    /// the newest page once, and marks it read.
    public func open(_ conversation: ConversationID) async {
        visibleConversation = conversation
        unread.setActive(isAppActive ? conversation : nil)
        inbox.markRead(conversation.jid)
        if conversation.kind == .direct {
            _ = directConversation(with: conversation.jid)
        }
        if !history.state(of: conversation).hasLoadedLatest {
            await loadLatest(conversation)
        }
        guard isAppActive else { return }
        await markDisplayed(conversation)
        if conversation.isRoom {
            await refreshPins(in: conversation.jid)
        }
    }

    /// Called when a conversation leaves the screen.
    public func close(_ conversation: ConversationID) {
        if visibleConversation == conversation {
            visibleConversation = nil
        }
        unread.clearActive(ifMatches: conversation)
    }

    /// The app moved to or from the foreground. A conversation on screen
    /// in a background app is not being read: its messages count as unread
    /// and get no read markers until the app is active again.
    public func setAppActive(_ active: Bool) async {
        isAppActive = active
        guard let visible = visibleConversation else { return }
        if active {
            unread.setActive(visible)
            await markDisplayed(visible)
        } else {
            unread.clearActive(ifMatches: visible)
        }
    }

    /// Fetches the newest page and merges it.
    public func loadLatest(_ conversation: ConversationID) async {
        guard history.begin(conversation) else { return }
        let page = await port.fetchHistory(of: conversation, before: nil, max: Self.historyPageSize)
        ingestArchive(page)
        history.finish(conversation, page: page, wasLatest: true)
    }

    /// Fetches the next older page, if any.
    public func loadOlder(_ conversation: ConversationID) async {
        let state = history.state(of: conversation)
        guard state.hasLoadedLatest, state.hasMoreOlder, let cursor = state.olderCursor else { return }
        guard history.begin(conversation) else { return }
        let page = await port.fetchHistory(of: conversation, before: cursor, max: Self.historyPageSize)
        ingestArchive(page)
        history.finish(conversation, page: page, wasLatest: false)
    }

    /// Full-text search over the conversation's archive.
    public func search(_ query: String, in conversation: ConversationID) async -> [TimelineItem] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return [] }
        let page = await port.searchHistory(of: conversation, query: trimmed, max: Self.historyPageSize)
        let scratch = TimelineStore()
        scratch.account = account
        for message in page.messages {
            scratch.ingest(message)
        }
        return scratch.timeline(for: conversation).feedItems.reversed()
    }

    /// After a reconnect: refresh the conversation on screen, and make
    /// every other loaded conversation refetch its newest page on open.
    func reloadActiveConversation() async {
        history.markAllStale()
        if let active = unread.activeConversation {
            await loadLatest(active)
            await markDisplayed(active)
        }
    }

    func ingestArchive(_ page: ArchivePage) {
        for message in page.messages {
            let result = timelines.ingest(message)
            if case let .inserted(item) = result,
               !item.conversation.isRoom,
               item.tombstone == nil {
                directory.touchDirect(item.conversation.jid, at: item.sentAt, preview: preview(of: item))
            }
        }
    }

    func refreshPins(in room: BareJID) async {
        let version = pins.versionBeforeFetch(room)
        guard let entries = try? await port.fetchPins(in: room) else { return }
        pins.seed(entries, in: room, fetchedAt: version)
    }
}
