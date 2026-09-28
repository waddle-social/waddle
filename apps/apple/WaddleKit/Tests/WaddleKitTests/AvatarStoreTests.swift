import Foundation
import Testing
@testable import WaddleKit

/// Scripted avatar lookups the test settles one by one.
@MainActor
private final class AvatarLookups {
    struct Call: Hashable {
        let jid: BareJID
        let knownID: String?
    }

    private(set) var calls: [Call] = []
    private var pending: [(jid: BareJID, resume: CheckedContinuation<AvatarFetch, Never>)] = []

    var inFlight: Int { pending.count }

    func fetch(_ jid: BareJID, knownID: String?) async -> AvatarFetch {
        calls.append(Call(jid: jid, knownID: knownID))
        return await withCheckedContinuation { pending.append((jid, $0)) }
    }

    /// Answers the oldest open lookup for `jid`.
    func answer(_ jid: BareJID, with result: AvatarFetch) async {
        guard let index = pending.firstIndex(where: { $0.jid == jid }) else {
            Issue.record("no open lookup for \(jid)")
            return
        }
        pending.remove(at: index).resume.resume(returning: result)
        // Let the store's completion run on the main actor.
        for _ in 0..<5 { await Task.yield() }
    }

    func waitForCalls(_ count: Int) async {
        await eventually { calls.count >= count }
    }
}

@MainActor
private final class TestClock {
    var now = date(0)
    func advance(minutes: Double) { now = now.addingTimeInterval(minutes * 60) }
}

private let carol = bare("carol@waddle.test")
private func image(_ byte: UInt8) -> AvatarImage {
    AvatarImage(data: Data([byte]), mediaType: "image/png", width: 0, height: 0)
}

@MainActor
@Suite("Avatar store")
struct AvatarStoreTests {
    private func store() -> (AvatarStore, AvatarLookups, TestClock) {
        let lookups = AvatarLookups()
        let clock = TestClock()
        let store = AvatarStore(now: { clock.now })
        store.fetch = { await lookups.fetch($0, knownID: $1) }
        return (store, lookups, clock)
    }

