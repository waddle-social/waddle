import WaddleKit
import XCTest
@testable import Waddle

/// The affiliation lists a room's member list is read from (XEP-0045 §9.5).
final class RoomMemberTiersTests: XCTestCase {
    private let ana = RoomMember(jid: bare("ana@waddle.test"), nick: "Ana", affiliation: .owner)
    private let jo = RoomMember(jid: bare("jo@waddle.test"), nick: "Jo", affiliation: .member)
    private let mallory = RoomMember(jid: bare("mallory@waddle.test"), nick: nil, affiliation: .outcast)

    /// Owners and admins may read who is banned, and see them listed.
    func testTheBannedAreListedForThoseWhoMayReadThem() async throws {
        let members = try await FFIXmppPort.mergedTiers { [ana, jo, mallory] tier in
            switch tier {
            case .owner: return [ana]
            case .member: return [jo]
            case .outcast: return [mallory]
            case .admin, .none: return []
            }
        }

        XCTAssertEqual(members, [ana, jo, mallory])
    }

    /// Everyone else is refused the banned (and here the owners and
    /// admins too), and still sees the members.
    func testARefusedBanListStillListsTheMembers() async throws {
        let members = try await FFIXmppPort.mergedTiers { [jo] tier in
            guard tier == .member else { throw PortError.rejected }
            return [jo]
        }

        XCTAssertEqual(members, [jo])
    }
}
