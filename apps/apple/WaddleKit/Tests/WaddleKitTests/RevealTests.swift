import Foundation
import Testing
@testable import WaddleKit

/// Jumping to a search hit or pin pages XEP-0313 history (RSM `before`)
/// until the message is loaded.
@MainActor
@Suite("Reveal message")
struct RevealTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        return (coordinator, port)
    }

    /// An archive page, oldest message first; its RSM `<first/>` is the
    /// oldest id.
    private func page(_ ids: [Int], complete: Bool = false) -> ArchivePage {
        let messages = ids.map {
            roomMessage("m\($0)", from: "bob", stanzaID: "s\($0)", at: date(TimeInterval($0)), source: .archive(mamID: "s\($0)"))
        }
        return ArchivePage(messages: messages, first: ids.first.map { "s\($0)" }, isComplete: complete)
    }

    private func cursors(_ port: FakePort) -> [String?] {
        port.historyRequests.map(\.1)
    }

    /// Lets queued main-actor work run until `condition` holds.
    private func settle(until condition: () -> Bool) async {
        for _ in 0..<1_000 where !condition() {
            await Task.yield()
        }
    }

    @Test func alreadyLoadedNeedsNoRequest() async {
        let (coordinator, port) = online()
        port.historyPages = [page([8, 9])]
        await coordinator.loadLatest(roomConversation)
        #expect(await coordinator.reveal(messageID: "s8", in: roomConversation) == .found(itemID: "s8"))
        #expect(port.historyRequests.count == 1)
    }

    @Test func loadsLatestThenPagesBackUntilFound() async {
        let (coordinator, port) = online()
        port.historyPages = [page([8, 9]), page([6, 7]), page([4, 5])]
        #expect(await coordinator.reveal(messageID: "s7", in: roomConversation) == .found(itemID: "s7"))
        #expect(cursors(port) == [nil, "s8"])
    }

    @Test func archiveStartReachedIsNotFound() async {
        let (coordinator, port) = online()
        port.historyPages = [page([8, 9]), page([6, 7], complete: true)]
        #expect(await coordinator.reveal(messageID: "gone", in: roomConversation) == .notFound)
        #expect(cursors(port) == [nil, "s8"])
    }

    @Test func spentBudgetGivesUp() async {
        let (coordinator, port) = online()
        port.historyPages = [page([8, 9]), page([6, 7]), page([4, 5]), page([2, 3])]
        #expect(await coordinator.reveal(messageID: "s2", in: roomConversation, pageBudget: 3) == .gaveUp)
        #expect(cursors(port) == [nil, "s8", "s6"])
    }

    @Test func failedPageFails() async {
        let (coordinator, port) = online()
        port.historyPages = [page([8, 9]), page([6, 7])]
        await coordinator.loadLatest(roomConversation)
        port.failingHistoryRequests = 1
        #expect(await coordinator.reveal(messageID: "s6", in: roomConversation) == .failed)
        #expect(cursors(port) == [nil, "s8"])
    }

    @Test func cancellationStopsPaging() async {
        let (coordinator, port) = online()
        port.historyPages = [page([8, 9]), page([6, 7]), page([4, 5])]
        port.holdsHistory = true
        let reveal = Task { await coordinator.reveal(messageID: "s4", in: roomConversation) }
        await settle { !port.heldHistory.isEmpty }
        reveal.cancel()
        port.releaseHistory()
        #expect(await reveal.value == .gaveUp)
        #expect(cursors(port) == [nil])
    }

    @Test func waitsForScrollLoadInsteadOfDuplicatingIt() async {
        let (coordinator, port) = online()
        port.historyPages = [page([8, 9]), page([6, 7]), page([4, 5])]
        await coordinator.loadLatest(roomConversation)
        port.holdsHistory = true
        let scroll = Task { await coordinator.loadOlder(roomConversation) }
        await settle { !port.heldHistory.isEmpty }
        let reveal = Task { await coordinator.reveal(messageID: "s4", in: roomConversation) }
        await settle { false }
        #expect(cursors(port) == [nil, "s8"])

        port.holdsHistory = false
        port.releaseHistory()
        await scroll.value
        #expect(await reveal.value == .found(itemID: "s4"))
        #expect(cursors(port) == [nil, "s8", "s6"])
    }

    @Test func historyResetStopsPaging() async {
        let (coordinator, port) = online()
        port.historyPages = [page([8, 9]), page([6, 7]), page([4, 5])]
        await coordinator.loadLatest(roomConversation)
        port.holdsHistory = true
        let reveal = Task { await coordinator.reveal(messageID: "s4", in: roomConversation) }
        await settle { !port.heldHistory.isEmpty }
        coordinator.timelines.clear()
        coordinator.history.clear()
        port.holdsHistory = false
        port.releaseHistory()
        #expect(await reveal.value == .failed)
        #expect(cursors(port) == [nil, "s8"])
    }
}

