import Foundation
import WaddleKit
import XCTest
@testable import Waddle

final class PushNotificationRoutingTests: XCTestCase {
    func testRemoteRouteRetainsServerAndAccount() throws {
        let owner = PushRegistrationStore.Owner(
            server: URL(string: "https://routing-tests.waddle.test")!,
            account: BareJID(parsing: "penguin@waddle.test")!
        )
        let registration = PushRegistration(serviceJID: "push.waddle.test", node: "routing-test-node", deviceID: "routing-test-device")
        defer { PushRegistrationStore.forget(owner, kind: .voip) }
        PushRegistrationStore.save(registration, token: "token", environment: .sandbox, appID: "test.voip", for: owner, kind: .voip)

        let target = try XCTUnwrap(NotificationController.target(from: [
            "waddle": ["v": 1, "class": "dm", "node": registration.node, "conversation": "puffin@waddle.test"]
        ]))
        XCTAssertEqual(target.owner, owner)
        XCTAssertEqual(target.account, owner.account)
        XCTAssertEqual(target.conversation, .direct(BareJID(parsing: "puffin@waddle.test")!))
    }

    func testLocalRouteKeepsItsAccountAndConversation() throws {
        let target = try XCTUnwrap(NotificationController.target(from: [
            "account": "penguin@waddle.test", "conversation": "puffin@waddle.test", "kind": "direct"
        ]))
        XCTAssertNil(target.owner)
        XCTAssertEqual(target.account, BareJID(parsing: "penguin@waddle.test"))
        XCTAssertEqual(target.conversation, .direct(BareJID(parsing: "puffin@waddle.test")!))
    }

    func testReadClearingCoversALocalAlertAndAPush() {
        let room = ConversationID.room(BareJID(parsing: "ice@rooms.waddle.test")!)
        XCTAssertEqual(
            NotificationController.threadIdentifiers(of: room),
            ["room:ice@rooms.waddle.test", "ice@rooms.waddle.test"]
        )
    }
}
