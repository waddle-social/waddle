import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// Opening a conversation with unread messages puts the "New messages"
/// divider above the first of them. In the split view the conversation
/// screen is built twice, so two screens open it, one after the other.
@MainActor
@Suite("The new messages divider")
struct NewMessagesDividerTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        return (coordinator, port)
    }

    private func archived(_ stanzaIDs: [String]) -> ArchivePage {
        ArchivePage(
            messages: stanzaIDs.enumerated().map { index, id in
                roomMessage(id, from: "bob", stanzaID: id, at: date(TimeInterval(index)), source: .archive(mamID: id))
            },
            first: stanzaIDs.first, isComplete: true)
    }

    private func row(_ id: String, in coordinator: SessionCoordinator) -> String? {
        coordinator.timelines.timeline(for: roomConversation).item(withID: id)?.presentationID
    }

    @Test func aSecondScreenOpeningTheConversationStillShowsTheDivider() async {
        let (coordinator, port) = online()
        port.historyPages = [archived(["s1", "s2", "s3"])]
        coordinator.unread.set(2, for: roomConversation)

        // The first open clears the count the second would have read.
        await coordinator.open(roomConversation)
        await coordinator.open(roomConversation)

        #expect(coordinator.unreadAnchor(for: roomConversation) == row("s2", in: coordinator))
    }

    @Test func theDividerStaysWhenTheFirstScreenGoesWhileItOpens() async {
        let (coordinator, port) = online()
        port.historyPages = [archived(["s1", "s2", "s3"])]
        port.holdsHistory = true
        coordinator.unread.set(2, for: roomConversation)

        let first = Task { await coordinator.open(roomConversation) }
        while port.heldHistory.isEmpty { await Task.yield() }
        let second = Task { await coordinator.open(roomConversation) }
        for _ in 0..<20 { await Task.yield() }
        // The first screen is replaced before its open finished.
        coordinator.close(roomConversation)
        port.holdsHistory = false
        port.releaseHistory()
        await first.value
        await second.value

        #expect(coordinator.unreadAnchor(for: roomConversation) == row("s2", in: coordinator))
    }

    /// The first screen opens, is gone, and the next opens; on a phone a
    /// thread or the details cover the conversation and uncover it.
    @Test func theDividerStaysWhenTheConversationIsOpenedAgain() async {
        let (coordinator, port) = online()
        port.historyPages = [archived(["s1", "s2", "s3"])]
        coordinator.unread.set(2, for: roomConversation)
        await coordinator.open(roomConversation)
        coordinator.close(roomConversation)

        await coordinator.open(roomConversation)

        #expect(coordinator.unreadAnchor(for: roomConversation) == row("s2", in: coordinator))
    }

    @Test func openingAnotherConversationForgetsTheDivider() async {
        let (coordinator, port) = online()
        port.historyPages = [archived(["s1", "s2", "s3"])]
        coordinator.unread.set(2, for: roomConversation)
        await coordinator.open(roomConversation)
        coordinator.close(roomConversation)

        await coordinator.open(bobConversation)
        coordinator.close(bobConversation)
        #expect(coordinator.unreadAnchor(for: roomConversation) == nil)

        // Back again with everything read: no divider.
        await coordinator.open(roomConversation)
        #expect(coordinator.unreadAnchor(for: roomConversation) == nil)
    }
}
