import Foundation

extension SessionCoordinator {
    /// Fetches the room settings offered by the server. Throws `PortError` on failure.
    public func roomSettings(of room: BareJID) async throws -> RoomSettings {
        try await port.fetchRoomSettings(of: room)
    }

    /// Saves room settings; empty patches send nothing. Throws `PortError` on failure.
    public func updateRoom(_ room: BareJID, with patch: RoomSettingsPatch) async throws {
        guard !patch.isEmpty else { return }
        try await managing(room) { generation in
            try await port.updateRoomSettings(patch, of: room)
            guard isCurrent(generation) else { return }
            updateChannel(room, name: patch.name, summary: patch.summary)
        }
    }

    /// Destroys the room and excludes it for this session. A huddle there
    /// is left first, and stays left if the server refuses. Throws
    /// `PortError` on failure.
    public func deleteRoom(_ room: BareJID) async throws {
        try await giveUp(room, .destroyed) { try await port.destroyRoom(room, reason: nil) }
    }

    /// Deletes a room from the admin console, with the same care: the
    /// owner of the community need not be in the room or hold a role in it.
    public func deleteAdminRoom(_ room: AdminRoom) async throws {
        try await giveUp(room.jid, .destroyed) { try await port.deleteAdminRoom(room.jid) }
    }

    /// Renames a group DM; whitespace-only names clear it. Throws `PortError` on failure.
    public func renameGroupDM(_ room: BareJID, to name: String) async throws {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        try await managing(room) { generation in
            try await port.renameGroupDM(room, name: trimmed.isEmpty ? nil : trimmed)
            guard isCurrent(generation) else { return }
            updateChannel(room, name: trimmed.isEmpty ? (room.localpart ?? room.description) : trimmed)
        }
    }

    /// Sends a group DM invitation. Throws `PortError` on send failure; refusals arrive later as message errors.
    public func inviteToGroupDM(_ room: BareJID, invitee: BareJID, history: GroupDMHistoryAccess) async throws {
        guard !isStopped else { throw PortError.notConnected }
        try await port.inviteToGroupDM(room, invitee: invitee, history: history)
    }

    /// Leaves a group DM and forgets it locally. A huddle there is left
    /// first, and stays left if the server refuses. Throws `PortError` on
    /// failure.
    public func leaveGroupDM(_ room: BareJID) async throws {
        try await giveUp(room, .forgotten) { try await port.leaveGroupDM(room) }
    }

    /// Leaves a room on every device of the account, until it rejoins:
    /// the account records that it left, with the room's notifications
    /// off, and this session leaves the room. The room stays known, among
    /// the rooms the account left, and the account keeps its role there.
    /// A huddle there is left first. Throws `PortError` on failure.
    public func leaveRoom(_ room: BareJID) async throws {
        guard let channel = directory.channel(for: room), !channel.isGroupDM else { throw PortError.invalidRequest }
        let nick = presence.selfNicks[room] ?? account.nick
        do {
            try await giveUp(room, .left) {
                try await afterEarlierBookmarkWrites(to: .room(room)) { [self] in
                    try await port.setRoomMembership(.left, of: room, name: channel.name)
                    // Marked before the next write to the bookmark gets
                    // its turn, so that one sees the room as left.
                    if let recorded = directory.channel(for: room) { markChannel(recorded, left: true) }
                }
                await port.leaveRoom(room, nick: nick)
            }
        } catch PortError.timeout {
            // The account may have recorded it all the same. Looking the
            // rooms up tells, and leaves the room if it did.
            await refreshDirectory(joiningOnlyNewRooms: true)
            guard directory.hasLeft(room) else { throw PortError.timeout }
        }
    }

    /// Rejoins a room the account left, with the room's default
    /// notifications. The rooms are looked up first: joining a room that
    /// was deleted meanwhile would create it. Throws `RoomRejoinError`
    /// when the room is gone, `PortError` otherwise.
    public func rejoinRoom(_ room: BareJID) async throws {
        guard !isStopped, status.connection == .online else { throw PortError.notConnected }
        guard let lookup = await refreshDirectory(joiningOnlyNewRooms: true) else { throw PortError.failed }
        guard lookup.rooms.contains(room) else {
            // A left room is kept through a lookup that could not read
            // the bookmarks, so only one that could says the room is gone.
            throw lookup.readOwnBookmarks ? RoomRejoinError.roomIsGone : PortError.failed
        }
        guard let channel = directory.channel(for: room), !isFenced(room) else { throw RoomRejoinError.roomIsGone }
        // Rejoined on another device meanwhile: the lookup joined it.
        guard channel.hasLeft else { return }
        try await managing(room) { generation in
            let mode = directory.defaultNotifyMode(for: .room(room))
            try await afterEarlierBookmarkWrites(to: .room(room)) { [port] in
                try await port.setRoomMembership(.joined(notifyMode: mode), of: room, name: channel.name)
            }
            guard isCurrent(generation) else { return }
            markChannel(channel, left: false)
            directory.setNotifyMode(mode, for: .room(room))
            await ensureJoined(room)
        }
    }

