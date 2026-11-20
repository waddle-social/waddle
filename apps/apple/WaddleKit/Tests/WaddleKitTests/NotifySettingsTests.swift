import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Notification settings")
struct NotifySettingsTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        return (coordinator, port)
    }

    @Test func showingMessagesInPushKeepsTheConversationsMode() async throws {
        let (coordinator, port) = online()
        try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        #expect(coordinator.directory.showsMessagesInPush(for: roomConversation))
        #expect(port.notifySettingsWrites == [
            .init(conversation: roomConversation, mode: .onMention, showsMessagesInPush: true),
        ])
    }

    @Test func aRefusedChangeIsRolledBack() async {
        let (coordinator, port) = online()
        port.notifySettingsWriteResult = .failure(.rejected)
        await #expect(throws: PortError.rejected) {
            try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        }
        #expect(!coordinator.directory.showsMessagesInPush(for: roomConversation))
    }

    @Test func changingTheModeKeepsShowingMessagesInPush() async throws {
        let (coordinator, port) = online()
        try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        try await coordinator.setNotifyMode(.always, for: roomConversation)
        #expect(port.notifySettingsWrites.last
            == .init(conversation: roomConversation, mode: .always, showsMessagesInPush: true))
        #expect(coordinator.directory.showsMessagesInPush(for: roomConversation))
    }

    @Test func loadingReplacesBothSettings() async {
        let (coordinator, port) = online()
        coordinator.directory.setShowsMessagesInPush(true, for: roomConversation)
        let peer = ConversationID.direct(bare("bob@waddle.test"))
        port.notifySettings = NotifySettings(modes: [peer: .never], showingMessagesInPush: [peer])
        await coordinator.loadNotifyModes()
        #expect(coordinator.directory.notifyMode(for: peer) == .never)
        #expect(coordinator.directory.showsMessagesInPush(for: peer))
        #expect(!coordinator.directory.showsMessagesInPush(for: roomConversation))
    }

    @Test func aDeletedRoomForgetsTheSetting() async throws {
        let (coordinator, _) = online()
        try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        coordinator.directory.remove(room)
        #expect(!coordinator.directory.showsMessagesInPush(for: roomConversation))
    }
}
