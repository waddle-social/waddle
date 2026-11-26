import Foundation

extension SessionCoordinator {
    /// Page size for XEP-0313 history requests.
    static var historyPageSize: Int { 50 }

    /// Called when a conversation comes on screen: marks it active, loads
    /// the newest page once, and marks it read. `screen` names the screen
    /// showing it, which passes the same value to `close`.
    public func open(_ conversation: ConversationID, screen: ConversationScreenID) async {
        // Read before the badge is cleared below. Opening the conversation
        // again finds nothing unread and keeps the divider placed before.
        let unreadAtOpen = unread.count(for: conversation)
        if lastOpenedConversation != conversation {
            lastOpenedConversation = conversation
            if !unreadAnchors.isEmpty { unreadAnchors.removeAll() }
        }
        if visibleConversation != conversation { visibleScreens.removeAll() }
        visibleConversation = conversation
        visibleScreens.insert(screen)
        unread.setActive(isAppActive ? conversation : nil)
        inbox.markRead(conversation.jid)
        switch conversation.kind {
        case .direct: _ = directConversation(with: conversation.jid)
        case .room: await ensureJoined(conversation.jid)
        }
        if !history.state(of: conversation).hasLoadedLatest {
            await loadLatest(conversation)
        }
        // Placed by the open that saw the messages unread, even when its own
        // screen has gone meanwhile, unless another conversation was opened.
        if unreadAtOpen > 0, lastOpenedConversation == conversation {
            unreadAnchors[conversation] = TimelineUnreadAnchor.firstUnreadID(
                in: timelines.timeline(for: conversation).feedItems, unreadCount: unreadAtOpen)
        }
        await markDisplayedIfVisible(conversation)
        // A room's pins, or those of a conversation of two: one query, to the room or to the other person.
        if keepsPins(conversation) {
            await refreshPins(in: conversation.jid)
        }
        if conversation.isRoom {
            await refreshRoomBots(in: conversation.jid)
        }
    }

    /// The row the conversation's "New messages" divider sits above, until
    /// another conversation is opened; nil when nothing was unread.
    public func unreadAnchor(for conversation: ConversationID) -> String? {
        unreadAnchors[conversation]
    }

    /// Called when a screen stops showing a conversation. The conversation
    /// stays visible while another screen still shows it.
    public func close(_ conversation: ConversationID, screen: ConversationScreenID) {
        if visibleConversation == conversation {
            visibleScreens.remove(screen)
            guard visibleScreens.isEmpty else { return }
            visibleConversation = nil
        }
        unread.clearActive(ifMatches: conversation)
    }

    /// The app moved to or from the foreground. A conversation on screen
    /// in a background app is not being read: its messages count as unread
    /// and get no read markers until the app is active again.
    public func setAppActive(_ active: Bool) async {
        isAppActive = active
        if let thread = visibleThread {
            if active {
                unread.setActiveThread(thread)
                await markThreadReadIfVisible(thread)
            } else {
                unread.clearActiveThread(ifMatches: thread)
            }
        }
        guard let visible = visibleConversation else { return }
        if active {
            unread.setActive(visible)
            await markDisplayedIfVisible(visible)
        } else {
            unread.clearActive(ifMatches: visible)
        }
    }

    /// Called when a room thread comes on screen: joins the room (a thread
    /// opened from Activity skips the room screen), marks the thread read
    /// and loads its newest replies.
    public func openThread(_ thread: ThreadKey) async {
        visibleThread = thread
        unread.setActiveThread(isAppActive ? thread : nil)
        await ensureJoined(thread.room)
        await markThreadReadIfVisible(thread)
        await loadThreadHistory(thread)
    }

    /// Called when a room thread leaves the screen.
    public func closeThread(_ thread: ThreadKey) {
        if visibleThread == thread {
            visibleThread = nil
        }
        unread.clearActiveThread(ifMatches: thread)
    }

    /// Fetches the thread's newest replies (and its root, which the thread
    /// filter includes) into `threadHistory`. A failed fetch keeps what was
    /// loaded before.
    public func loadThreadHistory(_ thread: ThreadKey) async {
        let epoch = connectionEpoch
        guard connection == .online,
              let page = try? await port.fetchThreadHistory(
                  in: thread.room,
                  threadID: thread.threadID,
                  before: nil,
                  max: Self.historyPageSize
              ),
              epoch == connectionEpoch
        else { return }
        threadHistory.store(ThreadHistory.build(from: page, thread: thread, account: account), for: thread)
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
        scratch.ingest(contentsOf: page.messages)
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
        // A thread opened offline, or open across the drop, loads now.
        if let thread = visibleThread {
            await loadThreadHistory(thread)
            await markThreadReadIfVisible(thread)
        }
    }

    func ingestArchive(_ page: ArchivePage) {
        for result in timelines.ingest(contentsOf: page.messages) {
            if case let .inserted(item) = result,
               !item.conversation.isRoom,
               item.tombstone == nil {
                directory.touchDirect(item.conversation.jid, at: item.sentAt, preview: preview(of: item))
            }
        }
    }
}

/// Names one screen showing a conversation, so that closing one of two
/// screens on the same conversation does not hide it from the other.
public struct ConversationScreenID: Hashable, Sendable {
    private let id: UUID

    public init() { id = UUID() }
}
