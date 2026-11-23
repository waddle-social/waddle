import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// A contact signed in on several devices shows one status: the most
/// available, in ADR-010's order. Do not disturb > available > away >
/// extended away > offline.
@MainActor
@Suite("Most-available status across devices")
struct MostAvailableTests {
    private func shown(_ first: String?, _ second: String?) -> Availability {
        let presence = PresenceStore(isRoom: { _ in false })
        presence.apply(WirePresence(from: jid("bob@waddle.test/laptop"), kind: .available, show: first))
        presence.apply(WirePresence(from: jid("bob@waddle.test/phone"), kind: .available, show: second))
        return presence.availability(of: bob)
    }

    @Test(arguments: [
        // A deliberate do not disturb wins over every other online state.
        ("dnd", nil, Availability.doNotDisturb),
        ("dnd", "chat", .doNotDisturb),
        ("dnd", "away", .doNotDisturb),
        ("dnd", "xa", .doNotDisturb),
        (nil, "away", .available),
        (nil, "xa", .available),
        ("chat", "away", .chat),
        ("away", "xa", .away),
    ] as [(String?, String?, Availability)])
    func theMostAvailableDeviceWins(_ pair: (String?, String?, Availability)) {
        #expect(shown(pair.0, pair.1) == pair.2)
        // Whichever device spoke last.
        #expect(shown(pair.1, pair.0) == pair.2)
    }

    @Test func anOnlineDeviceWinsOverOneThatWentOffline() {
        let presence = PresenceStore(isRoom: { _ in false })
        presence.apply(WirePresence(from: jid("bob@waddle.test/laptop"), kind: .available, show: "xa"))
        presence.apply(WirePresence(from: jid("bob@waddle.test/phone"), kind: .available, show: "dnd"))
        presence.apply(WirePresence(from: jid("bob@waddle.test/phone"), kind: .unavailable))

        #expect(presence.availability(of: bob) == .extendedAway)
    }
}