    /// Runs one management action on `room`. A second action on the same
    /// room fails instead of overlapping: the shared client saves settings
    /// by reading the whole form and writing it back, so two at once would
    /// undo each other.
    private func managing(_ room: BareJID, _ action: (_ generation: Int) async throws -> Void) async throws {
        // Offline, nothing would be sent, and a delete must be able to
        // tell "never sent" from "sent, no answer".
        guard !isStopped, status.connection == .online else { throw PortError.notConnected }
        guard roomsBeingManaged.insert(room).inserted else { throw PortError.failed }
        let generation = roomManagementGeneration
        let outcome: Result<Void, Error>
        do {
            try await action(generation)
            outcome = .success(())
        } catch {
            outcome = .failure(error)
        }
        if generation == roomManagementGeneration { roomsBeingManaged.remove(room) }
        // A leave on another device found meanwhile waited for this.
        if isCurrent(generation) { await departIfLeft(room) }
        try outcome.get()
    }

    /// Leaves `room` when the account left it on another device and this
    /// session is still in it, with the care of a leave made here: a
    /// huddle there is left first and none can start meanwhile. While the
    /// room is being managed nothing is sent; `managing` asks again when
    /// that settles.
    func departIfLeft(_ room: BareJID) async {
        guard directory.hasLeft(room), presence.joinedRooms.contains(room) else { return }
        let nick = presence.selfNicks[room] ?? account.nick
        try? await giveUp(room, .left) { await port.leaveRoom(room, nick: nick) }
    }

    /// Deletes or leaves `room` with `request`, then forgets it.
    ///
    /// The server creates a room when someone joins one that does not
    /// exist, and it takes us out of a room before it answers. So nothing
    /// of ours may reach the room once the request is sent: joins already
    /// on their way and the huddle's leave go first, and the room is fenced
    /// from joins and sealed from huddle presence until the request
    /// settles. A destroyed room keeps both for the rest of the session.
    private func giveUp(_ room: BareJID, _ exit: RoomExit, with request: () async throws -> Void) async throws {
        let forGood = exit == .destroyed
        try await managing(room) { generation in
            departingRooms.insert(room)
            defer {
                if generation == roomManagementGeneration { departingRooms.remove(room) }
            }
            await joinTails[room]?.value
            // A session restarted meanwhile owns the call slot now.
            guard isCurrent(generation) else { throw PortError.notConnected }
            if calls.groupSlotHolds(room) { await calls.hangUp() }
            // A hang-up from a moment ago may still be waiting to send.
            await calls.settleGroupPresence()
            guard isCurrent(generation) else { throw PortError.notConnected }
            calls.sealedGroupRooms.insert(room)
            await calls.settleGroupPresence()
            guard isCurrent(generation) else { throw PortError.notConnected }
            // The connection may have dropped during the waits above. Then
            // nothing is sent, which is not a delete without an answer.
            guard status.connection == .online else {
                calls.sealedGroupRooms.remove(room)
                throw PortError.notConnected
            }
            do {
                try await request()
            } catch {
                guard isCurrent(generation) else { throw error }
                if forGood, (error as? PortError) != .rejected || unconfirmedDeletions.contains(room) {
                    // Timed out or cut off: the room may be gone all the
                    // same, and nothing here can tell. A refusal of a
                    // second try proves nothing either: the server also
                    // refuses to delete a room that no longer exists.
                    unconfirmedDeletions.insert(room)
                    onDemandRooms.remove(room)
                } else {
                    calls.sealedGroupRooms.remove(room)
                }
                throw error
            }
            guard isCurrent(generation) else { return }
            unconfirmedDeletions.remove(room)
            if forGood {
                destroyedRooms.insert(room)
            } else {
                calls.sealedGroupRooms.remove(room)
            }
            if exit == .left {
                directory.setNotifyMode(.never, for: .room(room))
            } else {
                directory.remove(room)
                directoryMutations.append(.remove(room))
            }
            onDemandRooms.remove(room)
            presence.markLeft(room)
            unread.clearRoom(room)
        }
    }

    /// False once the session that started an action has stopped.
    private func isCurrent(_ generation: Int) -> Bool {
        generation == roomManagementGeneration && !isStopped
    }

    /// A room being given up, deleted this session, or possibly deleted:
    /// never joined.
    func isFenced(_ room: BareJID) -> Bool {
        destroyedRooms.contains(room) || departingRooms.contains(room) || unconfirmedDeletions.contains(room)
            || roomsOfDeletedSpaces.contains(room)
    }

    /// Whether the user is in, or joining, the huddle in `room`.
    public func isInHuddle(in room: BareJID) -> Bool {
        calls.groupSlotHolds(room)
    }

    /// Records a channel change for topology requests already in flight.
    func upsertChannel(_ channel: Channel) {
        directory.upsert(channel)
        directoryMutations.append(.upsert(channel))
    }

    /// Records a leave or rejoin, also for topology requests in flight.
    private func markChannel(_ channel: Channel, left: Bool) {
        directory.upsert(channel.with(hasLeft: left))
        directoryMutations.append(.setLeft(channel.roomJID, left))
        silenceLeftRooms()
    }

    private func updateChannel(_ room: BareJID, name: String?, summary: String? = nil) {
        guard let channel = directory.channel(for: room) else { return }
        upsertChannel(channel.with(name: name ?? channel.name, summary: summary ?? channel.summary))
    }
}

/// How a room is given up.
private enum RoomExit {
    /// Deleted for everyone.
    case destroyed
    /// Left for good (a group DM): forgotten here.
    case forgotten
    /// Left until the account rejoins: kept among the rooms it left.
    case left
}

/// Why a room could not be rejoined.
public enum RoomRejoinError: Error, Equatable, Sendable {
    /// The room no longer exists.
    case roomIsGone
}

/// A successful local directory change made while discovery may be suspended.
enum DirectoryMutation {
    case upsert(Channel)
    case remove(BareJID)
    /// The account left or rejoined the room here.
    case setLeft(BareJID, Bool)
}
