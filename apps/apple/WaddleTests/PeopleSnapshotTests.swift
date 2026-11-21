import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for the People view (see
/// `assertScreenSnapshot`).
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/PeopleSnapshotTests` and run the tests once to record
/// new ones (that run fails), then commit them.
@MainActor
final class PeopleSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let size = CGSize(width: 393, height: 600)
    #else
    private static let size = CGSize(width: 480, height: 600)
    #endif

    /// Two people here (one asking not to be disturbed, with a status
    /// message), one away, one offline, and a contact with a saved name
    /// who has never been messaged and is offline.
    func testPeopleHereAndAway() async throws {
        let sample = SampleSession()
        sample.coordinator.contacts = [Contact(jid: BareJID(parsing: "sam@waddle.test")!, name: "Sam Okafor")]
        _ = sample.coordinator.presence.apply(WirePresence(
            from: SampleSession.theo.with(resource: "phone")!,
            kind: .available, show: "dnd", status: "Reviewing the identity drafts",
            hats: [], occupant: nil, idleSince: nil
        ))
        _ = sample.coordinator.presence.apply(WirePresence(
            from: SampleSession.mika.with(resource: "phone")!,
            kind: .unavailable, show: nil, status: nil, hats: [], occupant: nil, idleSince: nil
        ))
        let view = NavigationStack { PeopleView() }
        try await assertScreenSnapshot(of: sample.host(view), size: Self.size)
    }

    /// The row on the phone's Home that opens People.
    func testHomeRowCountsPeopleHere() async throws {
        let view = List { PeopleHomeRow() }.navigationListStyle()
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.size.width, height: 120))
    }
}
