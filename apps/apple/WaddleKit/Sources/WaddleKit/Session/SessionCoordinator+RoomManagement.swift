import Foundation

extension SessionCoordinator {
    /// Fetches the room settings offered by the server. Throws `PortError` on failure.
    public func roomSettings(of room: BareJID) async throws -> RoomSettings {
        try await port.fetchRoomSettings(of: room)
    }

    /// Saves room settings; empty patches send nothing. Throws `PortError` on failure.
    public func updateRoom(_ room: BareJID, with patch: RoomSettingsPatch) async throws {
        guard !patch.isEmpty else { return }
        guard !isStopped else { throw PortError.notConnected }
        let generation = roomManagementGeneration
        try await port.updateRoomSettings(patch, of: room)
        guard generation == roomManagementGeneration, !isStopped else { return }
        updateChannel(room, name: patch.name, summary: patch.summary)
    }

    /// Destroys the room and excludes it for this session. Throws `PortError` on failure.
    public func deleteRoom(_ room: BareJID) async throws {
        guard !isStopped else { throw PortError.notConnected }
        let generation = roomManagementGeneration
        try await port.destroyRoom(room, reason: nil)
        guard generation == roomManagementGeneration, !isStopped else { return }
        destroyedRooms.insert(room)
        await forgetRoom(room, generation: generation)
    }

    /// Renames a group DM; whitespace-only names clear it. Throws `PortError` on failure.
    public func renameGroupDM(_ room: BareJID, to name: String) async throws {
        guard !isStopped else { throw PortError.notConnected }
        let generation = roomManagementGeneration
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        try await port.renameGroupDM(room, name: trimmed.isEmpty ? nil : trimmed)
        guard generation == roomManagementGeneration, !isStopped else { return }
        updateChannel(room, name: trimmed.isEmpty ? (room.localpart ?? room.description) : trimmed)
    }

    /// Sends a group DM invitation. Throws `PortError` on send failure; refusals arrive later as message errors.
    public func inviteToGroupDM(_ room: BareJID, invitee: BareJID, history: GroupDMHistoryAccess) async throws {
        try await port.inviteToGroupDM(room, invitee: invitee, history: history)
    }

    /// Leaves a group DM and forgets it locally. Throws `PortError` on failure.
    public func leaveGroupDM(_ room: BareJID) async throws {
        guard !isStopped else { throw PortError.notConnected }
        let generation = roomManagementGeneration
        try await port.leaveGroupDM(room)
        guard generation == roomManagementGeneration, !isStopped else { return }
        await forgetRoom(room, generation: generation)
    }

    private func forgetRoom(_ room: BareJID, generation: Int) async {
        departingRooms.insert(room)
        defer {
            if generation == roomManagementGeneration { departingRooms.remove(room) }
        }
        if calls.groupSlotHolds(room) { await calls.hangUp() }
        guard generation == roomManagementGeneration, !isStopped else { return }
        directory.remove(room)
        directoryMutations.append(.remove(room))
        onDemandRooms.remove(room)
        presence.markLeft(room)
    }

    /// Records a channel change for topology requests already in flight.
    func upsertChannel(_ channel: Channel) {
        directory.upsert(channel)
        directoryMutations.append(.upsert(channel))
    }

    private func updateChannel(_ room: BareJID, name: String?, summary: String? = nil) {
        guard let channel = directory.channel(for: room) else { return }
        upsertChannel(Channel(
            roomJID: room, name: name ?? channel.name, summary: summary ?? channel.summary,
            kind: channel.kind, position: channel.position, spaceID: channel.spaceID,
            autojoin: channel.autojoin, isGroupDM: channel.isGroupDM))
    }
}

/// A successful local directory change made while discovery may be suspended.
enum DirectoryMutation {
    case upsert(Channel)
    case remove(BareJID)
}
