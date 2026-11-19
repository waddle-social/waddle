import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Room management session lifetime")
struct RoomManagementLifecycleTests {
    @Test(arguments: ["updateRoomSettings", "destroyRoom", "renameGroupDM", "leaveGroupDM"])
    func oldResponsesCannotChangeARestartedSession(verb: String) async throws {
        let port = FakePort()
        let group = verb == "renameGroupDM" || verb == "leaveGroupDM"
        port.topology = Topology(spaces: [], channels: [Channel(roomJID: room, name: "Old session", isGroupDM: group)])
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        coordinator.handle(.connected)
        await eventually { coordinator.isSendReady }
        await coordinator.readyTask?.value
        port.heldDirectoryVerbs = [verb]
        let oldAction = Task {
            switch verb {
            case "updateRoomSettings": try await coordinator.updateRoom(room, with: RoomSettingsPatch(name: "Stale edit"))
            case "destroyRoom": try await coordinator.deleteRoom(room)
            case "renameGroupDM": try await coordinator.renameGroupDM(room, to: "Stale edit")
            default: try await coordinator.leaveGroupDM(room)
            }
        }
        await eventually { port.heldDirectoryRequests.count == 1 }
        defer { port.releaseDirectoryRequests() }
        try #require(port.heldDirectoryRequests.count == 1)
        await coordinator.stop()
        port.topology = Topology(spaces: [], channels: [Channel(roomJID: room, name: "New session", isGroupDM: group)])
        coordinator.start()
        await eventually { port.connectCount >= 2 }
        coordinator.handle(.connected)
        await eventually { port.topologyRequests == 2 }
        try #require(port.topologyRequests == 2)
        await coordinator.readyTask?.value
        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        try await oldAction.value
        #expect(coordinator.directory.channel(for: room)?.name == "New session")

        port.joined = []
        coordinator.handle(.disconnected)
        coordinator.handle(.connected)
        await eventually { port.topologyRequests == 3 }
        await coordinator.readyTask?.value
        #expect(coordinator.directory.channel(for: room)?.name == "New session")
        #expect(port.joined == [room])
        await coordinator.stop()
    }
    @Test(arguments: ["update", "delete", "rename", "leave"])
    func stoppedSessionRejectsMutatingActionsBeforeSending(action: String) async throws {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        await coordinator.stop()
        await #expect(throws: PortError.notConnected) {
            switch action {
            case "update": try await coordinator.updateRoom(room, with: RoomSettingsPatch(name: "Changed"))
            case "delete": try await coordinator.deleteRoom(room)
            case "rename": try await coordinator.renameGroupDM(room, to: "Changed")
            default: try await coordinator.leaveGroupDM(room)
            }
        }
        #expect(port.roomSettingsUpdates.isEmpty)
        #expect(port.roomDestructions.isEmpty)
        #expect(port.groupDMRenames.isEmpty)
        #expect(port.groupDMLeaves.isEmpty)
    }

}
