import SwiftUI
import XCTest

@testable import Waddle
@testable import WaddleKit

@MainActor
final class SidebarSpacesSnapshotTests: XCTestCase {
    func testSidebarWithAnExpandedSpaceAndUnreadDiscussionV2() async throws {
        try await assertSidebar(studioExpanded: true)
    }

    func testSidebarWithACollapsedSpaceAndItsRollupsV2() async throws {
        try await assertSidebar(studioExpanded: false)
    }

    private func assertSidebar(studioExpanded: Bool, testName: String = #function) async throws {
        let sample = SampleSession()
        let session = sample.coordinator
        let spaces = BareJID(parsing: "spaces.waddle.test")!
        let studio = Space(id: "studio", serviceJID: spaces, name: "Design studio", summary: nil)
        let planning = Space(id: "planning", serviceJID: spaces, name: "Planning", summary: nil)
        session.directory.apply(
            Topology(
                spaces: [studio, planning],
                channels: [
                    Channel(roomJID: SampleSession.design, name: "design", spaceID: studio.id),
                    Channel(roomJID: SampleSession.designCritique, name: "design-critique", kind: .forum, position: 1, spaceID: studio.id),
                    Channel(roomJID: SampleSession.releaseTrain, name: "release-train", spaceID: planning.id),
                    Channel(roomJID: SampleSession.general, name: "general"),
                ]
            ))
        session.isCommunityOwner = true
        session.unread.liveMessage(in: .room(SampleSession.designCritique), isMine: false, mentionsMe: true)
        session.applyInbox(
            InboxEntry(
                partner: SampleSession.design, kind: .room, lastStanzaID: "reply-1", lastUpdated: 100, unread: 3,
                preview: nil, thread: .init(id: "icon-review", title: "Icon review")
            ))
        for nick in ["Jo", "Priya"] {
            session.calls.groupPresence.apply(
                WirePresence(
                    from: SampleSession.design.with(resource: nick)!, kind: .available,
                    groupCall: GroupCallMarker(isPreparing: false, media: CallMedia(audio: true, video: false))
                ))
        }

        let suite = "SidebarSpacesSnapshotTests.\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let expansion = SidebarSpaceExpansion(defaults: defaults)
        expansion.setExpanded(studioExpanded, group: .space(studio.id), for: session.account.jid)
        expansion.setExpanded(false, group: .space(planning.id), for: session.account.jid)

        try await assertScreenSnapshot(
            of: sample.host(Sidebar(spaceExpansion: expansion)),
            size: CGSize(width: 280, height: 840),
            testName: testName
        )
    }
}
