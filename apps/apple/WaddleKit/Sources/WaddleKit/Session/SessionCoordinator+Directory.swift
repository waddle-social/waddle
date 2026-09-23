import Foundation

extension SessionCoordinator {
    /// Rediscovers spaces and rooms, then joins every autojoin room, group
    /// DM, and room the user opened or created this session. A failed
    /// discovery keeps the last known directory.
    func refreshDirectory() async {
        if case let .success(topology) = await port.discoverTopology() {
            directory.apply(topology)
        }
        let listed = (directory.channels + directory.groupDMs)
            .filter { $0.autojoin || $0.isGroupDM }
            .map(\.roomJID)
        var joined: Set<BareJID> = []
        for room in listed + onDemandRooms.sorted(by: { $0.description < $1.description }) where joined.insert(room).inserted {
            await port.joinRoom(room, nick: account.nick)
        }
    }

    /// Joins `room` if we are not an occupant, and keeps it joined across
    /// reconnects. Rooms whose bookmark says `autojoin=false` (XEP-0402)
    /// are otherwise never entered, so opening one would show a room we
    /// cannot send to or receive from (XEP-0045 §7.4).
    func ensureJoined(_ room: BareJID) async {
        onDemandRooms.insert(room)
        // Offline, the ready pipeline joins it on connect.
        guard status.connection == .online, !presence.joinedRooms.contains(room) else { return }
        await port.joinRoom(room, nick: account.nick)
    }

    func loadNotifyModes() async {
        guard let modes = try? await port.fetchNotifyModes() else { return }
        directory.replaceNotifyModes(modes)
    }

    /// XEP-0492 notification mode for a conversation. Optimistic, rolled
    /// back if the server rejects it.
    public func setNotifyMode(_ mode: NotifyMode, for conversation: ConversationID) async throws {
        let previous = directory.notifyMode(for: conversation)
        directory.setNotifyMode(mode, for: conversation)
        do {
            try await port.setNotifyMode(mode, for: conversation)
        } catch {
            directory.setNotifyMode(previous, for: conversation)
            throw error
        }
    }

    /// Creates a channel room, joins it, and returns its conversation.
    public func createChannel(name: String, summary: String?) async throws -> ConversationID {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let localpart = RoomLocalpart.make(from: trimmed) else { throw PortError.invalidRequest }
        let room = try await port.createRoom(localpart: localpart, name: trimmed, summary: summary, nick: account.nick)
        directory.upsert(Channel(roomJID: room, name: trimmed, summary: summary, position: directory.channels.count))
        // create_room leaves the room once it is configured.
        await ensureJoined(room)
        return .room(room)
    }

    /// Creates a group DM with `members` and returns its conversation.
    public func createGroupDM(name: String, members: [BareJID]) async throws -> ConversationID {
        let room = try await port.createGroupDM(name: name, members: members)
        directory.upsert(Channel(roomJID: room, name: name, isGroupDM: true))
        await ensureJoined(room)
        return .room(room)
    }

    /// Opens (or creates) the 1:1 conversation with `peer`.
    public func directConversation(with peer: BareJID) -> ConversationID {
        if !directory.directConversations.contains(where: { $0.peer == peer }) {
            directory.touchDirect(peer, at: Date(), preview: nil)
        }
        return .direct(peer)
    }

    public func members(of room: BareJID) async throws -> [RoomMember] {
        try await port.listMembers(of: room)
    }

    public func setAffiliation(_ affiliation: RoomAffiliation, of user: BareJID, in room: BareJID, reason: String? = nil) async throws {
        try await port.setAffiliation(affiliation, of: user, in: room, reason: reason)
    }

    public func kick(nick: String, from room: BareJID, reason: String? = nil) async throws {
        try await port.kick(nick: nick, from: room, reason: reason)
    }

    public func searchUsers(_ query: String) async throws -> [UserSearchResult] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return [] }
        return try await port.searchUsers(trimmed)
    }

    /// Our own occupant in `room`, for permission checks.
    public func selfOccupant(in room: BareJID) -> Occupant? {
        presence.occupant(named: account.nick, in: room)
    }
}

/// Derives a room localpart from a display name: lowercase ASCII letters,
/// digits and dashes, as Waddle channel JIDs use.
public enum RoomLocalpart {
    public static func make(from name: String) -> String? {
        var result = ""
        var lastWasDash = false
        for scalar in name.lowercased().unicodeScalars {
            if ("a"..."z").contains(scalar) || ("0"..."9").contains(scalar) {
                result.unicodeScalars.append(scalar)
                lastWasDash = false
            } else if !lastWasDash, !result.isEmpty {
                result += "-"
                lastWasDash = true
            }
        }
        while result.hasSuffix("-") {
            result.removeLast()
        }
        let capped = String(result.prefix(64))
        return capped.isEmpty ? nil : capped
    }
}
