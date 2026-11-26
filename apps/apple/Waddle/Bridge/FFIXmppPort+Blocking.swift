import WaddleKit

extension FFIXmppPort {
    func fetchBlockList() async throws -> [JID] {
        let jids = try await mappingPortErrors { try await client.fetchBlockList() }
        return jids.compactMap(JID.init(parsing:))
    }

    func block(_ jids: [JID]) async throws {
        try await mappingPortErrors { try await client.blockJids(jids: jids.map(\.description)) }
    }

    func unblock(_ jids: [JID]) async throws {
        try await mappingPortErrors { try await client.unblockJids(jids: jids.map(\.description)) }
    }

    func unblockAll() async throws {
        try await mappingPortErrors { try await client.unblockAll() }
    }
}
