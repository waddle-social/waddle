import XCTest
@testable import Waddle
@testable import WaddleKit

/// Which room occupant a profile opened from a room row stands for. A nick
/// can pass to someone else while the profile is open; the occupant only
/// counts while it is still the same person.
@MainActor
final class PeerProfilePresenceTests: XCTestCase {
    private let room = SampleSession.design

    func testTheOccupantWithTheProfilesAddressIsTheirs() {
        let sample = SampleSession()
        sample.occupy(room, nick: "Priya", jid: SampleSession.priya, affiliation: .admin)
        let route = PeerProfileRoute(name: "Priya", jid: SampleSession.priya, room: room, nick: "Priya")
        XCTAssertEqual(PeerProfilePresence.occupant(of: route, session: sample.coordinator)?.affiliation, .admin)
    }

    func testANickNowHeldBySomeoneElseIsNotTheirs() {
        let sample = SampleSession()
        sample.occupy(room, nick: "Priya", jid: SampleSession.theo, affiliation: .owner)
        let route = PeerProfileRoute(name: "Priya", jid: SampleSession.priya, room: room, nick: "Priya")
        XCTAssertNil(PeerProfilePresence.occupant(of: route, session: sample.coordinator))
    }

    /// An anonymous row has no address to compare, so the nick decides.
    func testWithoutAnAddressTheNickDecides() {
        let sample = SampleSession()
        sample.occupy(room, nick: "Priya", jid: SampleSession.priya, affiliation: .member)
        let route = PeerProfileRoute(name: "Priya", jid: nil, room: room, nick: "Priya")
        XCTAssertEqual(PeerProfilePresence.occupant(of: route, session: sample.coordinator)?.nick, "Priya")
    }
}
