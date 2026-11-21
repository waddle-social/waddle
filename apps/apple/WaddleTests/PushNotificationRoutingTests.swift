import Foundation
import UserNotifications
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

    @MainActor
    func testLocalAlertPostsWithThePushCollapseIdentifier() async {
        let center = RecordingMessageNotifications()
        let controller = NotificationController(messageCenter: center)
        await postRoomAlert(using: controller)
        XCTAssertEqual(center.requests.map(\.identifier), ["room-archive-42"])
    }

    @MainActor
    func testClearingAReadConversationRemovesItsDeliveredAlert() async {
        let center = RecordingMessageNotifications()
        let controller = NotificationController(messageCenter: center)
        await postRoomAlert(using: controller)
        let other = UNMutableNotificationContent()
        other.threadIdentifier = "direct:puffin@waddle.test"
        center.addMessage(UNNotificationRequest(identifier: "other-archive-42", content: other, trigger: nil))

        let removed = expectation(description: "Delivered alert removed")
        center.onRemoval = { removed.fulfill() }
        controller.clear(roomConversation)
        await fulfillment(of: [removed], timeout: 5)

        XCTAssertEqual(center.requests.map(\.identifier), ["other-archive-42"])
    }

    @MainActor
    private func postRoomAlert(using controller: NotificationController) async {
        let port = FakePort()
        let session = SessionCoordinator(account: me, port: port)
        session.directory.setNotifyMode(.always, for: roomConversation)
        controller.isAppActive = false
        let posted = expectation(description: "Local alert posted")
        session.onAlert = { alert in
            controller.post(alert, for: me.jid, isVisible: false)
            posted.fulfill()
        }
        session.start()
        port.emit(.message(roomMessage("hello", from: "bob", stanzaID: "room-archive-42", originID: "origin-42")))
        await fulfillment(of: [posted], timeout: 5)
        await session.stop()
    }
}

@MainActor
private final class RecordingMessageNotifications: MessageNotificationCenter {
    private(set) var requests: [UNNotificationRequest] = []
    var onRemoval: (() -> Void)?

    func addMessage(_ request: UNNotificationRequest) {
        requests.append(request)
    }

    func deliveredRequests() async -> [UNNotificationRequest] { requests }

    func removeDeliveredNotifications(withIdentifiers identifiers: [String]) {
        requests.removeAll { identifiers.contains($0.identifier) }
        onRemoval?()
    }
}
