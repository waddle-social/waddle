import Foundation
import WaddleKit

extension FFIXmppPort {
    /// XEP-0045 §9.5 tiers `listMembers` merges. Outcasts are not members.
    static let memberTiers: [WaddleMucAffiliation] = [.owner, .admin, .member]

    /// The FFI answers an empty topology on failure and when no session
    /// is live; both must keep the last known directory, so neither is
    /// reported as success. A non-empty topology is always a real answer.
    func discoverTopology() async -> Result<Topology, PortError> {
        signals.beginTopologyDiscovery()
        let topology = await client.discoverTopology()
        let sawError = signals.endTopologyDiscovery()
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

    /// One query per tier; a room commonly forbids some tiers to
    /// non-admins, so this throws only when every tier failed (or at once
    /// when there is no session).
    func listMembers(of room: BareJID) async throws -> [RoomMember] {
        var members: [RoomMember] = []
        var firstError: PortError?
        var anySucceeded = false
        for tier in Self.memberTiers {
            do {
                let entries = try await mappingPortErrors {
                    try await client.listRoomMembers(roomJid: room.description, affiliation: tier)
                }
                members += entries.compactMap(FFIInbound.roomMember)
                anySucceeded = true
            } catch PortError.notConnected {
                throw PortError.notConnected
            } catch {
                firstError = firstError ?? FFIOutbound.portError(error)
            }
        }
        guard anySucceeded else {
            throw firstError ?? PortError.failed
        }
        return FFIInbound.mergedMembers(members)
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

    func searchUsers(_ query: String) async throws -> [UserSearchResult] {
        let entries = try await mappingPortErrors { try await client.searchUsers(query: query) }
        return entries.compactMap(FFIInbound.userSearchResult)
    }

    func createRoom(localpart: String, name: String, summary: String?, nick: String) async throws -> BareJID {
        let patch = WaddleRoomConfigPatch(name: name, description: summary, forum: nil, pinPermission: nil)
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
