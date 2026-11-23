import WaddleKit
import XCTest
@testable import Waddle

/// What the Rust core's records become in the app's model.
final class InboundBridgeTests: XCTestCase {
    private func roomPresence(statusCodes: [UInt16], destroyed: Bool) -> WaddlePresence {
        WaddlePresence(
            from: "general@muc.waddle.test/alice", to: "alice@waddle.test/mac", presenceType: "unavailable",
            show: nil, status: nil, hats: [], mucAffiliation: WaddleMucAffiliation.none, mucRole: WaddleMucRole.none,
            mucJid: nil, mucStatusCodes: statusCodes, mucDestroyed: destroyed, vcardAvatar: nil, idleSince: nil,
            errorCondition: nil, errorType: nil, errorText: nil, handRaised: false, muted: false, muji: nil)
    }

    /// XEP-0045 §10.9 Example 205: a destruction may carry no status code,
    /// and is still a room presence that says the room is gone.
    func testARoomsDestructionCrossesTheBridge() throws {
        let presence = try XCTUnwrap(FFIInbound.wirePresence(roomPresence(statusCodes: [], destroyed: true)))
        XCTAssertEqual(presence.kind, .unavailable)
        XCTAssertEqual(presence.occupant?.roomDestroyed, true)
    }

    func testAnOrdinaryLeaveIsNoDestruction() throws {
        let presence = try XCTUnwrap(FFIInbound.wirePresence(roomPresence(statusCodes: [110], destroyed: false)))
        XCTAssertEqual(presence.occupant?.roomDestroyed, false)
        XCTAssertEqual(presence.occupant?.isSelf, true)
    }
}
