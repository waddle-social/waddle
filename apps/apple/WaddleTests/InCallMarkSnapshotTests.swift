import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for the "in a call" badge (ADR-010) on the
/// avatars of the People view and the DM lists, as the web app draws it.
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/InCallMarkSnapshotTests` and run the tests once to record
/// new ones (that run fails), then commit them.
@MainActor
final class InCallMarkSnapshotTests: XCTestCase {
    /// The phone is an SF Symbol, whose glyph changes between releases.
    private static let precision: Float = 0.98
    private static let call = UserActivity(general: "talking", specific: "on_the_phone")
    private static let lastWeek = Date(timeIntervalSince1970: 1_700_000_000)

    /// Every avatar size a person is listed at, with each availability the
    /// badge can sit beside.
    func testAvatarsInACall() {
        let states: [Availability] = [.available, .away, .doNotDisturb]
        assertHuddleSnapshot(precision: Self.precision, of: VStack(alignment: .leading, spacing: Theme.Spacing.l) {
            HStack(alignment: .bottom, spacing: Theme.Spacing.l) {
                ForEach([20, Theme.Size.rowAvatar, 40, Theme.Size.memberAvatar], id: \.self) { size in
                    PeerPresenceAvatar(jid: nil, name: "Priya", availability: .available, size: size, isInCall: true)
                }
            }
            HStack(spacing: Theme.Spacing.l) {
                ForEach(states, id: \.self) { state in
                    PeerPresenceAvatar(jid: nil, name: "Jo Hartmann", availability: state, size: 40, isInCall: true)
                }
                PeerPresenceAvatar(jid: nil, name: "Jo Hartmann", availability: .available, size: 40)
            }
        })
    }

    /// The sidebar's one-line rows, with Priya in a call and Theo not. The
    /// phone's two-line row draws the same avatar, and also a date whose
    /// format follows the machine's region, so it is left out.
    func testDirectMessageRowsWithSomeoneInACall() {
        let sample = SampleSession()
        let read = ConversationAttention(unread: 0, isMention: false, isMuted: false)
        let priya = DirectConversation(
            peer: SampleSession.priya, lastActivity: Self.lastWeek, preview: "Shipping the digest build now."
        )
        let theo = DirectConversation(
            peer: SampleSession.theo, lastActivity: Self.lastWeek, preview: "Reviewing the identity drafts."
        )
        let rows = VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            CompactDirectRowLabel(direct: priya, title: "Priya", availability: .available, isInCall: true, attention: read)
            CompactDirectRowLabel(direct: theo, title: "Theo", availability: .doNotDisturb, isInCall: false, attention: read)
        }
        assertHuddleSnapshot(precision: Self.precision, of: sample.host(rows))
    }

    /// People rows: Priya in a call, Theo in one while asking not to be
    /// disturbed, Mika not in one.
    func testPeopleRowsWithSomeoneInACall() {
        let sample = SampleSession()
        let people = [
            Person(jid: SampleSession.priya, name: "Priya", availability: .available, status: nil, isInCall: true),
            Person(
                jid: SampleSession.theo, name: "Theo", availability: .doNotDisturb,
                status: "Reviewing the identity drafts", isInCall: true
            ),
            Person(jid: SampleSession.mika, name: "Mika", availability: .available, status: nil, isInCall: false),
        ]
        let rows = VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            ForEach(people) { PersonRow(person: $0) }
        }
        assertHuddleSnapshot(precision: Self.precision, of: sample.host(rows))
    }

    /// The session's own rule, end to end: a call activity heard for
    /// someone online marks their row.
    func testAHeardCallMarksTheirRow() {
        let sample = SampleSession()
        sample.coordinator.handle(.activityChanged(jid: SampleSession.priya, activity: Self.call))

        XCTAssertTrue(sample.coordinator.presence.isInCall(SampleSession.priya))
        let people = PeopleList(session: sample.coordinator)
        XCTAssertEqual(people.here.first { $0.jid == SampleSession.priya }?.isInCall, true)
        XCTAssertEqual(people.here.first { $0.jid == SampleSession.mika }?.isInCall, false)
    }

    /// Someone's status is looked up again after our reconnect (the store
    /// was cleared), not when they come online: that would read back a
    /// call a crashed device left behind.
    func testSharedStatusIsLookedUpAgainOnReconnectNotWhenTheyComeOnline() {
        let sample = SampleSession()
        let priya = SampleSession.priya
        let shown = SharedStatusLookup(jid: priya, session: sample.coordinator)

        _ = sample.coordinator.presence.apply(WirePresence(
            from: priya.with(resource: "phone")!, kind: .unavailable, show: nil, status: nil,
            hats: [], occupant: nil, idleSince: nil
        ))
        _ = sample.coordinator.presence.apply(WirePresence(
            from: priya.with(resource: "laptop")!, kind: .available, show: nil, status: nil,
            hats: [], occupant: nil, idleSince: nil
        ))
        XCTAssertEqual(SharedStatusLookup(jid: priya, session: sample.coordinator), shown)

        sample.coordinator.status.connection = .offline(retryAt: nil)
        let offline = SharedStatusLookup(jid: priya, session: sample.coordinator)
        XCTAssertNotEqual(offline, shown)
        XCTAssertFalse(offline.isConnected)

        sample.coordinator.status.connection = .online
        XCTAssertEqual(SharedStatusLookup(jid: priya, session: sample.coordinator), shown)
    }
}
