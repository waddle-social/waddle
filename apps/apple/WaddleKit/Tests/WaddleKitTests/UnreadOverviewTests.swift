import Foundation
import Testing
@testable import WaddleKit

private let other = bare("random@muc.waddle.test")
private let thread = ThreadKey(room: room, threadID: "root-1")

private func roomRow(_ partner: BareJID = room, unread: Int, last: String, updated: Int64, preview: String? = nil) -> InboxEntry {
    InboxEntry(partner: partner, kind: .room, lastStanzaID: last, lastUpdated: updated, unread: unread, preview: preview)
}

private func threadRow(
    _ key: ThreadKey = thread,
    unread: Int,
    last: String,
    updated: Int64,
    title: String? = nil,
    preview: String? = nil,
    kind: InboxEntry.Kind = .room
) -> InboxEntry {
    InboxEntry(
        partner: key.room,
        kind: kind,
        lastStanzaID: last,
        lastUpdated: updated,
        unread: unread,
        preview: preview,
        thread: InboxEntry.Thread(id: key.threadID, title: title)
    )
}

/// An archived room message, optionally a reply in `thread`.
private func archived(_ body: String, from nick: String, stanzaID: String, at seconds: TimeInterval, thread: String? = nil) -> WireMessage {
    var message = roomMessage(body, from: nick, stanzaID: stanzaID, at: date(seconds), source: .archive(mamID: stanzaID))
    message.thread = thread
    return message
}

private func page(_ messages: [WireMessage]) -> ArchivePage {
    ArchivePage(messages: messages, first: messages.first.flatMap { $0.identity.stanzaID?.id }, isComplete: true)
}

