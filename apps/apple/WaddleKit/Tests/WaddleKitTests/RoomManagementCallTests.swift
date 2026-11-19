import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Room management call cleanup")
struct RoomManagementCallTests {
    private func inCall(group: Bool) async throws -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: me.jid, nick: "alice", resource: "waddle-apple-1"), port: port)
        coordinator.status.connection = .online
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: groupRoom, name: "Call room", isGroupDM: group)]))
        _ = coordinator.presence.apply(WirePresence(
            from: groupRoom.with(resource: "alice")!, kind: .available,
            occupant: .init(affiliation: .member, role: .participant, realJID: callOwn, statusCodes: [110])))
        let joining = Task { await coordinator.joinGroupCall(in: groupRoom, video: false) }
        await eventually { port.callSends.count == 1 }
        coordinator.calls.handleGroupPresence(groupWire(nick: "alice", preparing: true, realJID: callOwn))
        await eventually { port.callSends.count == 3 }
        let sid = try #require(coordinator.calls.state.sid)
        coordinator.calls.handle(CallEvent(from: groupMixer, sid: sid, kind: .sessionAccept(join: groupJoin, media: callAudio)))
        #expect(await joining.value)
        return (coordinator, port)
    }

    @Test(arguments: ["delete", "leave"])
    func forgettingRoomLeavesItsCallBeforeRemovingDirectoryEntry(action: String) async throws {
        let (coordinator, port) = try await inCall(group: action == "leave")
        port.callSends = []
        port.heldCallVerbs = ["groupPresence"]
        let forgetting = Task {
            if action == "delete" { try await coordinator.deleteRoom(groupRoom) }
            else { try await coordinator.leaveGroupDM(groupRoom) }
        }
        await eventually { !port.heldCallSends.isEmpty || coordinator.directory.channel(for: groupRoom) == nil }
        #expect(coordinator.calls.state == .idle)
        #expect(!coordinator.canJoinGroupCall(in: groupRoom))
        #expect(!(await coordinator.joinGroupCall(in: groupRoom, video: false)))
        #expect(coordinator.directory.channel(for: groupRoom) != nil)
        #expect(port.callSends.first == .groupPresence(groupRoom, "alice", .init(state: .left)))
        port.releaseCallSends()
        try await forgetting.value
        #expect(coordinator.directory.channel(for: groupRoom) == nil)
        #expect(!coordinator.presence.joinedRooms.contains(groupRoom))
        coordinator.calls.clear()
    }
    @Test(arguments: ["delete", "leave"])
    func failedActionsKeepTheRoomCallActive(action: String) async throws {
        let (coordinator, port) = try await inCall(group: action == "leave")
        port.roomDestructionResult = .failure(.rejected)
        port.groupDMLeaveResult = .failure(.rejected)
        port.callSends = []
        await #expect(throws: PortError.rejected) {
            if action == "delete" { try await coordinator.deleteRoom(groupRoom) }
            else { try await coordinator.leaveGroupDM(groupRoom) }
        }
        #expect(coordinator.calls.state.isLive)
        #expect(coordinator.directory.channel(for: groupRoom)?.name == "Call room")
        #expect(coordinator.presence.joinedRooms.contains(groupRoom))
        #expect(port.callSends.isEmpty)
        await coordinator.calls.hangUp()
        coordinator.calls.clear()
    }

}
