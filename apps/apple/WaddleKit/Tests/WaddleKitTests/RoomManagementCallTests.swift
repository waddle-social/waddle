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

    /// Leaving a huddle announces itself in the room. Sent to a room that
    /// is already gone, that presence would create the room again.
    @Test(arguments: ["delete", "leave"])
    func theHuddleIsLeftBeforeTheRoomIsGivenUp(action: String) async throws {
        let (coordinator, port) = try await inCall(group: action == "leave")
        port.callSends = []
        port.heldCallVerbs = ["groupPresence"]
        let forgetting = Task {
            if action == "delete" { try await coordinator.deleteRoom(groupRoom) }
            else { try await coordinator.leaveGroupDM(groupRoom) }
        }
        await eventually { !port.heldCallSends.isEmpty }
        #expect(port.callSends.first == .groupPresence(groupRoom, "alice", .init(state: .left)))
        #expect(port.roomDestructions.isEmpty)
        #expect(port.groupDMLeaves.isEmpty)
        #expect(!coordinator.canJoinGroupCall(in: groupRoom))
        #expect(coordinator.directory.channel(for: groupRoom) != nil)
        port.releaseCallSends()
        try await forgetting.value
        #expect(port.roomDestructions.count + port.groupDMLeaves.count == 1)
        #expect(coordinator.calls.state == .idle)
        #expect(coordinator.directory.channel(for: groupRoom) == nil)
        #expect(!coordinator.presence.joinedRooms.contains(groupRoom))
        coordinator.calls.clear()
    }
    @Test(arguments: ["delete", "leave"])
    func aRefusedActionKeepsTheRoomButNotItsHuddle(action: String) async throws {
        let (coordinator, port) = try await inCall(group: action == "leave")
        port.roomDestructionResult = .failure(.rejected)
        port.groupDMLeaveResult = .failure(.rejected)
        await #expect(throws: PortError.rejected) {
            if action == "delete" { try await coordinator.deleteRoom(groupRoom) }
            else { try await coordinator.leaveGroupDM(groupRoom) }
        }
        #expect(!coordinator.calls.state.isLive)
        #expect(coordinator.directory.channel(for: groupRoom)?.name == "Call room")
        #expect(coordinator.presence.joinedRooms.contains(groupRoom))
        #expect(coordinator.canJoinGroupCall(in: groupRoom) || coordinator.calls.state != .idle)
        coordinator.calls.clear()
    }

    /// A hang-up frees the call at once and sends its leave when the
    /// presence queue reaches it; a delete must wait for that send.
    @Test func aHangUpStillWaitingToSendIsFinishedBeforeTheRoomIsDeleted() async throws {
        let (coordinator, port) = try await inCall(group: false)
        port.callSends = []
        port.heldCallVerbs = ["groupPresence"]
        let hangingUp = Task { await coordinator.calls.hangUp() }
        await eventually { !port.heldCallSends.isEmpty }
        let deleting = Task { try await coordinator.deleteRoom(groupRoom) }
        for _ in 0..<50 { await Task.yield() }
        #expect(port.roomDestructions.isEmpty)
        port.heldCallVerbs = []
        port.releaseCallSends()
        await hangingUp.value
        try await deleting.value
        #expect(port.roomDestructions.map(\.room) == [groupRoom])
        coordinator.calls.clear()
    }
}
