import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Notification settings")
struct NotifySettingsTests {
    private func online(membersOnly: Bool? = nil) -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "general", membersOnly: membersOnly),
        ]))
        return (coordinator, port)
    }

    @Test func aMembersOnlyRoomDefaultsToEveryMessage() {
        let (coordinator, _) = online(membersOnly: true)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .always)
    }

    @Test func anOpenRoomDefaultsToMentions() {
        let (coordinator, _) = online(membersOnly: false)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .onMention)
    }

    @Test func aRoomDefaultsToMentionsUntilDiscoveryReportsMembersOnly() {
        let (coordinator, _) = online()
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .onMention)

        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "general", membersOnly: true),
        ]))
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .always)
    }

    @Test func anUnknownRefreshKeepsTheMembersOnlyRoomDefault() async throws {
        let (coordinator, port) = online(membersOnly: true)
        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "renamed"),
        ]))
        #expect(coordinator.directory.title(for: roomConversation) == "renamed")
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .always)
        try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        #expect(port.notifySettingsWrites == [
            .init(conversation: roomConversation, mode: .always, showsMessagesInPush: true),
        ])
    }

    @Test func aRefreshReportingAnOpenRoomChangesTheDefaultToMentions() {
        let (coordinator, _) = online(membersOnly: true)
        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "general", membersOnly: false),
        ]))
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .onMention)
    }

    @Test func aStoredModeOverridesTheMembersOnlyRoomDefault() async {
        let (coordinator, port) = online(membersOnly: true)
        port.notifySettings = NotifySettings(modes: [roomConversation: .onMention])
        await coordinator.loadNotifyModes()
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .onMention)
    }

    @Test func showingMessagesInPushKeepsTheConversationsMode() async throws {
        let (coordinator, port) = online()
        try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        #expect(coordinator.directory.showsMessagesInPush(for: roomConversation))
        #expect(port.notifySettingsWrites == [
            .init(conversation: roomConversation, mode: .onMention, showsMessagesInPush: true),
        ])
    }

    @Test func showingMessagesInPushKeepsTheMembersOnlyRoomDefault() async throws {
        let (coordinator, port) = online(membersOnly: true)
        try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        #expect(port.notifySettingsWrites == [
            .init(conversation: roomConversation, mode: .always, showsMessagesInPush: true),
        ])
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .always)
    }

    @Test func unmutingRestoresTheMembersOnlyRoomDefault() async throws {
        let (coordinator, port) = online(membersOnly: true)
        try await coordinator.toggleNotifyMute(for: roomConversation)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .never)
        try await coordinator.toggleNotifyMute(for: roomConversation)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .always)
        #expect(port.notifySettingsWrites.map(\.mode) == [.never, .always])
    }

    @Test(arguments: [false, nil] as [Bool?])
    func unmutingAnOpenOrUnknownRoomRestoresMentions(membersOnly: Bool?) async throws {
        let (coordinator, port) = online(membersOnly: membersOnly)
        try await coordinator.toggleNotifyMute(for: roomConversation)
        try await coordinator.toggleNotifyMute(for: roomConversation)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .onMention)
        #expect(port.notifySettingsWrites.map(\.mode) == [.never, .onMention])
    }

    @Test func unmutingADirectConversationRestoresEveryMessage() async throws {
        let (coordinator, port) = online()
        let peer = ConversationID.direct(bare("bob@waddle.test"))
        try await coordinator.toggleNotifyMute(for: peer)
        try await coordinator.toggleNotifyMute(for: peer)
        #expect(coordinator.directory.notifyMode(for: peer) == .always)
        #expect(port.notifySettingsWrites.map(\.mode) == [.never, .always])
    }

    @Test func anUnlistedRoomDefaultsToMentions() async throws {
        let (coordinator, port) = online(membersOnly: true)
        coordinator.directory.apply(.empty)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .onMention)
        try await coordinator.toggleNotifyMute(for: roomConversation)
        try await coordinator.toggleNotifyMute(for: roomConversation)
        #expect(port.notifySettingsWrites.map(\.mode) == [.never, .onMention])
    }

    /// The opt-in is stored with a mode, so it must not turn a group
    /// chat's default (every message) into the room default (mentions).
    @Test func showingMessagesInPushKeepsAGroupChatNotifyingAboutEveryMessage() async throws {
        let (coordinator, port) = online()
        let group = bare("g-1@groups.waddle.test")
        coordinator.directory.upsert(Channel(roomJID: group, name: "Trip", position: 0, isGroupDM: true))
        try await coordinator.setShowsMessagesInPush(true, for: .room(group))
        #expect(port.notifySettingsWrites.map(\.mode) == [.always])
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

    /// A mode change that overlaps a refused opt-in must not store the
    /// opt-in the refusal rolled back.
    @Test func aModeChangeBehindARefusedChangeStoresWhatIsShown() async {
        let (coordinator, port) = online()
        port.notifySettingsWriteResults = [.failure(.rejected), .success(())]
        let first = Task { try? await coordinator.setShowsMessagesInPush(true, for: roomConversation) }
        let second = Task { try? await coordinator.setNotifyMode(.always, for: roomConversation) }
        _ = await (first.value, second.value)
        #expect(port.notifySettingsWrites.last
            == .init(conversation: roomConversation, mode: .always, showsMessagesInPush: false))
        #expect(!coordinator.directory.showsMessagesInPush(for: roomConversation))
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .always)
    }

    /// Another device muted the conversation after this session loaded
    /// its settings: the opt-in must not write the old mode back.
    @Test func showingMessagesInPushKeepsAModeSetOnAnotherDevice() async throws {
        let (coordinator, port) = online()
        port.notifySettings = NotifySettings(modes: [roomConversation: .never])
        try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        #expect(port.notifySettingsWrites.map(\.mode) == [.never])
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .never)
    }

    /// Without the stored mode in hand, writing would risk undoing a
    /// change made on another device.
    @Test func nothingIsWrittenWhenTheStoredSettingsCannotBeRead() async {
        let (coordinator, port) = online()
        port.failsNotifySettingsFetch = true
        await #expect(throws: PortError.timeout) {
            try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        }
        #expect(port.notifySettingsWrites.isEmpty)
        #expect(!coordinator.directory.showsMessagesInPush(for: roomConversation))
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
