import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for the thread screen: a thread whose reply
/// has a sub-thread, that sub-thread open with its breadcrumb, and a
/// reply's hover toolbar. Shown
/// as the inspector shows it on a Mac and as pushed on a phone. Compared on
/// the OS the goldens were recorded on; see `assertScreenSnapshot`.
@MainActor
final class ThreadScreenSnapshotTests: XCTestCase {
    func testAReplyWithASubThread() async throws {
        let sample = SampleSession()
        sample.seedThreadWithSubThread()
        try await assertScreenSnapshot(of: sample.host(Self.thread("root")), size: Self.size)
    }

    func testAnOpenSubThreadWithTheWayBack() async throws {
        let sample = SampleSession()
        sample.seedThreadWithSubThread()
        try await assertScreenSnapshot(of: sample.host(Self.thread("reply-2")), size: Self.size)
    }

    /// The pointer over Jo's reply: its toolbar, with "Reply in thread"
    /// starting a sub-thread on it.
    func testAReplysHoverToolbar() async throws {
        let sample = SampleSession()
        sample.seedThreadWithSubThread()
        let row = try XCTUnwrap(sample.coordinator.timelines.timeline(for: .room(SampleSession.releaseTrain)).item(withID: "reply-1"))
        let screen = Self.thread("root").environment(\.messageToolbarRow, row.presentationID)
        try await assertScreenSnapshot(of: sample.host(screen), size: Self.size)
    }

    private static func thread(_ rootID: String) -> some View {
        NavigationStack {
            ThreadScreen(conversation: .room(SampleSession.releaseTrain), rootID: rootID)
        }
        // The seeded messages are 11 to 15 minutes old at this time.
        .environment(\.threadHeaderNow, date(0))
    }

    #if os(macOS)
    /// About the inspector's width beside a conversation.
    private static let size = CGSize(width: 420, height: 620)
    #else
    private static let size = CGSize.phone
    #endif
}

extension SampleSession {
    /// A thread in release-train: Priya's root, two replies, and a
    /// sub-thread of two messages under Mika's reply.
    func seedThreadWithSubThread() {
        let room = Self.releaseTrain
        func message(_ id: String, _ nick: String, _ body: String, age: TimeInterval, thread: String?, parent: String? = nil) -> WireMessage {
            WireMessage(
                source: .archive(mamID: id),
                type: .groupchat,
                from: room.with(resource: nick)!,
                to: Self.ana.jid.with(resource: "phone")!,
                identity: MessageIdentity(
                    messageID: nil,
                    originID: nil,
                    stanzaID: StanzaID(id: id, by: room),
                    stanzaIDs: [StanzaID(id: id, by: room)]
                ),
                timestamp: date(-age),
                body: body,
                thread: thread,
                parentThread: parent
            )
        }
        for wire in [
            message("root", "Priya", "Shall we move the release to Thursday?", age: 900, thread: nil),
            message("reply-1", "Jo", "Thursday works for me.", age: 840, thread: "root"),
            message("reply-2", "Mika", "Android needs one more day for room history.", age: 780, thread: "root"),
            message("sub-1", "Theo", "I can pair on it tomorrow morning.", age: 720, thread: "reply-2", parent: "root"),
            message("sub-2", "Mika", "Perfect, I'll send an invite.", age: 660, thread: "reply-2", parent: "root"),
        ] {
            coordinator.timelines.ingest(wire)
        }
    }
}
