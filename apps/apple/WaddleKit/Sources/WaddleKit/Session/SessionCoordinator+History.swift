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
        switch conversation.kind {
        case .direct: _ = directConversation(with: conversation.jid)
        case .room: await ensureJoined(conversation.jid)
        }
        if !history.state(of: conversation).hasLoadedLatest {
            await loadLatest(conversation)
        }
        await markDisplayedIfVisible(conversation)
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
            await markDisplayedIfVisible(visible)
        } else {
            unread.clearActive(ifMatches: visible)
        }
    }

    /// Fetches the newest page and merges it. A failed fetch leaves the
    /// paging state untouched and marks the conversation for retry.
    public func loadLatest(_ conversation: ConversationID) async {
        guard history.begin(conversation) else { return }
        do {
            let page = try await port.fetchHistory(of: conversation, before: nil, max: Self.historyPageSize)
            ingestArchive(page)
            history.finish(conversation, page: page, wasLatest: true)
        } catch {
            history.fail(conversation)
        }
    }

    /// Fetches the next older page, if any. Without a loaded newest page
    /// (a failed first load, or a trim that left no archived row to page
    /// from) this loads the newest page instead.
    public func loadOlder(_ conversation: ConversationID) async {
        let state = history.state(of: conversation)
        if !state.hasLoadedLatest {
            await loadLatest(conversation)
            return
        }
        guard state.hasLoadedLatest, state.hasMoreOlder, let cursor = state.olderCursor else { return }
        guard history.begin(conversation) else { return }
        do {
            let page = try await port.fetchHistory(of: conversation, before: cursor, max: Self.historyPageSize)
            ingestArchive(page)
            history.finish(conversation, page: page, wasLatest: false)
        } catch {
            history.fail(conversation)
        }
    }

    /// A live insert trimmed archived rows: page older from the oldest one
    /// left, or, when none is left, reload the newest page of a
    /// conversation on screen (after any load in flight) so paging works
    /// again.
    func archiveTrimmed(_ conversation: ConversationID, cursor: String?) {
        history.rewind(conversation, toOlderCursor: cursor)
        guard cursor == nil, visibleConversation == conversation else { return }
        Task { [weak self] in
            guard let self else { return }
            await self.history.waitUntilIdle(conversation)
            await self.loadLatest(conversation)
        }
    }

    /// Full-text search over the conversation's archive. Throws when the
    /// query failed, so the caller can tell failure from no matches.
    public func search(_ query: String, in conversation: ConversationID) async throws -> [TimelineItem] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return [] }
        let page = try await port.searchHistory(of: conversation, query: trimmed, max: Self.historyPageSize)
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
            await markDisplayedIfVisible(active)
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
