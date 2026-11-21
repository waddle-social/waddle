import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Room management refreshes")
struct RoomManagementRefreshTests {
    private func connected(group: Bool = false, autojoin: Bool = true) async -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        port.topology = Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "Old name", autojoin: autojoin, isGroupDM: group),
        ])
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        port.emit(.connected)
        await eventually { coordinator.isSendReady }
        await coordinator.readyTask?.value
        return (coordinator, port)
    }

    private func reconnect(_ coordinator: SessionCoordinator, _ port: FakePort) async {
        let requests = port.topologyRequests
        port.emit(.disconnected)
        port.emit(.connected)
        await eventually { port.topologyRequests > requests }
        await coordinator.readyTask?.value
    }

    @Test func deletedRoomIsNeitherListedNorRejoinedAfterReconnect() async throws {
        let (coordinator, port) = await connected()
        await coordinator.open(roomConversation)
        try await coordinator.deleteRoom(room)
        port.joined = []
        await reconnect(coordinator, port)
        #expect(coordinator.directory.channel(for: room) == nil)
        #expect(!coordinator.directory.isRoom(room))
        #expect(port.joined.isEmpty)
        await coordinator.stop()
    }
    @Test func staleRefreshCannotRestoreLeftGroupButLaterInvitationCan() async throws {
        let (coordinator, port) = await connected(group: true)
        await coordinator.open(roomConversation)
        port.holdsTopology = true
        port.emit(.disconnected)
        port.emit(.connected)
        await eventually { port.heldTopology.count == 1 }
        try #require(port.heldTopology.count == 1)
        try await coordinator.leaveGroupDM(room)
        port.joined = []
        port.holdsTopology = false
        port.releaseTopology()
        await coordinator.readyTask?.value
        #expect(coordinator.directory.groupDMs.isEmpty)
        #expect(!coordinator.directory.isRoom(room))
        #expect(port.joined.isEmpty)

        await reconnect(coordinator, port)
        #expect(coordinator.directory.groupDMs == [Channel(roomJID: room, name: "Old name", isGroupDM: true)])
        #expect(port.joined == [room])
        await coordinator.stop()
    }

    @Test(arguments: ["rename", "settings"])
    func staleRefreshKeepsSuccessfulEdits(action: String) async throws {
        let (coordinator, port) = await connected(group: action == "rename")
        port.holdsTopology = true
        port.emit(.disconnected)
        port.emit(.connected)
        await eventually { port.heldTopology.count == 1 }
        try #require(port.heldTopology.count == 1)
        if action == "rename" {
            try await coordinator.renameGroupDM(room, to: "Friends")
        } else {
            try await coordinator.updateRoom(room, with: RoomSettingsPatch(name: "Friends", summary: "Updated"))
        }
        port.holdsTopology = false
        port.releaseTopology()
        await coordinator.readyTask?.value
        #expect(coordinator.directory.title(for: roomConversation) == "Friends")
        if action == "settings" {
            #expect(coordinator.directory.channel(for: room)?.summary == "Updated")
        }
        await coordinator.stop()
    }

    @Test func deletionWhileAnEarlierJoinWaitsSkipsTheDeletedRoom() async throws {
        let port = FakePort()
        let first = bare("first@muc.waddle.test")
        port.topology = Topology(spaces: [], channels: [
            Channel(roomJID: first, name: "First", position: 0),
            Channel(roomJID: room, name: "Second", position: 1),
        ])
        port.heldDirectoryVerbs = ["joinRoom"]
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        port.emit(.connected)
        await eventually { port.heldDirectoryRequests.count == 1 }
        try #require(port.heldDirectoryRequests.count == 1)
        try await coordinator.deleteRoom(room)
        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        await coordinator.readyTask?.value
        #expect(port.joined == [bare("first@muc.waddle.test")])
        #expect(coordinator.directory.channel(for: room) == nil)
        await coordinator.stop()
    }

    /// A delete that times out may have happened, and joining a room that
    /// is gone creates it again. Nothing the client can ask settles it, so
    /// the room is not joined for the rest of the session, listed or not.
    @Test(arguments: [PortError.timeout, .notConnected, .failed], [true, false])
    func aDeleteWithNoClearAnswerKeepsTheRoomUnjoined(error: PortError, stillListed: Bool) async throws {
        let (coordinator, port) = await connected()
        await coordinator.open(roomConversation)
        port.joined = []
        port.roomDestructionResult = .failure(error)
        if !stillListed { port.topology = Topology(spaces: [], channels: []) }
        await #expect(throws: error) { try await coordinator.deleteRoom(room) }
        await coordinator.ensureJoined(room)
        await reconnect(coordinator, port)
        await coordinator.ensureJoined(room)
        #expect(port.joined.isEmpty)
        #expect(coordinator.directory.isRoom(room) == stillListed)
        #expect(!coordinator.canJoinGroupCall(in: room))
        await coordinator.stop()
    }

    /// A second try that is refused does not prove the room exists: the
    /// server refuses a delete of a room that is already gone.
    @Test func aRefusedRetryOfAnUnansweredDeleteKeepsTheRoomUnjoined() async throws {
        let (coordinator, port) = await connected()
        port.joined = []
        port.roomDestructionResult = .failure(.timeout)
        await #expect(throws: PortError.timeout) { try await coordinator.deleteRoom(room) }
        port.roomDestructionResult = .failure(.rejected)
        await #expect(throws: PortError.rejected) { try await coordinator.deleteRoom(room) }
        await coordinator.ensureJoined(room)
        #expect(port.joined.isEmpty)
        await coordinator.stop()
    }

    @Test func aRetryThatSucceedsAfterAnUnansweredDeleteForgetsTheRoom() async throws {
        let (coordinator, port) = await connected()
        port.roomDestructionResult = .failure(.timeout)
        await #expect(throws: PortError.timeout) { try await coordinator.deleteRoom(room) }
        port.roomDestructionResult = .success(())
        try await coordinator.deleteRoom(room)
        #expect(!coordinator.directory.isRoom(room))
        await coordinator.stop()
    }
}
