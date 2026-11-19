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
        try await giveUp(room) { generation in
            try await port.destroyRoom(room, reason: nil)
            if isCurrent(generation) { destroyedRooms.insert(room) }
        }
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
        try await giveUp(room) { _ in try await port.leaveGroupDM(room) }
    }

    /// Runs one management action on `room`. A second action on the same
    /// room fails instead of overlapping: the shared client saves settings
    /// by reading the whole form and writing it back, so two at once would
    /// undo each other.
    private func managing(_ room: BareJID, _ action: (_ generation: Int) async throws -> Void) async throws {
        guard !isStopped else { throw PortError.notConnected }
        guard roomsBeingManaged.insert(room).inserted else { throw PortError.failed }
        let generation = roomManagementGeneration
        defer {
            if generation == roomManagementGeneration { roomsBeingManaged.remove(room) }
        }
        try await action(generation)
    }

    /// Deletes or leaves `room` with `request`, then forgets it. The room
    /// is fenced from joins for the whole request: the server takes us out
    /// of a room before it answers, and a join in that gap, or the huddle's
    /// leave presence sent afterwards, would create a deleted room again.
    private func giveUp(_ room: BareJID, with request: (_ generation: Int) async throws -> Void) async throws {
        try await managing(room) { generation in
            departingRooms.insert(room)
            defer {
                if generation == roomManagementGeneration { departingRooms.remove(room) }
            }
            if calls.groupSlotHolds(room) { await calls.hangUp() }
            // A hang-up from a moment ago may still be waiting to send.
            await calls.settleGroupPresence()
            guard isCurrent(generation) else { throw PortError.notConnected }
            try await request(generation)
            guard isCurrent(generation) else { return }
            directory.remove(room)
            directoryMutations.append(.remove(room))
            onDemandRooms.remove(room)
            presence.markLeft(room)
        }
    }

    /// False once the session that started an action has stopped.
    private func isCurrent(_ generation: Int) -> Bool {
        generation == roomManagementGeneration && !isStopped
    }

    /// A room being given up, or deleted this session: never joined.
    func isFenced(_ room: BareJID) -> Bool {
        destroyedRooms.contains(room) || departingRooms.contains(room)
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