    @Test func firstRenderFetchesAndConcurrentRequestsShareOneLookup() async {
        let (store, lookups, _) = store()
        store.request(bob)
        store.request(bob)
        await lookups.waitForCalls(1)
        store.request(bob)
        #expect(lookups.calls == [.init(jid: bob, knownID: nil)])

        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))
        #expect(store.image(for: bob) == image(1))
        store.request(bob)
        #expect(lookups.calls.count == 1)
    }

    @Test func atMostFourLookupsRunAtOnce() async {
        let (store, lookups, _) = store()
        let peers = (1...6).map { bare("peer\($0)@waddle.test") }
        peers.forEach(store.request)
        await lookups.waitForCalls(4)
        #expect(lookups.inFlight == 4)
        #expect(lookups.calls.map(\.jid) == Array(peers.prefix(4)))

        await lookups.answer(peers[0], with: .absent)
        await lookups.waitForCalls(5)
        #expect(lookups.inFlight == 4)
        #expect(lookups.calls.last?.jid == peers[4])
    }

    @Test func positiveResultRevalidatesAfter45MinutesWithItsKnownID() async {
        let (store, lookups, clock) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))

        clock.advance(minutes: 44)
        store.request(bob)
        #expect(lookups.calls.count == 1)

        clock.advance(minutes: 2)
        store.request(bob)
        await lookups.waitForCalls(2)
        #expect(lookups.calls[1] == .init(jid: bob, knownID: "b1"))
        await lookups.answer(bob, with: .unchanged)
        #expect(store.image(for: bob) == image(1))

        // Unchanged restarts the 45-minute window.
        clock.advance(minutes: 30)
        store.request(bob)
        #expect(lookups.calls.count == 2)
    }

    @Test func missIsRetriedAfterTenMinutes() async {
        let (store, lookups, clock) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        await lookups.answer(bob, with: .absent)
        #expect(store.image(for: bob) == nil)

        clock.advance(minutes: 9)
        store.request(bob)
        #expect(lookups.calls.count == 1)

        clock.advance(minutes: 2)
        store.request(bob)
        await lookups.waitForCalls(2)
        #expect(lookups.calls[1] == .init(jid: bob, knownID: nil))
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))
        #expect(store.image(for: bob) == image(1))
    }

    @Test func failedRevalidationKeepsTheImageAndRetriesAfterTenMinutes() async {
        let (store, lookups, clock) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))
        clock.advance(minutes: 46)
        store.request(bob)
        await lookups.waitForCalls(2)
        await lookups.answer(bob, with: .failed)
        #expect(store.image(for: bob) == image(1))

        clock.advance(minutes: 11)
        store.request(bob)
        await lookups.waitForCalls(3)
        #expect(lookups.calls[2] == .init(jid: bob, knownID: "b1"))
    }

    @Test func reconnectMarksEverythingStale() async {
        let (store, lookups, _) = store()
        store.request(bob)
        store.request(carol)
        await lookups.waitForCalls(2)
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))
        await lookups.answer(carol, with: .absent)
        let generation = store.generation

        store.markAllStale()
        #expect(store.generation != generation)
        #expect(store.image(for: bob) == image(1))
        store.request(bob)
        store.request(carol)
        await lookups.waitForCalls(4)
        #expect(Set(lookups.calls[2...]) == [.init(jid: bob, knownID: "b1"), .init(jid: carol, knownID: nil)])
    }

    @Test func lookupSpanningAReconnectIsRepeatedWithoutAnotherRequest() async {
        let (store, lookups, _) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))

        store.markAllStale()
        store.request(bob)
        await lookups.waitForCalls(2)
        // Reconnect while the lookup is in flight; nothing asks again.
        store.markAllStale()
        await lookups.answer(bob, with: .failed)
        await lookups.waitForCalls(3)
        #expect(lookups.calls.count == 3)
        #expect(lookups.calls[2] == .init(jid: bob, knownID: "b1"))
        await lookups.answer(bob, with: .unchanged)
        #expect(store.image(for: bob) == image(1))
        // Current again: nothing more until it is due.
        store.request(bob)
        await Task.yield()
        #expect(lookups.calls.count == 3)
    }

    @Test func lookupFinishingInTheSameGenerationIsNotRepeated() async {
        let (store, lookups, _) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        await lookups.answer(bob, with: .absent)
        for _ in 0..<5 { await Task.yield() }
        #expect(lookups.calls.count == 1)
    }

    @Test func avatarChangedRefetchesWithTheKnownID() async {
        let (store, lookups, _) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))

        store.avatarChanged(bob, id: "b2")
        await lookups.waitForCalls(2)
        #expect(lookups.calls[1] == .init(jid: bob, knownID: "b1"))
        await lookups.answer(bob, with: .published(id: "b2", image: image(2)))
        #expect(store.image(for: bob) == image(2))

        // Our own id again: nothing to do.
        store.avatarChanged(bob, id: "b2")
        await Task.yield()
        #expect(lookups.calls.count == 2)
    }

    @Test func avatarDisabledClearsToInitialsAtOnce() async {
        let (store, lookups, _) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))

        store.avatarChanged(bob, id: nil)
        #expect(store.image(for: bob) == nil)
        store.request(bob)
        await Task.yield()
        #expect(lookups.calls.count == 1)
    }

    @Test func avatarChangedDuringALookupDiscardsItsResultAndRefetches() async {
        let (store, lookups, _) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        store.avatarChanged(bob, id: "b2")
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))
        await lookups.waitForCalls(2)
        await lookups.answer(bob, with: .published(id: "b2", image: image(2)))
        #expect(store.image(for: bob) == image(2))
    }

    @Test func disableDuringALookupWins() async {
        let (store, lookups, _) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        store.avatarChanged(bob, id: nil)
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))
        #expect(store.image(for: bob) == nil)
        #expect(lookups.calls.count == 1)
    }

    @Test func avatarChangedForAnUnseenPeerWaitsForFirstRender() async {
        let (store, lookups, _) = store()
        store.avatarChanged(bob, id: "b1")
        await Task.yield()
        #expect(lookups.calls.isEmpty)
    }

    @Test func clearDropsLateResults() async {
        let (store, lookups, _) = store()
        store.request(bob)
        await lookups.waitForCalls(1)
        store.clear()
        await lookups.answer(bob, with: .published(id: "b1", image: image(1)))
        #expect(store.image(for: bob) == nil)
    }

    @Test func publishedOwnAvatarShowsImmediately() async {
        let (store, lookups, _) = store()
        store.set(me.jid, image: image(9))
        #expect(store.image(for: me.jid) == image(9))
        store.request(me.jid)
        await Task.yield()
        #expect(lookups.calls.isEmpty)
    }

    @Test func publishedOwnAvatarRevalidatesWithItsItemID() async {
        let (store, lookups, clock) = store()
        let published = image(9)
        store.set(me.jid, image: published)
        clock.advance(minutes: 46)
        store.request(me.jid)
        await lookups.waitForCalls(1)
        #expect(lookups.calls == [.init(jid: me.jid, knownID: published.itemID)])
        await lookups.answer(me.jid, with: .unchanged)
        #expect(store.image(for: me.jid) == published)
    }

    @Test func itemIDIsTheLowercaseHexSHA1OfTheBytes() {
        let abc = AvatarImage(data: Data("abc".utf8), mediaType: "image/png", width: 0, height: 0)
        #expect(abc.itemID == "a9993e364706816aba3e25717850c26c9cd0d89d")
    }
}

@MainActor
@Suite("Avatar events")
struct AvatarEventTests {
    @Test func avatarChangedEventReachesTheStore() async {
        let port = FakePort()
        port.avatarLookup = { _, _ in .published(id: "b1", image: image(1)) }
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.loadAvatarIfNeeded(bob)
        await eventually { coordinator.avatars.image(for: bob) != nil }

        coordinator.handle(.avatarChanged(jid: bob, id: nil))
        #expect(coordinator.avatars.image(for: bob) == nil)
    }

    @Test func offlineRenderDoesNotLookUp() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.loadAvatarIfNeeded(bob)
        await Task.yield()
        #expect(port.avatarLookups.isEmpty)
    }

    @Test func connectingMarksAvatarsStale() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        let generation = coordinator.avatars.generation
        coordinator.handle(.connected)
        #expect(coordinator.avatars.generation != generation)
    }
}
