import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Block list")
struct BlockListTests {
    private let person = jid("ada@waddle.test")
    private let full = jid("ada@waddle.test/phone")
    private let domain = jid("remote.test")
    private let domainResource = jid("waddle.test/service")

    @Test func seedAndPushesPreserveOrderAndExactEntries() {
        let store = BlockListStore()
        #expect(store.load == .idle)
        store.seed([full, domain, full, domainResource], fetchedAt: store.versionBeforeFetch())

        #expect(store.blockedJIDs == [full, domain, domainResource])
        #expect(!store.isBlocked(person.bare))
        #expect(store.isBlocked(bare("someone@remote.test")))
        #expect(!store.isBlocked(bare("someone@sub.remote.test")))

        store.apply(.blocked([domain, person, person]))
        #expect(store.blockedJIDs == [full, domain, domainResource, person])
        #expect(store.isBlocked(person.bare))

        store.apply(.unblocked([person, domain]))
        #expect(store.blockedJIDs == [full, domainResource])
        #expect(!store.isBlocked(person.bare))

        store.apply(.unblockedAll)
        #expect(store.blockedJIDs.isEmpty)
    }

    /// A snapshot overtaken by a change is from before it: it is not taken, and the list is asked for again.
    @Test func aPushDuringTheFetchHasTheListReadAgain() async {
        let port = FakePort()
        port.blockListResult = .success([person])
        port.blockListFetches.isHolding = true
        let coordinator = start(port)
        coordinator.blockListRetryDelay = .milliseconds(1)
        await eventually { port.blockListFetches.count == 1 }
        #expect(coordinator.blockList.load == .loading)

        port.emit(.blockListChanged(.blocked([domain])))
        await eventually { coordinator.blockList.blockedJIDs == [domain] }
        // What the server holds now, which the second read returns.
        port.blockListResult = .success([person, domain])
        port.blockListFetches.isHolding = false
        port.blockListFetches.release()
        await coordinator.readyTask?.value

        #expect(port.blockListFetchCount == 2)
        #expect(coordinator.blockList.load == .loaded)
        #expect(coordinator.blockList.blockedJIDs == [person, domain])
        await coordinator.stop()
    }

    @Test func aReadThatFailsIsTriedAgain() async {
        let port = FakePort()
        port.blockListResult = .failure(.timeout)
        // Held, so the first read has taken its answer before the next one's is set.
        port.blockListFetches.isHolding = true
        let coordinator = start(port)
        coordinator.blockListRetryDelay = .milliseconds(1)
        await eventually { port.blockListFetches.count == 1 }
        port.blockListResult = .success([person])
        port.blockListFetches.isHolding = false
        port.blockListFetches.release()
        await coordinator.readyTask?.value
        #expect(port.blockListFetchCount == 2)
        #expect(coordinator.blockList.load == .loaded)
        #expect(coordinator.blockList.blockedJIDs == [person])
        await coordinator.stop()
    }

    /// Their own entry before their server's; one device of theirs is not them.
    @Test func whatBlocksAPerson() {
        let store = BlockListStore()
        let device = JID(parsing: "\(person.bare)/phone")!
        store.seed([device], fetchedAt: store.versionBeforeFetch())
        #expect(store.entry(covering: person.bare) == nil)
        store.apply(.blocked([JID(parsing: person.bare.domain)!]))
        #expect(store.entry(covering: person.bare)?.bare.localpart == nil)
        store.apply(.blocked([person]))
        #expect(store.entry(covering: person.bare) == person)
    }

    @Test func fetchesOnEveryConnection() async {
        let port = FakePort()
        port.blockListResult = .success([person])
        let coordinator = await connected(port)
        #expect(port.blockListFetchCount == 1)
        #expect(coordinator.blockList.load == .loaded)
        #expect(coordinator.blockList.blockedJIDs == [person])

        port.blockListResult = .success([domain])
        await reconnect(coordinator, port)
        #expect(port.blockListFetchCount == 2)
        #expect(coordinator.blockList.load == .loaded)
        #expect(coordinator.blockList.blockedJIDs == [domain])
        await coordinator.stop()
    }