/// A live insert over the cap trims the oldest rows; archived ones must
/// stay reachable by paging, and a revealed row must survive it.
@MainActor
@Suite("Reveal and the live-trim cap")
struct RevealTrimTests {
    private func page(_ ids: ClosedRange<Int>) -> ArchivePage {
        let messages = ids.map {
            roomMessage("m\($0)", from: "bob", stanzaID: "s\($0)", at: date(TimeInterval($0)), source: .archive(mamID: "s\($0)"))
        }
        return ArchivePage(messages: messages, first: "s\(ids.lowerBound)", isComplete: false)
    }

    @Test func defaultBudgetStaysWellInsideTheCapacity() {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        #expect(coordinator.defaultRevealPageBudget == 6)
        #expect(coordinator.defaultRevealPageBudget * SessionCoordinator.historyPageSize <= coordinator.timelines.capacity * 3 / 5)
    }

    @Test func revealedRowSurvivesTheNextLiveMessage() async {
        let port = FakePort()
        // Room for two pages: the default budget is one.
        let coordinator = SessionCoordinator(account: me, port: port, timelineCapacity: 100)
        coordinator.status.connection = .online
        port.historyPages = [page(951...1000), page(901...950)]
        #expect(await coordinator.reveal(messageID: "s951", in: roomConversation) == .found(itemID: "s951"))
        coordinator.route(roomMessage("live", from: "bob", stanzaID: "live-1"))
        #expect(coordinator.timelines.timeline(for: roomConversation).item(withID: "s951") != nil)
        #expect(port.historyRequests.count == 1)
    }

    @Test func trimmedArchiveRowsArePagedAgain() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port, timelineCapacity: 100)
        coordinator.status.connection = .online
        port.historyPages = [page(951...1000), page(901...950)]
        await coordinator.loadLatest(roomConversation)
        await coordinator.loadOlder(roomConversation)
        #expect(coordinator.history.state(of: roomConversation).olderCursor == "s901")

        coordinator.route(roomMessage("live", from: "bob", stanzaID: "live-1"))
        #expect(coordinator.timelines.timeline(for: roomConversation).item(withID: "s901") == nil)
        // The next older page starts where the loaded rows now end.
        #expect(coordinator.history.state(of: roomConversation).olderCursor == "s902")
        #expect(coordinator.history.state(of: roomConversation).hasMoreOlder)
    }

    @Test func trimmingEveryArchivedRowReloadsTheNewestPage() {
        let history = HistoryStore()
        _ = history.begin(roomConversation)
        history.finish(roomConversation, page: page(1...2), wasLatest: true)
        history.rewind(roomConversation, toOlderCursor: nil)
        let state = history.state(of: roomConversation)
        #expect(!state.hasLoadedLatest)
        #expect(state.olderCursor == nil)
        #expect(state.hasMoreOlder)
    }

    private func settle(until condition: () -> Bool) async {
        for _ in 0..<1_000 where !condition() {
            await Task.yield()
        }
    }

    /// A trim while an older page is in flight: that page must not move the
    /// cursor past the rows the rewind refetches.
    @Test func rewindDuringAnInFlightPageIsKept() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port, timelineCapacity: 100)
        coordinator.status.connection = .online
        port.historyPages = [page(951...1000), page(901...950), page(851...900)]
        await coordinator.loadLatest(roomConversation)
        await coordinator.loadOlder(roomConversation)
        port.holdsHistory = true
        let older = Task { await coordinator.loadOlder(roomConversation) }
        await settle { !port.heldHistory.isEmpty }

        coordinator.route(roomMessage("live", from: "bob", stanzaID: "live-1"))
        port.holdsHistory = false
        port.releaseHistory()
        await older.value

        #expect(coordinator.history.state(of: roomConversation).olderCursor == "s902")
        await coordinator.loadOlder(roomConversation)
        #expect(port.historyRequests.last?.1 == "s902")
    }

    /// A live row carries the room's XEP-0359 stanza-id, which is its
    /// archive id: paging continues from it once no archived row is left.
    @Test func liveRowsKeepPagingWhenNoArchivedRowIsLeft() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port, timelineCapacity: 3)
        coordinator.status.connection = .online
        port.historyPages = [page(9...10)]
        await coordinator.loadLatest(roomConversation)
        for index in 1...3 {
            coordinator.route(roomMessage("live", from: "bob", stanzaID: "live-\(index)"))
        }
        let state = coordinator.history.state(of: roomConversation)
        #expect(state.hasLoadedLatest)
        #expect(state.olderCursor == "live-1")
    }

    /// With no archive id left at all, the conversation on screen reloads
    /// its newest page instead of losing paging.
    @Test func noPagingIDLeftReloadsTheVisibleConversation() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port, timelineCapacity: 2)
        coordinator.status.connection = .online
        port.historyPages = [page(9...10), page(20...21)]
        await coordinator.open(roomConversation)
        for index in 1...2 {
            coordinator.route(WireMessage(
                source: .live,
                type: .groupchat,
                from: room.with(resource: "bob"),
                to: JID(bare: me.jid, resource: "phone"),
                identity: MessageIdentity(messageID: "o\(index)", originID: "o\(index)", stanzaID: nil, stanzaIDs: []),
                timestamp: nil,
                body: "unarchived"
            ))
        }
        await settle { port.historyRequests.count == 2 }
        #expect(port.historyRequests.map(\.1) == [nil, nil])
    }
}
