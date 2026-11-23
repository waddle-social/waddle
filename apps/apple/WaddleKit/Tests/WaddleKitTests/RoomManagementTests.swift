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
            position: 7, spaceID: "space", autojoin: false, pinPermission: .adminsOnly))
    }

    /// The owner's change is what members of the room get from its
    /// disco#info on the next look; until then this device knows it.
    @Test func aSavedPinPermissionIsKnownAtOnce() async throws {
        let (coordinator, _) = online(Channel(roomJID: room, name: "general", pinPermission: .adminsOnly))
        try await coordinator.updateRoom(room, with: RoomSettingsPatch(name: "renamed"))
        #expect(coordinator.directory.channel(for: room)?.pinPermission == .adminsOnly)
        try await coordinator.updateRoom(room, with: RoomSettingsPatch(pinPermission: .anyone))
        #expect(coordinator.directory.channel(for: room)?.pinPermission == .anyone)
        #expect(coordinator.directory.channel(for: room)?.name == "renamed")
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


    /// The server tells us we are out of the room before it answers the
    /// delete, and a join in that gap would create the room again.
    @Test func aRoomBeingDeletedIsNotJoinedWhileTheRequestIsPending() async throws {
        let (coordinator, port) = online()
        port.heldDirectoryVerbs = ["destroyRoom"]
        let deleting = Task { try await coordinator.deleteRoom(room) }
        await eventually { port.heldDirectoryRequests.count == 1 }
        coordinator.presence.markLeft(room)
        await coordinator.ensureJoined(room)
        #expect(port.joined.isEmpty)
        port.releaseDirectoryRequests()
        try await deleting.value
        await coordinator.ensureJoined(room)
        #expect(port.joined.isEmpty)
    }

    @Test func aRefusedDeleteLetsTheRoomBeJoinedAgain() async throws {
        let (coordinator, port) = online()
        port.roomDestructionResult = .failure(.rejected)
        await #expect(throws: PortError.rejected) { try await coordinator.deleteRoom(room) }
        await coordinator.ensureJoined(room)
        #expect(port.joined == [room])
    }

    /// The shared client saves settings by reading the whole form and
    /// writing it back, so two saves at once would undo each other.
    @Test func aSecondActionOnARoomWaitsItsTurnByFailing() async throws {
        let (coordinator, port) = online()
        port.heldDirectoryVerbs = ["updateRoomSettings"]
        let first = Task { try await coordinator.updateRoom(room, with: RoomSettingsPatch(name: "First")) }
        await eventually { port.heldDirectoryRequests.count == 1 }
        await #expect(throws: PortError.failed) {
            try await coordinator.updateRoom(room, with: RoomSettingsPatch(summary: "Second"))
        }
        await #expect(throws: PortError.failed) { try await coordinator.deleteRoom(room) }
        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        try await first.value
        #expect(port.roomSettingsUpdates.map(\.patch) == [RoomSettingsPatch(name: "First")])
        #expect(port.roomDestructions.isEmpty)
        try await coordinator.updateRoom(room, with: RoomSettingsPatch(summary: "Second"))
        #expect(coordinator.directory.channel(for: room) == Channel(roomJID: room, name: "First", summary: "Second"))
    }

    @Test func aDeletedRoomCanBeCreatedAgain() async throws {
        let (coordinator, port) = online()
        try await coordinator.deleteRoom(room)
        let created = try await coordinator.createChannel(name: "General", summary: nil)
        #expect(created == roomConversation)
        #expect(coordinator.directory.channel(for: room)?.name == "General")
        #expect(port.joined == [room])
    }

    @Test func aStoppedSessionSendsNoInvitation() async throws {
        let (coordinator, port) = online(Channel(roomJID: room, name: "Group", isGroupDM: true))
        await coordinator.stop()
        await #expect(throws: PortError.notConnected) {
            try await coordinator.inviteToGroupDM(room, invitee: jid("bob@waddle.test").bare, history: .fromJoin)
        }
        #expect(port.groupDMInvitations.isEmpty)
    }

    /// A join already on its way must land before the delete, not after:
    /// after it, the server would create the room again.
    @Test func aJoinAlreadyOnItsWayIsFinishedBeforeTheRoomIsDeleted() async throws {
        let (coordinator, port) = online()
        port.heldDirectoryVerbs = ["joinRoom"]
        let joining = Task { await coordinator.ensureJoined(room) }
        await eventually { port.heldDirectoryRequests.count == 1 }
        let deleting = Task { try await coordinator.deleteRoom(room) }
        for _ in 0..<50 { await Task.yield() }
        #expect(port.roomDestructions.isEmpty)
        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        await joining.value
        try await deleting.value
        #expect(port.joined == [room])
        #expect(port.roomDestructions.map(\.room) == [room])
    }

    @Test func aDeletedRoomGetsNoHuddlePresenceAfterwards() async throws {
        let (coordinator, port) = online()
        try await coordinator.deleteRoom(room)
        await coordinator.calls.leaveGroupPresence(in: room, nick: "alice", identity: nil, epoch: coordinator.calls.generation)
        #expect(port.callSends.isEmpty)
    }

    @Test func aRefusedDeleteLeavesHuddlePresenceWorking() async throws {
        let (coordinator, port) = online()
        port.roomDestructionResult = .failure(.rejected)
        await #expect(throws: PortError.rejected) { try await coordinator.deleteRoom(room) }
        await coordinator.calls.leaveGroupPresence(in: room, nick: "alice", identity: nil, epoch: coordinator.calls.generation)
        #expect(port.callSends == [.groupPresence(room, "alice", .init(state: .left))])
    }

    @Test(arguments: ["delete", "leave"])
    func forgettingARoomClearsItsUnreadCounts(action: String) async throws {
        let (coordinator, _) = online(Channel(roomJID: room, name: "Room", isGroupDM: action == "leave"))
        let thread = ThreadKey(room: room, threadID: "t1")
        coordinator.unread.set(3, for: roomConversation)
        coordinator.unread.setThread(2, for: thread)
        if action == "delete" { try await coordinator.deleteRoom(room) } else { try await coordinator.leaveGroupDM(room) }
        #expect(coordinator.unread.count(for: roomConversation) == 0)
        #expect(coordinator.unread.threadCount(for: thread) == 0)
    }

    @Test func aDeleteTappedWhileOfflineIsNotSentAndLeavesTheRoomUsable() async throws {
        let (coordinator, port) = online()
        coordinator.status.connection = .connecting
        await #expect(throws: PortError.notConnected) { try await coordinator.deleteRoom(room) }
        #expect(port.roomDestructions.isEmpty)
        coordinator.status.connection = .online
        await coordinator.ensureJoined(room)
        #expect(port.joined == [room])
    }

    @Test func aConnectionLostWhileADeleteWaitsItsTurnSendsNothing() async throws {
        let (coordinator, port) = online()
        port.heldDirectoryVerbs = ["joinRoom"]
        let joining = Task { await coordinator.ensureJoined(room) }
        await eventually { port.heldDirectoryRequests.count == 1 }
        let deleting = Task { try await coordinator.deleteRoom(room) }
        for _ in 0..<50 { await Task.yield() }
        coordinator.status.connection = .connecting
        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        await joining.value
        await #expect(throws: PortError.notConnected) { try await deleting.value }
        #expect(port.roomDestructions.isEmpty)
        coordinator.status.connection = .online
        coordinator.presence.markLeft(room)
        port.joined = []
        await coordinator.ensureJoined(room)
        #expect(port.joined == [room])
        await coordinator.calls.leaveGroupPresence(in: room, nick: "alice", identity: nil, epoch: coordinator.calls.generation)
        #expect(port.callSends == [.groupPresence(room, "alice", .init(state: .left))])
    }
}
