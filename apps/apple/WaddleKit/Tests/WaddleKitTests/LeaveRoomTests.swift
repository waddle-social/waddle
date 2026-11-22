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
        #expect(port.left == [room])
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

        #expect(port.left.isEmpty)
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

        #expect(port.left == [room])
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

        #expect(port.left.isEmpty)
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

    @Test func groupChatsAreNeverListedAsLeft() {
        let (coordinator, _) = online([Channel(roomJID: room, name: "group", isGroupDM: true, hasLeft: true)])
        #expect(coordinator.directory.leftRooms.isEmpty)
        #expect(coordinator.directory.groupDMs.map(\.roomJID) == [room])
    }
}
