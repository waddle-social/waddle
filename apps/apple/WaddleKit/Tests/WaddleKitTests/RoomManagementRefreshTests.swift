import Observation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Room management refreshes")
struct RoomManagementRefreshTests {
    private func connected(
        group: Bool = false, autojoin: Bool = true, membersOnly: Bool? = nil
    ) async -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        port.topology = Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "Old name", autojoin: autojoin, isGroupDM: group, membersOnly: membersOnly),
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

    /// Another device made the room admins-only, and a lookup on its way
    /// read that. A pin permission saved here meanwhile is newer and
    /// stands; a rename made meanwhile carries only the old one, which
    /// must not undo the lookup's.
    @Test(arguments: [true, false])
    func aLookupOnItsWayKeepsASavedPinPermissionButNotAStaleOne(savesPinPermission: Bool) async throws {
        let (coordinator, port) = await connected()
        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "Old name", membersOnly: false, pinPermission: .anyone),
        ]))
        port.topology = Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "Old name", membersOnly: false, pinPermission: .adminsOnly),
        ])
        port.holdsTopology = true
        port.emit(.disconnected)
        port.emit(.connected)
        await eventually { port.heldTopology.count == 1 }
        try #require(port.heldTopology.count == 1)
        let patch = savesPinPermission ? RoomSettingsPatch(pinPermission: .anyone) : RoomSettingsPatch(name: "Friends")
        try await coordinator.updateRoom(room, with: patch)
        port.holdsTopology = false
        port.releaseTopology()
        await coordinator.readyTask?.value
        #expect(coordinator.directory.channel(for: room)?.pinPermission == (savesPinPermission ? .anyone : .adminsOnly))
        await coordinator.stop()
    }

    @Test func deletionWhileAnEarlierJoinWaitsSkipsTheDeletedRoom() async throws {
        let port = FakePort()
        let concurrency = SessionCoordinator.maxConcurrentAutoJoins
        let inFlight = (0..<concurrency).map { bare("first-\($0)@muc.waddle.test") }
        let channels = inFlight.enumerated().map { index, jid in
            Channel(roomJID: jid, name: "First \(index)", position: index)
        } + [Channel(roomJID: room, name: "Queued", position: concurrency)]
        port.topology = Topology(spaces: [], channels: channels)
        port.heldDirectoryVerbs = ["joinRoom"]
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        port.emit(.connected)
        await eventually { port.heldDirectoryRequests.count == concurrency }
        try #require(port.heldDirectoryRequests.count == concurrency)
        try await coordinator.deleteRoom(room)
        port.heldDirectoryVerbs = []
        port.releaseDirectoryRequests()
        await coordinator.readyTask?.value
        #expect(Set(port.joined) == Set(inFlight))
        #expect(!port.joined.contains(room))
        #expect(coordinator.directory.channel(for: room) == nil)
        await coordinator.stop()
    }

    @Test(arguments: [false, nil] as [Bool?])
    func aRenameDuringRefreshKeepsTheDiscoveredMembersOnlyDefault(membersOnly: Bool?) async throws {
        let (coordinator, port) = await connected(membersOnly: membersOnly)
        port.topology = Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "Old name", membersOnly: true),
        ])
        port.holdsTopology = true
        port.emit(.disconnected)
        port.emit(.connected)
        await eventually { port.heldTopology.count == 1 }
        try #require(port.heldTopology.count == 1)

        try await coordinator.updateRoom(room, with: RoomSettingsPatch(name: "Renamed"))
        port.holdsTopology = false
        port.releaseTopology()
        await coordinator.readyTask?.value

        #expect(coordinator.directory.title(for: roomConversation) == "Renamed")
        #expect(coordinator.directory.notifyMode(for: roomConversation) == .always)
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

    /// XEP-0045 §10.9: the presence the room sends each occupant it removes
    /// when its owner deletes it.
    private let destruction = WirePresence(
        from: room.with(resource: "alice")!, kind: .unavailable,
        occupant: .init(affiliation: RoomAffiliation.none, role: RoomRole.none, realJID: nil, statusCodes: [110], roomDestroyed: true))

    /// A room deleted on another device is gone, and joining it would
    /// create it again (#2538): it is forgotten as if deleted here, even
    /// while a stale directory still lists it.
    @Test func aRoomDeletedElsewhereIsForgottenAndNeverJoinedAgain() async throws {
        let (coordinator, port) = await connected()
        await coordinator.open(roomConversation)
        coordinator.unread.set(2, for: roomConversation)
        port.joined = []
        port.emit(.presence(destruction))
        await eventually { coordinator.directory.channel(for: room) == nil }
        #expect(!coordinator.directory.isRoom(room))
        #expect(!coordinator.presence.joinedRooms.contains(room))
        #expect(coordinator.unread.count(for: roomConversation) == 0)
        await coordinator.ensureJoined(room)
        await coordinator.open(roomConversation)
        await reconnect(coordinator, port)
        await coordinator.ensureJoined(room)
        #expect(port.joined.isEmpty)
        #expect(coordinator.directory.channel(for: room) == nil)
        #expect(!coordinator.canJoinGroupCall(in: room))
        await coordinator.stop()
    }

    /// A delete that got no answer here is settled by the room's own word.
    @Test func theRoomsDestructionSettlesAnUnansweredDelete() async throws {
        let (coordinator, port) = await connected()
        port.roomDestructionResult = .failure(.timeout)
        await #expect(throws: PortError.timeout) { try await coordinator.deleteRoom(room) }
        #expect(coordinator.directory.isRoom(room))
        port.emit(.presence(destruction))
        await eventually { !coordinator.directory.isRoom(room) }
        #expect(coordinator.directory.channel(for: room) == nil)
        await coordinator.stop()
    }

    @Test func aRoomDeletedElsewhereGetsNoHuddlePresenceAfterwards() async throws {
        let (coordinator, port) = await connected()
        port.emit(.presence(destruction))
        await eventually { !coordinator.directory.isRoom(room) }
        await coordinator.calls.leaveGroupPresence(in: room, nick: "alice", identity: nil, epoch: coordinator.calls.generation)
        #expect(port.callSends.isEmpty)
        await coordinator.stop()
    }

    /// The room says it is gone before its answer to our delete comes, and
    /// then that answer is lost: the delete happened all the same.
    @Test func aDeleteWhoseRoomSaysItIsGoneSucceedsWithoutItsAnswer() async throws {
        let (coordinator, port) = await connected()
        port.roomDestructionResult = .failure(.timeout)
        port.heldDirectoryVerbs = ["destroyRoom"]
        let deleting = Task { try await coordinator.deleteRoom(room) }
        await eventually { port.heldDirectoryRequests.count == 1 }
        coordinator.handle(.presence(destruction))
        port.releaseDirectoryRequests()
        try await deleting.value
        #expect(!coordinator.directory.isRoom(room))
        #expect(!coordinator.unconfirmedDeletions.contains(room))
        await coordinator.stop()
    }

    /// A destruction that arrives while the session stops leaves nothing
    /// behind for the next session, where the room may exist again.
    @Test func aDestructionHeardWhileStoppingIsNotKept() async throws {
        let (coordinator, _) = await connected()
        await coordinator.stop()
        coordinator.handle(.presence(destruction))
        #expect(!coordinator.destroyedRooms.contains(room))
        #expect(!coordinator.calls.sealedGroupRooms.contains(room))
    }

    /// An inbox answer the server sent before the room was deleted counts
    /// nothing for it afterwards.
    @Test func aDeletedRoomsInboxRowCountsNothing() async throws {
        let (coordinator, port) = await connected()
        port.emit(.presence(destruction))
        await eventually { !coordinator.directory.isRoom(room) }
        coordinator.handle(.inboxPush(InboxEntry(
            partner: room, kind: .room, lastStanzaID: "s1", lastUpdated: 1, unread: 3, preview: nil)))
        #expect(coordinator.unread.count(for: roomConversation) == 0)
        await coordinator.stop()
    }

    /// Only the room service speaks for a room. A presence from someone's
    /// own address that claims a destruction forgets nothing.
    @Test func aDestructionClaimedByAContactForgetsNothing() async throws {
        let (coordinator, port) = await connected()
        let claimed = WirePresence(
            from: jid("bob@waddle.test/phone"), kind: .unavailable,
            occupant: .init(affiliation: RoomAffiliation.none, role: RoomRole.none, realJID: nil, statusCodes: [], roomDestroyed: true))
        port.emit(.presence(claimed))
        port.emit(.presence(destruction))
        await eventually { !coordinator.directory.isRoom(room) }
        #expect(!coordinator.destroyedRooms.contains(bob))
        await coordinator.stop()
    }

    /// The app closes a deleted room it has open by watching the rooms
    /// deleted this session; a deletion on another device must reach it.
    @Test func aRoomDeletedElsewhereReachesThoseWatchingDeletedRooms() async throws {
        let (coordinator, port) = await connected()
        let watcher = DeletedRoomsWatcher(coordinator)
        port.emit(.presence(destruction))
        await eventually { watcher.changes > 0 }
        #expect(watcher.changes > 0)
        #expect(coordinator.destroyedRooms == [room])
        await coordinator.stop()
    }

    /// Room addresses come from their names, so an owner can delete a room
    /// and make it again at the same address. A lookup that began after
    /// the deletion and found the room answering for itself lists it again.
    @Test func aRoomMadeAgainAfterItsDeletionIsListedAndJoinedAgain() async throws {
        let (coordinator, port) = await connected()
        port.emit(.presence(destruction))
        await eventually { !coordinator.directory.isRoom(room) }
        port.joined = []
        port.topology = Topology(spaces: [], channels: [Channel(roomJID: room, name: "Old name", membersOnly: false)])
        await reconnect(coordinator, port)
        #expect(coordinator.directory.isRoom(room))
        #expect(port.joined == [room])
        #expect(!coordinator.isFenced(room))
        #expect(!coordinator.calls.sealedGroupRooms.contains(room))
        await coordinator.stop()
    }

    /// A lookup already on its way when the room was deleted read a room
    /// that no longer exists: it does not bring it back.
    @Test func aLookupOnItsWayWhenTheRoomWasDeletedDoesNotBringItBack() async throws {
        let (coordinator, port) = await connected()
        port.topology = Topology(spaces: [], channels: [Channel(roomJID: room, name: "Old name", membersOnly: false)])
        port.holdsTopology = true
        port.emit(.disconnected)
        port.emit(.connected)
        await eventually { port.heldTopology.count == 1 }
        try #require(port.heldTopology.count == 1)
        coordinator.handle(.presence(destruction))
        port.joined = []
        port.holdsTopology = false
        port.releaseTopology()
        await coordinator.readyTask?.value
        #expect(!coordinator.directory.isRoom(room))
        #expect(port.joined.isEmpty)
        #expect(coordinator.destroyedRooms.contains(room))
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

/// Counts changes to the rooms deleted this session, as a SwiftUI view
/// watching them would see them.
@MainActor
private final class DeletedRoomsWatcher {
    private let coordinator: SessionCoordinator
    private(set) var changes = 0

    init(_ coordinator: SessionCoordinator) {
        self.coordinator = coordinator
        track()
    }

    private func track() {
        withObservationTracking { _ = coordinator.destroyedRooms } onChange: { [self] in
            MainActor.assumeIsolated {
                changes += 1
                track()
            }
        }
    }
}
