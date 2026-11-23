import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// The Mac split view can hold two screens for one conversation. The
/// conversation is on screen until the last of them closes.
@MainActor
@Suite("Screens showing one conversation")
struct ConversationScreensTests {
    private func online() -> SessionCoordinator {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        return coordinator
    }

    @Test func closingOneOfTwoScreensKeepsTheConversationRead() async {
        let coordinator = online()
        let first = ConversationScreenID(), second = ConversationScreenID()
        await coordinator.open(roomConversation, screen: first)
        await coordinator.open(roomConversation, screen: second)

        coordinator.close(roomConversation, screen: first)
        coordinator.handle(.message(roomMessage("m1", from: "bob", stanzaID: "s1")))

        #expect(coordinator.visibleConversation == roomConversation)
        #expect(coordinator.unread.count(for: roomConversation) == 0)
    }

    @Test func closingTheLastScreenLetsMessagesCountAsUnread() async {
        let coordinator = online()
        let first = ConversationScreenID(), second = ConversationScreenID()
        await coordinator.open(roomConversation, screen: first)
        await coordinator.open(roomConversation, screen: second)

        coordinator.close(roomConversation, screen: first)
        coordinator.close(roomConversation, screen: second)
        coordinator.handle(.message(roomMessage("m1", from: "bob", stanzaID: "s1")))

        #expect(coordinator.visibleConversation == nil)
        #expect(coordinator.unread.count(for: roomConversation) == 1)
    }

    /// A screen closed for a conversation it was replaced in must not
    /// keep the next conversation's screens from closing it.
    @Test func screensOfTheConversationShownBeforeAreForgotten() async {
        let coordinator = online()
        let first = ConversationScreenID(), second = ConversationScreenID()
        await coordinator.open(roomConversation, screen: first)
        await coordinator.open(bobConversation, screen: second)

        coordinator.close(bobConversation, screen: second)

        #expect(coordinator.visibleConversation == nil)
    }

    @Test func aScreenThatOpensTwiceClosesOnce() async {
        let coordinator = online()
        let screen = ConversationScreenID()
        await coordinator.open(roomConversation, screen: screen)
        await coordinator.open(roomConversation, screen: screen)

        coordinator.close(roomConversation, screen: screen)

        #expect(coordinator.visibleConversation == nil)
    }
}
