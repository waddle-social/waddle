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
        try await giveUp(room, forGood: true) { try await port.destroyRoom(room, reason: nil) }
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
        try await giveUp(room, forGood: false) { try await port.leaveGroupDM(room) }
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
        defer {
            if generation == roomManagementGeneration { roomsBeingManaged.remove(room) }
        }
        try await action(generation)
    }

    /// Deletes or leaves `room` with `request`, then forgets it.
    ///
    /// The server creates a room when someone joins one that does not
    /// exist, and it takes us out of a room before it answers. So nothing
    /// of ours may reach the room once the request is sent: joins already
    /// on their way and the huddle's leave go first, and the room is fenced
    /// from joins and sealed from huddle presence until the request
    /// settles. `forGood` keeps both for the rest of the session.
    private func giveUp(_ room: BareJID, forGood: Bool, with request: () async throws -> Void) async throws {
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
            directory.remove(room)
            directoryMutations.append(.remove(room))
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

    private func updateChannel(_ room: BareJID, name: String?, summary: String? = nil) {
        guard let channel = directory.channel(for: room) else { return }
        upsertChannel(channel.with(name: name ?? channel.name, summary: summary ?? channel.summary))
    }
}

/// A successful local directory change made while discovery may be suspended.
enum DirectoryMutation {
    case upsert(Channel)
    case remove(BareJID)
}