@MainActor
@Suite("Unread overview")
struct UnreadOverviewTests {
    private func online(_ port: FakePort = FakePort()) -> (SessionCoordinator, FakePort) {
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "general"),
            Channel(roomJID: other, name: "random"),
        ]))
        return (coordinator, port)
    }

    // MARK: Inbox thread rows

    @Test func threadRowSetsThreadBadgeNotRoomBadge() {
        let (coordinator, _) = online()
        coordinator.handle(.inboxPush(threadRow(unread: 3, last: "r3", updated: 10)))
        #expect(coordinator.unread.threadCount(for: thread) == 3)
        #expect(coordinator.unread.count(for: roomConversation) == 0)
    }

    @Test func directThreadRowNeverOverwritesTheConversationBadge() {
        let (coordinator, _) = online()
        coordinator.handle(.inboxPush(InboxEntry(partner: bob, kind: .direct, lastStanzaID: "d1", lastUpdated: 5, unread: 2, preview: nil)))
        coordinator.handle(.inboxPush(threadRow(ThreadKey(room: bob, threadID: "call"), unread: 9, last: "d2", updated: 6, kind: .direct)))
        #expect(coordinator.unread.count(for: bobConversation) == 2)
        #expect(coordinator.unread.threadCounts.isEmpty)
    }

    // MARK: Candidates

    @Test func candidatesAreNewestFirstAndSkipUnknownRooms() {
        let (coordinator, _) = online()
        let ghost = bare("left@muc.waddle.test")
        coordinator.handle(.inboxPush(roomRow(unread: 2, last: "s2", updated: 10)))
        coordinator.handle(.inboxPush(roomRow(other, unread: 1, last: "o1", updated: 30)))
        coordinator.handle(.inboxPush(roomRow(ghost, unread: 5, last: "g1", updated: 99)))
        // A thread-only room sorts by its thread row's recency.
        coordinator.handle(.inboxPush(threadRow(unread: 1, last: "r1", updated: 40, title: "Launch plan")))

        let candidates = coordinator.overviewCandidates()
        #expect(candidates.map(\.room) == [room, other])
        #expect(candidates[0].lastUpdated == 40)
        #expect(candidates[0].threads.map(\.title) == ["Launch plan"])
    }

    @Test func threadTitleFallsBackToPreviewThenLabel() {
        #expect(UnreadOverview.threadTitle(threadRow(unread: 1, last: "a", updated: 1, title: "  ", preview: "hello")) == "hello")
        #expect(UnreadOverview.threadTitle(threadRow(unread: 1, last: "a", updated: 1)) == "Thread")
        #expect(UnreadOverview.threadTitle(nil) == "Thread")
    }

    // MARK: Refresh

    @Test func refreshShowsUnreadRoomAndThreadMessages() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(roomRow(unread: 2, last: "s3", updated: 10)))
        coordinator.handle(.inboxPush(threadRow(unread: 1, last: "r2", updated: 11, title: "Root")))
        port.historyPages = [page([
            archived("old", from: "bob", stanzaID: "s1", at: 1),
            archived("new one", from: "bob", stanzaID: "s2", at: 2),
            archived("mine", from: me.nick, stanzaID: "sm", at: 3),
            archived("newer", from: "carol", stanzaID: "s3", at: 4),
        ])]
        port.threadPages[thread.threadID] = page([
            archived("Root", from: "bob", stanzaID: "root-1", at: 1),
            archived("reply a", from: "carol", stanzaID: "r1", at: 5, thread: "root-1"),
            archived("reply b", from: "dave", stanzaID: "r2", at: 6, thread: "root-1"),
        ])

        await coordinator.refreshUnreadOverview()

        let groups = coordinator.unreadOverview.groups
        #expect(groups.count == 1)
        #expect(groups[0].title == "general")
        #expect(groups[0].unread == 2)
        #expect(groups[0].messages.map(\.body) == ["new one", "newer"])
        #expect(groups[0].threads.map(\.title) == ["Root"])
        // The root the thread filter returns is not an unread reply.
        #expect(groups[0].threads[0].messages.map(\.body) == ["reply b"])
        #expect(!groups[0].isIncomplete)
        #expect(coordinator.unreadOverview.hasLoaded)
        #expect(port.historyRequests.map(\.0) == [roomConversation])
        #expect(port.threadRequests.map(\.threadID) == [thread.threadID])
        #expect(port.threadRequests.first?.max == UnreadOverview.fetchSize(for: 1))
    }

    @Test func roomMessagesStopAtTheReadCursor() async {
        let (coordinator, port) = online()
        await coordinator.loadLatest(roomConversation)
        coordinator.route(roomMessage("seen", from: "bob", stanzaID: "s1"))
        await coordinator.markDisplayed(roomConversation)
        // The server count also includes a thread reply, so it exceeds the
        // feed rows after the cursor.
        coordinator.handle(.inboxPush(roomRow(unread: 3, last: "s2", updated: 20)))
        port.historyPages = [page([
            archived("seen", from: "bob", stanzaID: "s1", at: 1),
            archived("fresh", from: "bob", stanzaID: "s2", at: 2),
        ])]

        await coordinator.refreshUnreadOverview()

        #expect(coordinator.unreadOverview.groups.first?.messages.map(\.body) == ["fresh"])
    }

    @Test func unchangedSectionsAreNotRefetched() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(roomRow(unread: 1, last: "s1", updated: 10)))
        port.historyPages = [page([archived("hi", from: "bob", stanzaID: "s1", at: 1)])]
        await coordinator.refreshUnreadOverview()
        await coordinator.refreshUnreadOverview()
        #expect(port.historyRequests.count == 1)
        #expect(coordinator.unreadOverview.groups.first?.messages.map(\.body) == ["hi"])

        // A new message changes the section, which refetches.
        coordinator.handle(.inboxPush(roomRow(unread: 2, last: "s2", updated: 11)))
        await coordinator.refreshUnreadOverview()
        #expect(port.historyRequests.count == 2)
    }

    @Test func failedSectionKeepsTheGroupAndFlagsIt() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(threadRow(unread: 2, last: "r2", updated: 10)))
        port.failingThreads = [thread.threadID]

        await coordinator.refreshUnreadOverview()

        let group = coordinator.unreadOverview.groups.first
        #expect(group?.threads.first?.unread == 2)
        #expect(group?.threads.first?.messages.isEmpty == true)
        #expect(group?.isIncomplete == true)
        #expect(coordinator.unreadOverview.didFail)
    }

    @Test func offlineRefreshShowsCountsWithoutFetching() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(roomRow(unread: 4, last: "s4", updated: 10)))
        coordinator.status.connection = .offline(retryAt: nil)
        await coordinator.refreshUnreadOverview()
        #expect(coordinator.unreadOverview.groups.first?.unread == 4)
        #expect(port.historyRequests.isEmpty)
        #expect(!coordinator.unreadOverview.didFail)
    }

    @Test func signOutDropsTheOverview() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(roomRow(unread: 1, last: "s1", updated: 10)))
        port.historyPages = [page([archived("hi", from: "bob", stanzaID: "s1", at: 1)])]
        await coordinator.refreshUnreadOverview()
        #expect(!coordinator.unreadOverview.groups.isEmpty)
        coordinator.unreadOverview.reset()
        #expect(coordinator.unreadOverview.groups.isEmpty)
        #expect(!coordinator.unreadOverview.hasLoaded)
    }

    // MARK: Read state

    @Test func markDisplayedReadsTheServerWithoutALoadedTimeline() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(roomRow(unread: 3, last: "s3", updated: 10)))
        await coordinator.markDisplayed(roomConversation)
        #expect(port.inboxReads == [room])
        #expect(port.displayed.isEmpty)
        #expect(coordinator.unread.count(for: roomConversation) == 0)
        // The server's echo of the same newest message stays cleared.
        coordinator.handle(.inboxPush(roomRow(unread: 3, last: "s3", updated: 10)))
        #expect(coordinator.unread.count(for: roomConversation) == 0)
    }

    @Test func markDisplayedReadsTheServerWhenOnlyThreadRepliesArrived() async {
        let (coordinator, port) = online()
        await coordinator.loadLatest(roomConversation)
        coordinator.route(roomMessage("hi", from: "bob", stanzaID: "s1"))
        await coordinator.markDisplayed(roomConversation)
        #expect(port.displayed.map(\.id) == ["s1"])
        // A thread reply bumps the room row but adds no feed row to mark.
        coordinator.handle(.inboxPush(roomRow(unread: 1, last: "r1", updated: 20)))
        await coordinator.markDisplayed(roomConversation)
        #expect(port.displayed.map(\.id) == ["s1"])
        #expect(port.inboxReads == [room, room])
    }

    @Test func nothingToReadSendsNoInboxIQ() async {
        let (coordinator, port) = online()
        await coordinator.markDisplayed(roomConversation)
        #expect(port.inboxReadRequests.isEmpty)
    }

    @Test func archivedRowIsNotAMarkerTargetBeforeTheNewestPageLoads() {
        let (coordinator, _) = online()
        coordinator.ingestArchive(page([archived("old", from: "bob", stanzaID: "s1", at: 1)]))
        #expect(coordinator.newestDisplayedTarget(in: roomConversation) == nil)
    }

    @Test func markThreadReadSendsTheThread() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(threadRow(unread: 2, last: "r2", updated: 10)))
        await coordinator.markThreadRead(thread)
        #expect(port.inboxReadRequests.map(\.threadID) == [thread.threadID])
        #expect(coordinator.unread.threadCount(for: thread) == 0)
    }

    @Test func markOverviewReadClearsRoomsAndThreads() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(roomRow(unread: 2, last: "s2", updated: 10)))
        coordinator.handle(.inboxPush(threadRow(unread: 1, last: "r1", updated: 11)))
        await coordinator.markOverviewRead()
        #expect(port.inboxReadRequests.map(\.threadID) == [nil, thread.threadID])
        #expect(coordinator.overviewCandidates().isEmpty)
    }

    @Test func offlineReadReplaysOnlyWhenNothingNewerArrived() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(roomRow(unread: 2, last: "s2", updated: 10)))
        coordinator.handle(.inboxPush(roomRow(other, unread: 1, last: "o1", updated: 10)))
        coordinator.status.connection = .offline(retryAt: nil)
        await coordinator.markDisplayed(roomConversation)
        await coordinator.markDisplayed(.room(other))
        #expect(port.inboxReadRequests.isEmpty)

        coordinator.status.connection = .online
        port.inbox = [
            roomRow(unread: 2, last: "s2", updated: 10),
            // A message arrived in `other` while offline: that read is stale.
            roomRow(other, unread: 2, last: "o2", updated: 20),
        ]
        #expect(await coordinator.hydrateInbox())
        #expect(port.inboxReads == [room])
        #expect(coordinator.unread.count(for: roomConversation) == 0)
        #expect(coordinator.unread.count(for: .room(other)) == 2)
    }

    @Test func openingAnUnreadRoomReadsItOnTheServer() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(roomRow(unread: 3, last: "s3", updated: 10)))
        await coordinator.open(roomConversation)
        #expect(port.inboxReads == [room])
        // Reopening with nothing new sends nothing.
        coordinator.close(roomConversation)
        await coordinator.open(roomConversation)
        #expect(port.inboxReads == [room])
    }

    @Test func refusedReadOnScreenDoesNotLoop() async {
        let (coordinator, port) = online()
        coordinator.inboxHydrateRetryDelays = [0]
        await coordinator.open(roomConversation)
        port.failingInboxReads = 1000
        port.inbox = [roomRow(unread: 2, last: "s2", updated: 10)]
        coordinator.handle(.inboxPush(roomRow(unread: 2, last: "s2", updated: 10)))
        await eventually(timeout: 0.3) { port.inboxReadRequests.count > 5 }
        #expect(port.inboxReadRequests.count == 1)
    }

    @Test func newerReplyIsReadAfterOneRefusal() async {
        let (coordinator, port) = online()
        coordinator.inboxHydrateRetryDelays = [60]
        await coordinator.openThread(thread)
        port.failingInboxReads = 1
        coordinator.handle(.inboxPush(threadRow(unread: 1, last: "r1", updated: 10)))
        await eventually { port.inboxReadRequests.count == 1 }
        // The server recovered; a newer reply arrives while the thread is open.
        coordinator.handle(.inboxPush(threadRow(unread: 2, last: "r2", updated: 20)))
        await eventually { port.inboxReadRequests.count == 2 }
        #expect(port.inboxReadRequests.count == 2)
        #expect(port.inboxReads == [room])
    }

    @Test func streamChangeFreesTheHydrateRetrySlot() {
        let (coordinator, _) = online()
        coordinator.scheduleInboxHydrate(delays: [60])
        #expect(coordinator.inboxHydrateTask != nil)
        coordinator.handle(.disconnected)
        #expect(coordinator.inboxHydrateTask == nil)
    }

    @Test func reconnectLoadsTheThreadOnScreen() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)
        await coordinator.openThread(thread)
        #expect(port.threadRequests.isEmpty)
        coordinator.status.connection = .online
        await coordinator.reloadActiveConversation()
        #expect(port.threadRequests.map(\.threadID) == [thread.threadID])
    }

    @Test func rejectedReadDropsTheBarrierAndRehydrates() async {
        let (coordinator, port) = online()
        coordinator.inboxHydrateRetryDelays = [0]
        coordinator.handle(.inboxPush(roomRow(unread: 2, last: "s2", updated: 10)))
        port.failingInboxReads = 1
        port.inbox = [roomRow(unread: 2, last: "s2", updated: 10)]
        await coordinator.markDisplayed(roomConversation)
        // The server kept the count, and the badge comes back with it.
        await eventually { coordinator.unread.count(for: roomConversation) == 2 }
        #expect(coordinator.unread.count(for: roomConversation) == 2)
    }

    @Test func failedHydrateRetries() async {
        let (coordinator, port) = online()
        port.failsInboxFetch = true
        #expect(await !coordinator.hydrateInbox())
        port.failsInboxFetch = false
        port.inbox = [roomRow(unread: 5, last: "s5", updated: 10)]
        coordinator.scheduleInboxHydrate(delays: [0.01])
        await eventually { coordinator.unread.count(for: roomConversation) == 5 }
        #expect(coordinator.unread.count(for: roomConversation) == 5)
    }

    // MARK: Threads on screen

    @Test func openThreadJoinsReadsAndLoadsReplies() async {
        let (coordinator, port) = online()
        coordinator.handle(.inboxPush(threadRow(unread: 1, last: "r1", updated: 10)))
        port.threadPages[thread.threadID] = page([
            archived("Root", from: "bob", stanzaID: "root-1", at: 1),
            archived("reply", from: "carol", stanzaID: "r1", at: 2, thread: "root-1"),
        ])

        await coordinator.openThread(thread)

        #expect(port.joined == [room])
        #expect(port.inboxReadRequests.map(\.threadID) == [thread.threadID])
        let history = coordinator.threadHistory.history(for: thread)
        #expect(history?.root?.body == "Root")
        #expect(history?.replies.map(\.body) == ["reply"])
    }

    @Test func pushForTheOpenThreadIsReadInsteadOfBadged() async {
        let (coordinator, port) = online()
        await coordinator.openThread(thread)
        coordinator.handle(.inboxPush(threadRow(unread: 1, last: "r9", updated: 30)))
        #expect(coordinator.unread.threadCount(for: thread) == 0)
        await eventually { port.inboxReadRequests.contains { $0.threadID == thread.threadID } }
        #expect(port.inboxReadRequests.contains { $0.threadID == thread.threadID })

        coordinator.closeThread(thread)
        coordinator.handle(.inboxPush(threadRow(unread: 2, last: "r10", updated: 31)))
        #expect(coordinator.unread.threadCount(for: thread) == 2)
    }

    @Test func mergedThreadRepliesPreferLiveRowsAndSortByTime() {
        let store = TimelineStore()
        store.account = me
        store.ingest(archived("a", from: "bob", stanzaID: "r1", at: 1, thread: "root-1"))
        store.ingest(archived("c", from: "bob", stanzaID: "r3", at: 3, thread: "root-1"))
        let live = store.timeline(for: roomConversation).threadReplies(threadID: "root-1")

        let fetchedStore = TimelineStore()
        fetchedStore.account = me
        fetchedStore.ingest(archived("a (fetched copy)", from: "bob", stanzaID: "r1", at: 1, thread: "root-1"))
        fetchedStore.ingest(archived("b", from: "bob", stanzaID: "r2", at: 2, thread: "root-1"))
        let fetched = fetchedStore.timeline(for: roomConversation).threadReplies(threadID: "root-1")

        #expect(ThreadHistory.merged(live: live, fetched: fetched).map(\.body) == ["a", "b", "c"])
    }
}
