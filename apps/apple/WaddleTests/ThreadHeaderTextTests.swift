import WaddleKit
import XCTest
@testable import Waddle

/// The thread header's line under the title.
final class ThreadHeaderTextTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    private func header(startedBy: String?, lastActivity: Date?) -> ThreadHeader {
        var root: TimelineItem?
        if let startedBy {
            let room = BareJID(parsing: "lobby@muc.waddle.test")!
            let message = WireMessage(
                source: .live,
                type: .groupchat,
                from: room.with(resource: startedBy),
                to: nil,
                identity: MessageIdentity(messageID: "root", originID: nil, stanzaID: nil, stanzaIDs: []),
                timestamp: lastActivity,
                body: "Root"
            )
            root = TimelineItem(id: "root", conversation: .room(room), isMine: false, message: message, body: "Root")
        }
        return ThreadHeader(root: root, replies: [], replyCount: 4, breadcrumb: [])
    }

    func testEveryPart() {
        let parts = ThreadHeaderText.parts(header(startedBy: "priya", lastActivity: now.addingTimeInterval(-13 * 60)), now: now)
        XCTAssertEqual(parts.lead, ["Started by priya", "4 replies"])
        // The unit's wording follows the device's language and region
        // ("13 min. ago", "13m ago"); the number does not.
        XCTAssertTrue(parts.activity?.hasPrefix("active 13") == true, "\(parts.activity ?? "nil")")
    }

    /// A reply stamped a little ahead of this device's clock.
    func testATimeAheadOfTheClockIsNow() {
        let parts = ThreadHeaderText.parts(header(startedBy: "priya", lastActivity: now.addingTimeInterval(5)), now: now)
        XCTAssertEqual(parts.activity, "active now")
    }

    func testAMissingPartLeavesNoSeparator() {
        let parts = ThreadHeaderText.parts(header(startedBy: nil, lastActivity: nil), now: now)
        XCTAssertEqual(parts.lead, ["4 replies"])
        XCTAssertNil(parts.activity)
    }

    func testAThreadWithoutItsFirstMessageIsCalledThread() {
        XCTAssertEqual(ThreadHeaderText.title(nil), "Thread")
        XCTAssertEqual(ThreadHeaderText.title(.text("")), "Thread")
        XCTAssertEqual(ThreadHeaderText.title(.text("Ship it")), "Ship it")
    }
}
