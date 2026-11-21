import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// The account's contact list, which the People view lists offline
/// contacts from.
@MainActor
@Suite("Contact list")
struct ContactListTests {
    private let ada = Contact(jid: bare("ada@waddle.test"), name: "Ada Lovelace")

    private func connected(_ contacts: Result<[Contact], PortError>) async -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        port.contacts = contacts
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        port.emit(.connected)
        await eventually { coordinator.isSendReady }
        await coordinator.readyTask?.value
        return (coordinator, port)
    }

    @Test func theContactListIsNotKnownBeforeItIsLoaded() {
        #expect(SessionCoordinator(account: me, port: FakePort()).contacts == nil)
    }

    @Test func theContactListIsLoadedOnConnecting() async {
        let (coordinator, _) = await connected(.success([ada]))

        #expect(coordinator.contacts == [ada])
        await coordinator.stop()
    }

    /// "No contacts" is an answer; "the lookup failed" is not one.
    @Test func aLookupThatFailsLeavesTheListUnknown() async {
        let (coordinator, _) = await connected(.failure(.timeout))

        #expect(coordinator.contacts == nil)
        #expect(coordinator.contactsLoad == .failed)
        await coordinator.stop()
    }

    /// Nothing retries a failed lookup by itself, so the screen offers to.
    @Test func tryingAgainAfterAFailedLookupLoadsTheList() async {
        let (coordinator, port) = await connected(.failure(.timeout))
        port.contacts = .success([ada])

        await coordinator.refreshContacts()

        #expect(coordinator.contacts == [ada])
        #expect(coordinator.contactsLoad == .idle)
        await coordinator.stop()
    }

    @Test func aLaterLookupThatFailsKeepsTheListAlreadyHeld() async {
        let (coordinator, port) = await connected(.success([ada]))
        port.contacts = .failure(.timeout)

        await coordinator.refreshContacts()

        #expect(coordinator.contacts == [ada])
        #expect(coordinator.contactsLoad == .failed)
        await coordinator.stop()
    }

    @Test func signingOutForgetsTheContactList() async {
        let (coordinator, _) = await connected(.success([ada]))

        await coordinator.stop()

        #expect(coordinator.contacts == nil)
    }
}
