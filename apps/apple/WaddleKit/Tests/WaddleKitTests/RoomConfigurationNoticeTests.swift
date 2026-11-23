import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// XEP-0045 §10.2.1: only the room itself says its configuration changed,
/// in a groupchat message from its bare address that carries status 104
/// and nothing to show. Anything else is an ordinary message.
@Suite("Room configuration notices")
struct RoomConfigurationNoticeTests {
    private func notice(
        from: String = "general@muc.waddle.test", isGroupchat: Bool = true,
        hasBody: Bool = false, statusCodes: [UInt16] = [104]
    ) -> BareJID? {
        XmppEvent.roomConfigurationChange(
            from: JID(parsing: from), isGroupchat: isGroupchat, hasBody: hasBody, statusCodes: statusCodes)
    }

    @Test func theRoomsOwnNoticeNamesTheRoom() {
        #expect(notice() == room)
    }

    @Test func anOccupantCannotSendIt() {
        #expect(notice(from: "general@muc.waddle.test/mallory") == nil)
    }

    @Test func aMessageWithABodyIsNoNotice() {
        #expect(notice(hasBody: true) == nil)
    }

    @Test func onlyAGroupchatMessageIsANotice() {
        #expect(notice(isGroupchat: false) == nil)
    }

    @Test func otherStatusCodesAreNoNotice() {
        #expect(notice(statusCodes: []) == nil)
        #expect(notice(statusCodes: [170]) == nil)
    }
}
