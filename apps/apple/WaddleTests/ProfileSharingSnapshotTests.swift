import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for what the profile shares (mood, activity,
/// tune), the activity editor, and what someone else shares and their
/// details. Forms only draw
/// in a real window, so these are screen snapshots (see
/// `assertScreenSnapshot`).
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/ProfileSharingSnapshotTests` and run the tests once to
/// record new ones (that run fails), then commit them.
@MainActor
final class ProfileSharingSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let width: CGFloat = 393
    #else
    private static let width: CGFloat = 480
    #endif

    private let coding = UserActivity(general: "working", specific: "coding", text: "Shipping the profile")

    func testSharingAMoodAnActivityAndATune() async throws {
        let sample = SampleSession()
        sample.coordinator.status.mood = UserMood(value: "happy", text: nil)
        sample.coordinator.status.activity = coding
        sample.coordinator.status.tune = UserTune(title: "Heroes", artist: "David Bowie", album: "\"Heroes\"")
        let view = NavigationStack { Form { ProfileSharingSection() }.huddleForm() }
        try await assertScreenSnapshot(of: sample.host(view), size: CGSize(width: Self.width, height: 320))
    }

    func testSharingNothing() async throws {
        let view = NavigationStack { Form { ProfileSharingSection() }.huddleForm() }
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.width, height: 320))
    }

    func testActivityEditorWithAnActivityPicked() async throws {
        let view = NavigationStack { ProfileActivityEditor(current: UserActivity(general: "working", specific: "coding")) }
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.width, height: 620))
    }

    func testTuneEditorBeforeAnythingIsTyped() async throws {
        let view = NavigationStack { ProfileTuneEditor(current: nil) }
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.width, height: 420))
    }

    func testACallOnAnotherDeviceOwnsTheActivity() async throws {
        let sample = SampleSession()
        sample.coordinator.status.activity = UserActivity(general: "talking", specific: "on_video_phone")
        let view = NavigationStack { Form { ProfileSharingSection() }.huddleForm() }
        try await assertScreenSnapshot(of: sample.host(view), size: CGSize(width: Self.width, height: 320))
    }

    func testSomeoneElsesDetails() async throws {
        let details = ProfileDetails(
            fullName: "Ada Lovelace", nickname: "ada", pronouns: "she/her",
            bio: "Mathematician. Writing notes on the Analytical Engine.", link: "https://ada.example"
        )
        let view = Form { PeerDetailsSection(details: details) }.huddleForm()
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.width, height: 320))
    }

    func testWhatSomeoneElseShares() async throws {
        let shared = SharedStatus(
            mood: UserMood(value: "happy", text: "Shipped it"),
            activity: coding,
            tune: UserTune(title: "Heroes", artist: "David Bowie", album: "\"Heroes\"")
        )
        let view = Form { PeerSharingSection(status: shared, isOnline: true) }.huddleForm()
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.width, height: 320))
    }

    func testSomeoneElseInACall() async throws {
        let shared = SharedStatus(activity: UserActivity(general: "talking", specific: "on_video_phone"))
        let view = Form { PeerSharingSection(status: shared, isOnline: true) }.huddleForm()
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.width, height: 160))
    }
}
