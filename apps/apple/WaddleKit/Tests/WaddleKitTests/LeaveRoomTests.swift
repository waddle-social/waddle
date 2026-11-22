import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Leaving and rejoining a room")
struct LeaveRoomTests {
    private let general = Channel(roomJID: room, name: "general")

    private func online(_ channels: [Channel]? = nil) -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        let topology = Topology(spaces: [], channels: channels ?? [general])
        port.topology = topology
        coordinator.directory.apply(topology)
        return (coordinator, port)
    }

    private func enter(_ coordinator: SessionCoordinator) {
        _ = coordinator.presence.apply(WirePresence(
            from: jid("general@muc.waddle.test/alice"), kind: .available,
            occupant: .init(affiliation: .member, role: .participant, realJID: nil, statusCodes: [110])))
    }

    private func inboxRow(unread: Int) -> InboxEntry {
        InboxEntry(partner: room, kind: .room, lastStanzaID: "s1", lastUpdated: 1, unread: unread, preview: nil)
    }

    @Test func leavingRecordsItOnTheAccountAndLeavesTheRoom() async throws {
        let (coordinator, port) = online()
        enter(coordinator)
        coordinator.unread.set(3, for: roomConversation)

        try await coordinator.leaveRoom(room)

        #expect(port.membershipWrites == [.init(room: room, membership: .left, name: "general")])
        #expect(port.leavePresences == [room])
        #expect(coordinator.directory.channels.isEmpty)
        #expect(coordinator.directory.leftRooms.map(\.roomJID) == [room])
        #expect(coordinator.directory.title(for: roomConversation) == "general")
        #expect(!coordinator.presence.joinedRooms.contains(room))
        #expect(coordinator.unread.count(for: roomConversation) == 0)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .never)
    }

    @Test func aRefusedLeaveKeepsTheRoom() async throws {
        let (coordinator, port) = online()
        enter(coordinator)
        port.membershipWriteResult = .failure(.rejected)

        await #expect(throws: PortError.rejected) { try await coordinator.leaveRoom(room) }

        #expect(port.leavePresences.isEmpty)
        #expect(coordinator.directory.channels.map(\.roomJID) == [room])
        #expect(coordinator.directory.leftRooms.isEmpty)
        #expect(coordinator.presence.joinedRooms.contains(room))
    }

    @Test func aLeftRoomIsNotJoinedAgain() async throws {
        let (coordinator, port) = online()
        try await coordinator.leaveRoom(room)
        port.topology = Topology(spaces: [], channels: [general.with(hasLeft: true)])

        await coordinator.refreshDirectory()
        await coordinator.open(roomConversation)
        await coordinator.openThread(ThreadKey(room: room, threadID: "thread"))

        #expect(port.joined.isEmpty)
        #expect(coordinator.directory.leftRooms.map(\.roomJID) == [room])
    }

    @Test func aRefreshAnsweredBeforeTheLeaveDoesNotBringTheRoomBack() async throws {
        let (coordinator, port) = online()
        port.holdsTopology = true
        let refresh = Task { await coordinator.refreshDirectory() }
        while port.topologyRequests == 0 { await Task.yield() }

        try await coordinator.leaveRoom(room)
        port.releaseTopology()
        await refresh.value

        #expect(port.joined.isEmpty)
        #expect(coordinator.directory.channels.isEmpty)
        #expect(coordinator.directory.leftRooms.map(\.roomJID) == [room])
    }

    @Test func unreadCountsOfALeftRoomAreIgnored() async throws {
        let (coordinator, _) = online()
        try await coordinator.leaveRoom(room)

        coordinator.handle(.inboxPush(inboxRow(unread: 4)))
        coordinator.handle(.inboxPush(InboxEntry(
            partner: room, kind: .room, lastStanzaID: "s2", lastUpdated: 2, unread: 2, preview: nil, thread: InboxEntry.Thread(id: "thread", title: nil))))

        #expect(coordinator.unread.total == 0)
        #expect(coordinator.unread.threadCounts.isEmpty)
        #expect(coordinator.overviewCandidates().isEmpty)
    }

    @Test func rejoiningRecordsItListsTheRoomAndJoins() async throws {
        let (coordinator, port) = online([general.with(hasLeft: true)])
        coordinator.directory.setNotifyMode(.never, for: roomConversation)

        try await coordinator.rejoinRoom(room)

        #expect(port.membershipWrites == [.init(room: room, membership: .joined(notifyMode: .onMention), name: "general")])
        #expect(coordinator.directory.channels.map(\.roomJID) == [room])
        #expect(coordinator.directory.leftRooms.isEmpty)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .onMention)
        #expect(port.joined == [room])

        coordinator.handle(.inboxPush(inboxRow(unread: 4)))
        #expect(coordinator.unread.count(for: roomConversation) == 4)
    }

    @Test func rejoiningARoomThatIsGoneNeitherRecordsNorJoins() async throws {
        // A join to a room that does not exist creates it.
        let (coordinator, port) = online([general.with(hasLeft: true)])
        port.topology = .empty

        await #expect(throws: RoomRejoinError.roomIsGone) { try await coordinator.rejoinRoom(room) }

        #expect(port.membershipWrites.isEmpty)
        #expect(port.joined.isEmpty)
        #expect(coordinator.directory.leftRooms.isEmpty)
    }

    @Test func rejoiningWhenTheRoomsCannotBeLookedUpFails() async throws {
        let (coordinator, port) = online([general.with(hasLeft: true)])
        port.topologyFailure = .timeout

        await #expect(throws: PortError.failed) { try await coordinator.rejoinRoom(room) }

        #expect(port.membershipWrites.isEmpty)
        #expect(port.joined.isEmpty)
        #expect(coordinator.directory.leftRooms.map(\.roomJID) == [room])
    }

    @Test func aRoomLeftOnAnotherDeviceIsLeftHere() async throws {
        let (coordinator, port) = online()
        enter(coordinator)
        coordinator.unread.set(2, for: roomConversation)
        port.topology = Topology(spaces: [], channels: [general.with(hasLeft: true)])

        await coordinator.refreshDirectory(joiningOnlyNewRooms: true)

        #expect(port.leavePresences == [room])
        #expect(!coordinator.presence.joinedRooms.contains(room))
        #expect(coordinator.unread.count(for: roomConversation) == 0)
        #expect(coordinator.directory.leftRooms.map(\.roomJID) == [room])
    }

    @Test func leftRoomsStayLeftWhenTheAccountsBookmarksCannotBeRead() async throws {
        let (coordinator, port) = online([general.with(hasLeft: true)])
        port.topology = Topology(spaces: [], channels: [general], ownBookmarksLoaded: false)

        await coordinator.refreshDirectory()

        #expect(port.joined.isEmpty)
        #expect(coordinator.directory.leftRooms.map(\.roomJID) == [room])
    }

    @Test func aRoomIsNotTakenAsLeftElsewhereWhenBookmarksCannotBeRead() async throws {
        let (coordinator, port) = online()
        enter(coordinator)
        port.topology = Topology(spaces: [], channels: [general], ownBookmarksLoaded: false)

        await coordinator.refreshDirectory(joiningOnlyNewRooms: true)

        #expect(port.leavePresences.isEmpty)
        #expect(coordinator.directory.channels.map(\.roomJID) == [room])
    }

    @Test func aLeaveWaitsForANotificationChangeAlreadyBeingSaved() async throws {
        // Both rewrite the same bookmark; overlapping, one would undo the other.
        let (coordinator, port) = online()
        port.heldDirectoryVerbs = ["setNotifyMode"]
        let mute = Task { try await coordinator.setNotifyMode(.never, for: roomConversation) }
        while port.notifySettingsWrites.isEmpty { await Task.yield() }

        let leave = Task { try await coordinator.leaveRoom(room) }
        for _ in 0..<20 { await Task.yield() }
        #expect(port.membershipWrites.isEmpty)

        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        try await mute.value
        try await leave.value
        #expect(port.membershipWrites.map(\.membership) == [.left])
    }

    @Test func leftRoomsKnownOnlyFromBookmarksStayLeftWhenBookmarksCannotBeRead() async throws {
        // Such a room is not in a lookup that could not read the bookmarks.
        let (coordinator, port) = online([general.with(hasLeft: true)])
        port.topology = Topology(spaces: [], channels: [], ownBookmarksLoaded: false)

        await coordinator.refreshDirectory()
        coordinator.handle(.inboxPush(inboxRow(unread: 4)))

        #expect(coordinator.directory.leftRooms.map(\.roomJID) == [room])
        #expect(coordinator.unread.total == 0)
        #expect(port.joined.isEmpty)
    }

    @Test func aRenameSavedDuringALookupDoesNotBringALeftRoomBack() async throws {
        // The lookup finds the room left on another device; the rename
        // made here meanwhile knows nothing of that.
        let (coordinator, port) = online()
        port.topology = Topology(spaces: [], channels: [general.with(hasLeft: true)])
        port.holdsTopology = true
        let refresh = Task { await coordinator.refreshDirectory(joiningOnlyNewRooms: true) }
        while port.topologyRequests == 0 { await Task.yield() }

        try await coordinator.updateRoom(room, with: RoomSettingsPatch(name: "renamed"))
        port.releaseTopology()
        await refresh.value

        #expect(coordinator.directory.channels.isEmpty)
        #expect(coordinator.directory.leftRooms.map(\.name) == ["renamed"])
        #expect(port.joined.isEmpty)
    }

    @Test func aLeaveThatTimedOutButWasRecordedCountsAsLeft() async throws {
        let (coordinator, port) = online()
        enter(coordinator)
        port.membershipWriteResult = .failure(.timeout)
        port.topology = Topology(spaces: [], channels: [general.with(hasLeft: true)])

        try await coordinator.leaveRoom(room)

        #expect(coordinator.directory.leftRooms.map(\.roomJID) == [room])
        #expect(port.leavePresences == [room])
        #expect(!coordinator.presence.joinedRooms.contains(room))
    }

    @Test func aLeaveThatTimedOutAndWasNotRecordedFails() async throws {
        let (coordinator, port) = online()
        enter(coordinator)
        port.membershipWriteResult = .failure(.timeout)

        await #expect(throws: PortError.timeout) { try await coordinator.leaveRoom(room) }

        #expect(coordinator.directory.channels.map(\.roomJID) == [room])
        #expect(coordinator.presence.joinedRooms.contains(room))
    }

    @Test func aNotificationChangeWaitingBehindALeaveFailsAndIsUndone() async throws {
        // Stored after the leave it would turn the left room's notifications on.
        let (coordinator, port) = online()
        port.heldDirectoryVerbs = ["setRoomMembership"]
        let leave = Task { try await coordinator.leaveRoom(room) }
        while port.heldDirectoryRequests.isEmpty { await Task.yield() }
        let change = Task { try await coordinator.setNotifyMode(.always, for: roomConversation) }
        for _ in 0..<20 { await Task.yield() }

        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        try await leave.value
        await #expect(throws: PortError.invalidRequest) { try await change.value }

        #expect(port.notifySettingsWrites.isEmpty)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .never)
    }

    @Test func notificationSettingsOfALeftRoomCannotBeChanged() async throws {
        // Its details can still be open when another device leaves it.
        let (coordinator, port) = online([general.with(hasLeft: true)])
        coordinator.directory.setNotifyMode(.never, for: roomConversation)

        await #expect(throws: PortError.invalidRequest) {
            try await coordinator.setNotifyMode(.always, for: roomConversation)
        }
        await #expect(throws: PortError.invalidRequest) {
            try await coordinator.setShowsMessagesInPush(true, for: roomConversation)
        }

        #expect(port.notifySettingsWrites.isEmpty)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .never)
        #expect(!coordinator.directory.showsMessagesInPush(for: roomConversation))
    }

    @Test func rejoiningNeedsTheRoomFoundAfreshWhenBookmarksCannotBeRead() async throws {
        // A left room is kept through a lookup that could not read the
        // bookmarks, so being known proves nothing: the room may be gone,
        // and joining it would create it.
        let (coordinator, port) = online([general.with(hasLeft: true)])
        port.topology = Topology(spaces: [], channels: [], ownBookmarksLoaded: false)

        await #expect(throws: PortError.failed) { try await coordinator.rejoinRoom(room) }

        #expect(port.membershipWrites.isEmpty)
        #expect(port.joined.isEmpty)
        #expect(coordinator.directory.leftRooms.map(\.roomJID) == [room])
    }

    @Test func aRoomLeftElsewhereWhileBeingEditedIsLeftOnceTheEditSettles() async throws {
        let (coordinator, port) = online()
        enter(coordinator)
        port.heldDirectoryVerbs = ["updateRoomSettings"]
        let edit = Task { try await coordinator.updateRoom(room, with: RoomSettingsPatch(name: "renamed")) }
        while port.heldDirectoryRequests.isEmpty { await Task.yield() }
        port.topology = Topology(spaces: [], channels: [general.with(hasLeft: true)])

        await coordinator.refreshDirectory(joiningOnlyNewRooms: true)
        #expect(port.leavePresences.isEmpty)

        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        try await edit.value

        #expect(port.leavePresences == [room])
        #expect(!coordinator.presence.joinedRooms.contains(room))
        #expect(coordinator.directory.leftRooms.map(\.name) == ["renamed"])
    }

    @Test func noHuddleStartsWhileARoomLeftElsewhereIsBeingLeft() async throws {
        let (coordinator, port) = online()
        enter(coordinator)
        port.topology = Topology(spaces: [], channels: [general.with(hasLeft: true)])
        port.heldDirectoryVerbs = ["leaveRoom"]
        let refresh = Task { await coordinator.refreshDirectory(joiningOnlyNewRooms: true) }
        while port.heldDirectoryRequests.isEmpty { await Task.yield() }

        // Still an occupant: the leave presence is on its way.
        #expect(coordinator.presence.joinedRooms.contains(room))
        #expect(coordinator.isFenced(room))
        #expect(!coordinator.canJoinGroupCall(in: room))

        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        await refresh.value
        #expect(!coordinator.canJoinGroupCall(in: room))
    }

    @Test func noHuddleStartsInALeftRoom() {
        let (coordinator, _) = online([general.with(hasLeft: true)])
        enter(coordinator)
        #expect(!coordinator.canJoinGroupCall(in: room))
    }

    @Test func aJoinAnsweredAfterTheRoomWasFoundLeftIsFollowedByALeave() async throws {
        // The join was sent before a lookup found the room left on another
        // device; the room lets us in only afterwards.
        let (coordinator, port) = online([general.with(hasLeft: true)])

        coordinator.handle(.presence(WirePresence(
            from: jid("general@muc.waddle.test/alice"), kind: .available,
            occupant: .init(affiliation: .member, role: .participant, realJID: nil, statusCodes: [110]))))
        for _ in 0..<200 where port.leavePresences.isEmpty { await Task.yield() }

        #expect(port.leavePresences == [room])
        #expect(!coordinator.presence.joinedRooms.contains(room))
    }

    @Test func aRoomRejoinedOnAnotherDeviceGetsItsNotificationSettingsBack() async throws {
        let (coordinator, port) = online([general.with(hasLeft: true)])
        coordinator.directory.setNotifyMode(.never, for: roomConversation)
        port.notifySettings = NotifySettings(modes: [roomConversation: .always], showingMessagesInPush: [roomConversation])
        port.topology = Topology(spaces: [], channels: [general])

        try await coordinator.rejoinRoom(room)

        #expect(port.membershipWrites.isEmpty)
        #expect(coordinator.directory.channels.map(\.roomJID) == [room])
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .always)
        #expect(coordinator.directory.showsMessagesInPush(for: roomConversation))
    }

    @Test func aRoomRejoinedOnAnotherDeviceWithDefaultNotificationsIsNoLongerMuted() async throws {
        let (coordinator, port) = online([general.with(hasLeft: true)])
        coordinator.directory.setNotifyMode(.never, for: roomConversation)
        port.topology = Topology(spaces: [], channels: [general])

        await coordinator.refreshDirectory()

        #expect(coordinator.directory.notifyModes[roomConversation] == nil)
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .onMention)
    }

    @Test func settingsReadForARejoinedRoomDoNotUndoAMuteMadeMeanwhile() async throws {
        let (coordinator, port) = online([general.with(hasLeft: true)])
        coordinator.directory.setNotifyMode(.never, for: roomConversation)
        port.topology = Topology(spaces: [], channels: [general])
        port.heldDirectoryVerbs = ["fetchNotifySettings"]
        let refresh = Task { await coordinator.refreshDirectory() }
        while port.heldDirectoryRequests.isEmpty { await Task.yield() }

        // The room is listed again, and the user mutes it before the
        // settings asked for above have come back.
        port.heldDirectoryVerbs = []
        let mute = Task { try await coordinator.setNotifyMode(.never, for: roomConversation) }
        for _ in 0..<50 { await Task.yield() }
        port.releaseDirectoryRequests()
        await refresh.value
        try await mute.value

        #expect(coordinator.directory.notifyMode(for: roomConversation) == .never)
        #expect(port.notifySettings.modes[roomConversation] == .never)
    }

    @Test func settingsOfSeveralRejoinedRoomsAreReadTogetherAndDoNotHoldUpJoins() async throws {
        let other = Channel(roomJID: bare("random@muc.waddle.test"), name: "random")
        let (coordinator, port) = online([general.with(hasLeft: true), other.with(hasLeft: true)])
        port.topology = Topology(spaces: [], channels: [general, other])
        port.heldDirectoryVerbs = ["fetchNotifySettings"]
        let refresh = Task { await coordinator.refreshDirectory() }
        while port.heldDirectoryRequests.count < 2 { await Task.yield() }

        // Both reads are out at once, and the rooms are joined already.
        #expect(Set(port.joined) == [room, other.roomJID])

        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        await refresh.value
    }

    @Test func groupChatsAreNeverListedAsLeft() {
        let (coordinator, _) = online([Channel(roomJID: room, name: "group", isGroupDM: true, hasLeft: true)])
        #expect(coordinator.directory.leftRooms.isEmpty)
        #expect(coordinator.directory.groupDMs.map(\.roomJID) == [room])
    }
}
