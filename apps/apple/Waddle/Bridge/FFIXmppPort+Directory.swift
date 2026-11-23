import Foundation
import WaddleKit

extension FFIXmppPort {
    /// XEP-0045 §9.5 tiers `listMembers` merges. The outcast list is read
    /// too, so owners and admins see who is banned. The room refuses it to
    /// everyone else, and `mergedTiers` lists the other tiers regardless.
    static let memberTiers: [WaddleMucAffiliation] = [.owner, .admin, .member, .outcast]

    /// The FFI answers an empty topology on failure and when no session
    /// is live; both must keep the last known directory, so neither is
    /// reported as success. A non-empty topology is always a real answer.
    func discoverTopology() async -> Result<Topology, PortError> {
        let window = signals.beginTopologyDiscovery()
        let topology = await client.discoverTopology()
        let sawError = signals.endTopologyDiscovery(window)
        let isEmpty = topology.spaces.isEmpty && topology.channels.isEmpty
        if isEmpty, sawError {
            return .failure(.failed)
        }
        guard signals.isConnected else {
            return .failure(.notConnected)
        }
        return .success(FFIInbound.topology(topology))
    }

    func joinRoom(_ room: BareJID, nick: String) async {
        await client.joinRoom(roomJid: room.description, nick: nick)
    }

    func leaveRoom(_ room: BareJID, nick: String) async {
        await client.leaveRoom(roomJid: room.description, nick: nick)
    }

    func fetchRoomSettings(of room: BareJID) async throws -> RoomSettings {
        let config = try await mappingPortErrors { try await client.fetchRoomConfig(roomJid: room.description) }
        return FFIInbound.roomSettings(config)
    }

    func updateRoomSettings(_ patch: RoomSettingsPatch, of room: BareJID) async throws {
        try await mappingPortErrors {
            try await client.submitRoomConfig(roomJid: room.description, patch: FFIOutbound.roomSettingsPatch(patch))
        }
    }

    func destroyRoom(_ room: BareJID, reason: String?) async throws {
        try await mappingPortErrors { try await client.destroyRoom(roomJid: room.description, reason: reason) }
    }

    func renameGroupDM(_ room: BareJID, name: String?) async throws {
        try await mappingPortErrors { try await client.renameGroupDm(roomJid: room.description, name: name) }
    }

    /// Refusals arrive later as message errors on the event stream.
    func inviteToGroupDM(_ room: BareJID, invitee: BareJID, history: GroupDMHistoryAccess) async throws {
        try await mappingPortErrors {
            try await client.inviteToGroupDm(
                roomJid: room.description, inviteeJid: invitee.description, history: FFIOutbound.groupDMHistoryAccess(history))
        }
    }

    func leaveGroupDM(_ room: BareJID) async throws {
        try await mappingPortErrors { try await client.leaveGroupDm(roomJid: room.description) }
    }

    func listMembers(of room: BareJID) async throws -> [RoomMember] {
        let roomJid = room.description
        return try await Self.mergedTiers { tier in
            try await mappingPortErrors {
                try await self.client.listRoomMembers(roomJid: roomJid, affiliation: tier)
            }.compactMap(FFIInbound.roomMember)
        }
    }

    /// One query per tier; a room commonly forbids some tiers to
    /// non-admins, so this throws only when every tier failed (or at once
    /// when there is no session).
    static func mergedTiers(
        _ fetch: @escaping @Sendable (WaddleMucAffiliation) async throws -> [RoomMember]
    ) async throws -> [RoomMember] {
        enum TierOutcome: Sendable {
            case entries([RoomMember])
            case notConnected
            case error(PortError)
        }
        let outcomes = await withTaskGroup(of: (Int, TierOutcome).self, returning: [(Int, TierOutcome)].self) { group in
            for (index, tier) in memberTiers.enumerated() {
                group.addTask {
                    do {
                        return (index, .entries(try await fetch(tier)))
                    } catch PortError.notConnected {
                        return (index, .notConnected)
                    } catch {
                        return (index, .error(FFIOutbound.portError(error)))
                    }
                }
            }
            var collected: [(Int, TierOutcome)] = []
            for await outcome in group {
                collected.append(outcome)
            }
            return collected.sorted { $0.0 < $1.0 }
        }
        var members: [RoomMember] = []
        var firstError: PortError?
        var anySucceeded = false
        for (_, outcome) in outcomes {
            switch outcome {
            case let .entries(entries):
                members += entries
                anySucceeded = true
            case .notConnected:
                throw PortError.notConnected
            case let .error(error):
                firstError = firstError ?? error
            }
        }
        guard anySucceeded else {
            throw firstError ?? PortError.failed
        }
        return FFIInbound.mergedMembers(members)
    }

    /// XEP-0030 disco#items on the room's `urn:waddle:room:bots:0` node.
    func listRoomBots(in room: BareJID) async throws -> [RoomBot] {
        let entries = try await mappingPortErrors { try await client.listRoomBots(roomJid: room.description) }
        return entries.compactMap(FFIInbound.roomBot)
    }

    func setAffiliation(_ affiliation: RoomAffiliation, of user: BareJID, in room: BareJID, reason: String?) async throws {
        try await mappingPortErrors {
            try await client.setRoomAffiliation(
                roomJid: room.description,
                targetJid: user.description,
                affiliation: FFIOutbound.affiliation(affiliation),
                reason: reason
            )
        }
    }

    func kick(nick: String, from room: BareJID, reason: String?) async throws {
        try await mappingPortErrors { try await client.kickOccupant(roomJid: room.description, nick: nick, reason: reason) }
    }

    /// A contact whose address does not parse is skipped, not fatal.
    func fetchContacts() async throws -> [Contact] {
        try await mappingPortErrors { try await client.fetchRosterContacts() }.compactMap { contact in
            FFIInbound.bareJID(contact.jid).map { Contact(jid: $0, name: contact.name) }
        }
    }

    func searchUsers(_ query: String) async throws -> [UserSearchResult] {
        let entries = try await mappingPortErrors { try await client.searchUsers(query: query) }
        return entries.compactMap(FFIInbound.userSearchResult)
    }

    func createRoom(localpart: String, name: String, summary: String?, nick: String) async throws -> BareJID {
        let patch = WaddleRoomConfigPatch(membersOnly: nil, publicRoom: nil, persistentRoom: nil, name: name, description: summary, forum: nil, pinPermission: nil)
        let created = try await mappingPortErrors { try await client.createRoom(localpart: localpart, nick: nick, patch: patch) }
        guard let room = FFIInbound.bareJID(created) else { throw PortError.failed }
        return room
    }

    /// `urn:waddle:group-dm:create:0` requires the creator among the
    /// members, so the account is added when the caller left it out.
    func createGroupDM(name: String, members: [BareJID]) async throws -> BareJID {
        let memberJIDs = Self.groupMembers(members, including: account).map(\.description)
        let created = try await mappingPortErrors { try await client.createGroupDm(name: name, memberJids: memberJIDs) }
        guard let room = FFIInbound.bareJID(created) else { throw PortError.failed }
        return room
    }

    static func groupMembers(_ members: [BareJID], including account: BareJID?) -> [BareJID] {
        var unique: [BareJID] = []
        for member in (account.map { [$0] } ?? []) + members where !unique.contains(member) {
            unique.append(member)
        }
        return unique
    }
}
