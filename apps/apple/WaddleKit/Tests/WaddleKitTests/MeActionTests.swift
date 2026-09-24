import Foundation
import Testing
@testable import WaddleKit

@MainActor
struct MeActionTests {
    @Test func parsesExactPrefix() {
        #expect(MeAction.parse(body: "/me shrugs in disgust") == "shrugs in disgust")
        #expect(MeAction.parse(body: "/me ") == "")
    }

    /// XEP-0245 §3 "Some Non-Commands", plus case and missing-space variants.
    @Test func nonCommandsAreNotActions() {
        let bodies = [
            "/meshrugs in disgust",
            "/me's disgusted",
            " /me shrugs in disgust",
            "\"/me shrugs in disgust\"",
            "* Atlas shrugs in disgust",
            "Why did Atlas say \"/me shrugs in disgust\"?",
            "/ME shrugs",
            "/me",
        ]
        for body in bodies {
            #expect(MeAction.parse(body: body) == nil, "\(body)")
        }
    }

    @Test func presentation() {
        #expect(MeAction.presentation(actor: "Atlas", action: "shrugs in disgust") == "* Atlas shrugs in disgust")
        #expect(MeAction.presentation(ofBody: "/me waves ", actor: "bob") == "* bob waves")
        #expect(MeAction.presentation(ofBody: "/me ", actor: "bob") == "* bob")
        #expect(MeAction.presentation(ofBody: "waves", actor: "bob") == nil)
    }

    @Test func roomAlertPreviewsMeAsAction() {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.status.connection = .online
        var alerts: [IncomingAlert] = []
        coordinator.onAlert = { alerts.append($0) }
        coordinator.directory.setNotifyMode(.always, for: roomConversation)
        coordinator.route(roomMessage("/me shrugs in disgust", from: "Atlas", stanzaID: "s1"))
        coordinator.route(roomMessage(" /me is not an action", from: "Atlas", stanzaID: "s2"))
        #expect(alerts.map(\.body) == ["* Atlas shrugs in disgust", "/me is not an action"])
    }

    @Test func directPreviewShowsMeAction() {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.status.connection = .online
        coordinator.route(directMessage("/me waves", from: jid("bob@waddle.test/laptop"), to: jid("alice@waddle.test/phone"), id: "d1"))
        #expect(coordinator.directory.directConversations.first?.preview == "* bob waves")
    }

    @Test func ownDirectSendPreviewsMeAction() async {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        await coordinator.send(Draft(text: "/me waves"), in: bobConversation)
        #expect(coordinator.directory.directConversations.first?.preview == "* alice waves")
    }
}
