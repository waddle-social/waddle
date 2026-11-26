import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for blocking (XEP-0191): the action in a
/// person's details, before and after, and the list in Settings. Forms
/// only draw in a real window, so these are screen snapshots, compared on
/// the OS the goldens were recorded on (see `assertScreenSnapshot`).
@MainActor
final class BlockingSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let size = CGSize(width: 393, height: 520)
    #else
    private static let size = CGSize(width: 520, height: 460)
    #endif

    private func blocked(_ addresses: [String], in sample: SampleSession) {
        let store = sample.coordinator.blockList
        store.seed(addresses.compactMap(JID.init(parsing:)), fetchedAt: store.versionBeforeFetch())
        store.load = .loaded
    }

    private func details(of peer: BareJID, in sample: SampleSession) -> some View {
        sample.host(
            Form {
                PeerBlockSection(peer: peer, name: sample.coordinator.directory.title(for: .direct(peer)))
            }
            .huddleForm()
        )
    }

    private func settings(in sample: SampleSession) -> some View {
        sample.host(
            Form { BlockedPeopleSection() }
                .huddleForm()
        )
    }

    func testSomeoneWhoCanBeBlocked() async throws {
        let sample = SampleSession()
        sample.coordinator.status.connection = .online
        blocked([], in: sample)
        try await assertScreenSnapshot(of: details(of: SampleSession.jo, in: sample), size: Self.size)
    }

    func testSomeoneWhoIsBlocked() async throws {
        let sample = SampleSession()
        sample.coordinator.status.connection = .online
        blocked([SampleSession.jo.description], in: sample)
        try await assertScreenSnapshot(of: details(of: SampleSession.jo, in: sample), size: Self.size)
    }

    /// A person the app knows, one it does not, and a whole server another client blocked.
    func testTheBlockedPeopleInSettings() async throws {
        let sample = SampleSession()
        sample.coordinator.status.connection = .online
        blocked([SampleSession.jo.description, "mallory@elsewhere.example", "spam.example"], in: sample)
        try await assertScreenSnapshot(of: settings(in: sample), size: Self.size)
    }

    func testNobodyBlocked() async throws {
        let sample = SampleSession()
        sample.coordinator.status.connection = .online
        blocked([], in: sample)
        try await assertScreenSnapshot(of: settings(in: sample), size: Self.size)
    }

    func testAListThatDidNotLoad() async throws {
        let sample = SampleSession()
        sample.coordinator.status.connection = .online
        sample.coordinator.blockList.load = .failed
        try await assertScreenSnapshot(of: settings(in: sample), size: Self.size)
    }
}
