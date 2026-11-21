import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for what the profile shares (mood, activity,
/// tune), the activity editor, and someone else's details. Forms only draw
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
        let view = NavigationStack { Form { ProfileSharingSection() }.formStyle(.grouped) }
        try await assertScreenSnapshot(of: sample.host(view), size: CGSize(width: Self.width, height: 320))
    }

    func testSharingNothing() async throws {
        let view = NavigationStack { Form { ProfileSharingSection() }.formStyle(.grouped) }
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.width, height: 320))
    }

    func testActivityEditorWithAnActivityPicked() async throws {
        let view = NavigationStack { ProfileActivityEditor(current: UserActivity(general: "working", specific: "coding")) }
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.width, height: 620))
    }

    func testSomeoneElsesDetails() async throws {
        let details = ProfileDetails(
            fullName: "Ada Lovelace", nickname: "ada", pronouns: "she/her",
            bio: "Mathematician. Writing notes on the Analytical Engine.", link: "https://ada.example"
        )
        let view = Form { PeerDetailsSection(details: details) }.formStyle(.grouped)
        try await assertScreenSnapshot(of: SampleSession().host(view), size: CGSize(width: Self.width, height: 320))
    }
}
