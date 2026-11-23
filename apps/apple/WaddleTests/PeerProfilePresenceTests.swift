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

    // MARK: A call seen only through a room

    private let call = SharedStatus(activity: UserActivity(general: "talking", specific: "on_the_phone"))

    /// Someone seen only in the room sends no activity notifications, so
    /// their profile shows the call the lookup read, under the availability
    /// the profile finds for them, and keeps looking for its end.
    func testAProfileFromARoomShowsTheCallLookedUpWhileTheyAreThere() {
        let sample = SampleSession()
        let ivy = bare("ivy@waddle.test")
        sample.occupy(room, nick: "Ivy", jid: ivy, affiliation: .member)
        let route = PeerProfileRoute(name: "Ivy", jid: ivy, room: room, nick: "Ivy")

        let isOnline = PeerProfilePresence.resolve(route, session: sample.coordinator).availability.isOnline
        let live = sample.coordinator.liveSharedStatus(of: ivy, lookedUp: call)
        XCTAssertTrue(live?.shown(isOnline: isOnline).activity?.showsACall == true)
        XCTAssertFalse(sample.coordinator.presence.isInCall(ivy))
        XCTAssertTrue(UnheardCallCheck(jid: ivy, shown: call, isOnline: isOnline, session: sample.coordinator).isChecking)

        sample.coordinator.presence.markLeft(room)
        let gone = PeerProfilePresence.resolve(route, session: sample.coordinator).availability.isOnline
        XCTAssertFalse(gone)
        XCTAssertNil(live?.shown(isOnline: gone).activity)
        XCTAssertFalse(sample.coordinator.presence.isInCall(ivy))
        XCTAssertFalse(UnheardCallCheck(jid: ivy, shown: call, isOnline: gone, session: sample.coordinator).isChecking)
    }

    /// Someone whose presence is seen is told when their call ends, so it
    /// is not looked up again; nor is the account's own here.
    func testACallThatWillBeNotifiedIsNotLookedUpAgain() {
        let sample = SampleSession()
        XCTAssertFalse(UnheardCallCheck(
            jid: SampleSession.priya, shown: call, isOnline: true, session: sample.coordinator
        ).isChecking)
        XCTAssertFalse(UnheardCallCheck(
            jid: SampleSession.ana.jid, shown: call, isOnline: true, session: sample.coordinator
        ).isChecking)
    }
}
