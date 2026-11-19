import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Room management")
struct RoomManagementTests {
    private func online(_ channel: Channel = Channel(roomJID: room, name: "general")) -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.directory.apply(Topology(spaces: [], channels: [channel]))
        return (coordinator, port)
    }

    @Test func fetchesRoomSettingsWithoutChangingDirectory() async throws {
        let (coordinator, port) = online()
        port.roomSettingsResult = .success(RoomSettings(name: "Server name", summary: nil, pinPermission: .anyone))
        let settings = try await coordinator.roomSettings(of: room)
        #expect(settings == RoomSettings(name: "Server name", summary: nil, pinPermission: .anyone))
        #expect(port.roomSettingsRequests == [room])
        #expect(coordinator.directory.title(for: roomConversation) == "general")
    }
    @Test func updatesRoomSettingsAndPreservesChannelMetadata() async throws {
        let (coordinator, port) = online(Channel(
            roomJID: room, name: "general", summary: "Old summary", kind: .forum,
            position: 7, spaceID: "space", autojoin: false))
        let patch = RoomSettingsPatch(name: "New name", summary: "", pinPermission: .adminsOnly)
        try await coordinator.updateRoom(room, with: patch)
        #expect(port.roomSettingsUpdates.map(\.room) == [room])
        #expect(port.roomSettingsUpdates.map(\.patch) == [patch])
        #expect(coordinator.directory.channel(for: room) == Channel(
            roomJID: room, name: "New name", summary: "", kind: .forum,
            position: 7, spaceID: "space", autojoin: false))
    }

    @Test func emptyPatchDoesNotSendARequest() async throws {
        let (coordinator, port) = online()
        try await coordinator.updateRoom(room, with: RoomSettingsPatch())
        #expect(port.roomSettingsUpdates.isEmpty)
        #expect(coordinator.directory.channel(for: room) == Channel(roomJID: room, name: "general"))
    }

    @Test func deletionForgetsTheRoomAfterServerSuccess() async throws {
        let (coordinator, port) = online()
        _ = coordinator.presence.apply(WirePresence(
            from: jid("general@muc.waddle.test/alice"), kind: .available,
            occupant: .init(affiliation: .owner, role: .moderator, realJID: nil, statusCodes: [110])))
        coordinator.directory.setNotifyMode(.never, for: roomConversation)
        try await coordinator.deleteRoom(room)
        #expect(port.roomDestructions.map(\.room) == [room])
        #expect(port.roomDestructions.first?.reason == nil)
        #expect(coordinator.directory.channel(for: room) == nil)
        #expect(!coordinator.directory.isRoom(room))
        #expect(coordinator.directory.notifyModes[roomConversation] == nil)
        #expect(!coordinator.presence.joinedRooms.contains(room))
        #expect(coordinator.presence.occupants[room] == nil)
    }

    @Test func openingADeletedRoomOrThreadNeverJoinsIt() async throws {
        let (coordinator, port) = online()
        try await coordinator.deleteRoom(room)
        await coordinator.open(roomConversation)
        await coordinator.openThread(ThreadKey(room: room, threadID: "thread"))
        #expect(port.joined.isEmpty)
        #expect(coordinator.directory.channel(for: room) == nil)
    }

    @Test func renamesGroupDMAndPreservesMetadata() async throws {
        let (coordinator, port) = online(Channel(
            roomJID: room, name: "Old group", summary: "Keep", kind: .text,
            position: 4, spaceID: "space", autojoin: false, isGroupDM: true))
        try await coordinator.renameGroupDM(room, to: "  Friends\n")
        #expect(port.groupDMRenames.map(\.room) == [room])
        #expect(port.groupDMRenames.map(\.name) == ["Friends"])
        #expect(coordinator.directory.channel(for: room) == Channel(
            roomJID: room, name: "Friends", summary: "Keep", kind: .text,
            position: 4, spaceID: "space", autojoin: false, isGroupDM: true))
    }

    @Test func clearingGroupNameSendsNilAndUsesFallbackTitle() async throws {
        let (coordinator, port) = online(Channel(roomJID: room, name: "Friends", isGroupDM: true))
        try await coordinator.renameGroupDM(room, to: " \n")
        #expect(port.groupDMRenames.map(\.name) == [nil])
        #expect(coordinator.directory.title(for: roomConversation) == "general")
        #expect(coordinator.directory.channel(for: room)?.isGroupDM == true)
    }

    @Test(arguments: [GroupDMHistoryAccess.fromJoin, .full])
    func invitesToGroupDMWithRequestedHistory(history: GroupDMHistoryAccess) async throws {
        let (coordinator, port) = online(Channel(roomJID: room, name: "Friends", isGroupDM: true))
        let invitee = bare("bob@waddle.test")
        try await coordinator.inviteToGroupDM(room, invitee: invitee, history: history)
        #expect(port.groupDMInvitations.map(\.room) == [room])
        #expect(port.groupDMInvitations.map(\.invitee) == [bare("bob@waddle.test")])
        #expect(port.groupDMInvitations.map(\.history) == [history])
        #expect(coordinator.directory.groupDMs == [Channel(roomJID: room, name: "Friends", isGroupDM: true)])
    }

    @Test func leavesGroupDMAndForgetsItAfterServerSuccess() async throws {
        let (coordinator, port) = online(Channel(roomJID: room, name: "Friends", isGroupDM: true))
        _ = coordinator.presence.apply(WirePresence(
            from: jid("general@muc.waddle.test/alice"), kind: .available,
            occupant: .init(affiliation: .member, role: .participant, realJID: nil, statusCodes: [110])))
        coordinator.directory.setNotifyMode(.never, for: roomConversation)
        try await coordinator.leaveGroupDM(room)
        #expect(port.groupDMLeaves == [room])
        #expect(coordinator.directory.groupDMs.isEmpty)
        #expect(!coordinator.directory.isRoom(room))
        #expect(coordinator.directory.notifyModes[roomConversation] == nil)
        #expect(!coordinator.presence.joinedRooms.contains(room))
    }

    @Test(arguments: ["fetch", "update", "delete", "rename", "invite", "leave"])
    func failuresPreserveDirectoryPresenceAndOnDemandRejoin(action: String) async throws {
        let channel = Channel(roomJID: room, name: "Original", summary: "Keep", isGroupDM: true)
        let (coordinator, port) = online(channel)
        port.roomSettingsResult = .failure(.rejected)
        port.roomSettingsUpdateResult = .failure(.rejected)
        port.roomDestructionResult = .failure(.rejected)
        port.groupDMRenameResult = .failure(.rejected)
        port.groupDMInvitationResult = .failure(.rejected)
        port.groupDMLeaveResult = .failure(.rejected)
        _ = coordinator.presence.apply(WirePresence(
            from: jid("general@muc.waddle.test/alice"), kind: .available,
            occupant: .init(affiliation: .member, role: .participant, realJID: nil, statusCodes: [110])))
        coordinator.directory.setNotifyMode(.never, for: roomConversation)
        await coordinator.open(roomConversation)
        await #expect(throws: PortError.rejected) {
            switch action {
            case "fetch": _ = try await coordinator.roomSettings(of: room)
            case "update": try await coordinator.updateRoom(room, with: RoomSettingsPatch(name: "Changed"))
            case "delete": try await coordinator.deleteRoom(room)
            case "rename": try await coordinator.renameGroupDM(room, to: "Changed")
            case "invite": try await coordinator.inviteToGroupDM(room, invitee: bare("bob@waddle.test"), history: .full)
            default: try await coordinator.leaveGroupDM(room)
            }
        }
        #expect(coordinator.directory.groupDMs == [Channel(roomJID: room, name: "Original", summary: "Keep", isGroupDM: true)])
        #expect(coordinator.directory.isRoom(room))
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .never)
        #expect(coordinator.presence.joinedRooms.contains(room))
        #expect(coordinator.selfOccupant(in: room)?.nick == "alice")
        coordinator.start()
        port.emit(.connected)
        await eventually { port.topologyRequests == 1 }
        await coordinator.readyTask?.value
        #expect(port.joined == [room])
        await coordinator.stop()
    }

    @Test(arguments: [false, true])
    func editsPreserveDirectoryPositionOrder(group: Bool) async throws {
        let (coordinator, _) = online()
        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "First", position: 0, isGroupDM: group),
            Channel(roomJID: bare("second@muc.waddle.test"), name: "Second", position: 1, isGroupDM: group),
        ]))
        if group { try await coordinator.renameGroupDM(room, to: "Renamed") }
        else { try await coordinator.updateRoom(room, with: RoomSettingsPatch(pinPermission: .anyone)) }
        let listed = group ? coordinator.directory.groupDMs : coordinator.directory.channels
        #expect(listed.map(\.roomJID) == [room, bare("second@muc.waddle.test")])
    }

}
