import SwiftUI
import XCTest

@testable import Waddle
@testable import WaddleKit

/// The room directory in Night and Daylight, using the same screen on both shells.
@MainActor
final class BrowseRoomsSnapshotTests: XCTestCase {
    func testRoomsWithHuddlesPreviewsAndActions() async throws {
        let sample = SampleSession()
        let session = sample.coordinator
        session.isCommunityOwner = true
        session.directory.upsert(
            Channel(
                roomJID: BareJID(parsing: "workshop@rooms.waddle.test")!, name: "workshop", autojoin: false
            ))
        for (room, nick, peer) in [
            (SampleSession.general, "Priya", SampleSession.priya),
            (SampleSession.general, "Jo", SampleSession.jo),
            (SampleSession.releaseTrain, "Mika", SampleSession.mika),
        ] {
            let presence = WirePresence(
                from: room.with(resource: nick)!, kind: .available,
                occupant: .init(affiliation: .member, role: .participant, realJID: peer.with(resource: "desktop"), statusCodes: []),
                groupCall: GroupCallMarker(isPreparing: false, media: CallMedia(audio: true, video: true))
            )
            session.calls.groupPresence.apply(presence)
        }
        for room in [SampleSession.general, SampleSession.designCritique] {
            _ = session.presence.apply(
                WirePresence(
                    from: room.with(resource: "Ana")!, kind: .available,
                    occupant: .init(affiliation: .member, role: .participant, realJID: nil, statusCodes: [110])
                ))
        }
        preview("Notes from the huddle are in the thread.", in: SampleSession.general, session: session)
        preview("Jo: @ana, can you check the release notes?", in: SampleSession.releaseTrain, session: session)
        preview("The new icons are ready to review.", in: SampleSession.design, session: session)

        try await assertScreenSnapshot(of: page(sample), size: CGSize(width: 640, height: 580))
    }

    func testEmptyDirectory() async throws {
        let sample = SampleSession()
        sample.coordinator.directory.apply(.empty)
        try await assertScreenSnapshot(of: page(sample), size: CGSize(width: 640, height: 480))
    }

    private func page(_ sample: SampleSession) -> some View {
        sample.host(BrowseRoomsView())
            #if os(iOS)
        .environment(\.horizontalSizeClass, .regular)
            #endif
    }

    private func preview(_ text: String, in room: BareJID, session: SessionCoordinator) {
        _ = session.inbox.apply(
            InboxEntry(
                partner: room, kind: .room, lastStanzaID: "latest", lastUpdated: 100, unread: 0, preview: text
            ))
    }
}