    @Test func aFailedFetchKeepsTheEarlierList() async {
        let port = FakePort()
        port.blockListResult = .success([person])
        let coordinator = await connected(port)

        port.blockListResult = .failure(.timeout)
        await reconnect(coordinator, port)
        #expect(coordinator.blockList.load == .failed)
        #expect(coordinator.blockList.blockedJIDs == [person])
        await coordinator.stop()
    }

    @Test func aFetchFromADroppedConnectionCannotOverwriteTheNewList() async {
        let port = FakePort()
        port.blockListResult = .success([person])
        port.blockListFetches.isHolding = true
        let coordinator = start(port)
        await eventually { port.blockListFetches.count == 1 }
        let firstReady = coordinator.readyTask

        port.blockListFetches.isHolding = false
        port.blockListResult = .success([domain])
        await reconnect(coordinator, port)
        #expect(coordinator.blockList.blockedJIDs == [domain])

        port.blockListFetches.release()
        await firstReady?.value
        #expect(coordinator.blockList.load == .loaded)
        #expect(coordinator.blockList.blockedJIDs == [domain])
        await coordinator.stop()
    }

    @Test(arguments: [false, true])
    func writesWaitForConfirmationAndApplyOnce(withPush: Bool) async throws {
        let port = FakePort()
        port.blockListResult = .success([domain])
        let coordinator = await connected(port)
        port.blockingWrites.isHolding = true

        let block = Task { try await coordinator.block(person.bare) }
        await eventually { port.blockingWrites.count == 1 }
        #expect(port.blockRequests == [[person]])
        #expect(coordinator.blockList.blockedJIDs == [domain])
        if withPush {
            port.emit(.blockListChanged(.blocked([person])))
            await eventually { coordinator.blockList.blockedJIDs == [domain, person] }
        }
        port.blockingWrites.release()
        try await block.value
        #expect(coordinator.blockList.blockedJIDs == [domain, person])

        let unblock = Task { try await coordinator.unblock(domain) }
        await eventually { port.blockingWrites.count == 1 }
        #expect(port.unblockRequests == [[domain]])
        #expect(coordinator.blockList.blockedJIDs == [domain, person])
        if withPush {
            port.emit(.blockListChanged(.unblocked([domain])))
            await eventually { coordinator.blockList.blockedJIDs == [person] }
        }
        port.blockingWrites.release()
        try await unblock.value
        #expect(coordinator.blockList.blockedJIDs == [person])
        await coordinator.stop()
    }

    @Test func writeErrorsPropagateWithoutChangingTheList() async {
        let port = FakePort()
        port.blockListResult = .success([domain])
        port.blockResult = .failure(.rejected)
        port.unblockResult = .failure(.notConnected)
        let coordinator = await connected(port)

        await #expect(throws: PortError.rejected) { try await coordinator.block(person.bare) }
        await #expect(throws: PortError.notConnected) { try await coordinator.unblock(domain) }
        #expect(coordinator.blockList.blockedJIDs == [domain])
        await coordinator.stop()
    }

    @Test func signOutClearsTheListAndLoadState() async {
        let port = FakePort()
        port.blockListResult = .success([person])
        let coordinator = await connected(port)

        await coordinator.signOut()

        #expect(coordinator.blockList.blockedJIDs.isEmpty)
        #expect(coordinator.blockList.load == .idle)
    }

    private func start(_ port: FakePort) -> SessionCoordinator {
        let coordinator = SessionCoordinator(
            account: me, port: port, reconnectPolicy: ReconnectPolicy(base: 0.01, cap: 0.01))
        coordinator.blockListRetryDelay = .milliseconds(1)
        coordinator.start()
        port.emit(.connected)
        return coordinator
    }

    private func connected(_ port: FakePort) async -> SessionCoordinator {
        let coordinator = start(port)
        await eventually { coordinator.isSendReady }
        await coordinator.readyTask?.value
        return coordinator
    }

    private func reconnect(_ coordinator: SessionCoordinator, _ port: FakePort) async {
        await eventually { port.connectCount == 1 }
        port.emit(.disconnected)
        await eventually { port.connectCount == 2 }
        port.emit(.connected)
        await eventually { coordinator.isSendReady }
        await coordinator.readyTask?.value
    }
}
