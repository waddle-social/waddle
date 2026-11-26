import WaddleKit

/// The block list as a test sets it and watches it asked for.
extension FakePort {
    func fetchBlockList() async throws -> [JID] {
        blockListFetchCount += 1
        let result = blockListResult
        await blockListFetches.wait()
        return try result.get()
    }

    func block(_ jids: [JID]) async throws {
        blockRequests.append(jids)
        await blockingWrites.wait()
        try blockResult.get()
    }

    func unblock(_ jids: [JID]) async throws {
        unblockRequests.append(jids)
        await blockingWrites.wait()
        try unblockResult.get()
    }

    func unblockAll() async throws {
        unblockAllCount += 1
        try unblockAllResult.get()
    }
}
