import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@Suite("Blocked entries")
struct BlockedEntryTests {
    private func names(_ person: BareJID) -> String? { person == bob ? "Bob" : nil }

    @Test func aPersonIsShownByNameWithTheirAddress() {
        let entry = BlockedEntry(jid("bob@waddle.test"), name: names)
        #expect(entry.title == "Bob")
        #expect(entry.detail == "bob@waddle.test")
        #expect(entry.unblockLabel == "Unblock Bob")
    }

    @Test func aPersonTheAppDoesNotKnowIsTheirAddress() {
        let entry = BlockedEntry(jid("mallory@elsewhere.example"), name: names)
        #expect(entry.title == "mallory@elsewhere.example")
        #expect(entry.detail == nil)
    }

    @Test func aNameThatIsOnlyTheAddressIsNotSaidTwice() {
        let entry = BlockedEntry(jid("mallory@elsewhere.example")) { $0.description }
        #expect(entry.title == "mallory@elsewhere.example")
        #expect(entry.detail == nil)
    }

    /// Another client can block a whole server, or one of a person's devices.
    @Test func aServerAndADeviceSayWhatTheyCover() {
        #expect(BlockedEntry(jid("spam.example"), name: names).title == "Everyone at spam.example")
        let device = BlockedEntry(jid("bob@waddle.test/phone"), name: names)
        #expect(device.title == "Bob on phone")
        #expect(device.detail == "bob@waddle.test/phone")
        #expect(BlockedEntry(jid("spam.example/bot"), name: names).title == "spam.example/bot")
    }
}
