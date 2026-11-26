import XCTest
@testable import Waddle

/// Which conversation actions sit in the toolbar and which fold into its
/// More menu, so an iPhone's bar leaves the title room to be read.
final class ConversationToolbarLayoutTests: XCTestCase {
    /// iPhone room: call and search stay, pins and details fold away
    /// (Android's room bar: search and an overflow menu).
    func testAPhoneRoomFoldsPinsAndDetailsIntoMore() {
        let layout = ConversationToolbarLayout(isRoom: true, isCompact: true)
        XCTAssertEqual(layout.inline, [.call, .search])
        XCTAssertEqual(layout.more, [.pins, .details])
    }

    /// iPhone 1:1: voice and video share one call menu, as Android's DM
    /// bar has one call button. Pins and details fold away as in a room.
    func testAPhoneDirectChatSharesOneCallMenu() {
        let layout = ConversationToolbarLayout(isRoom: false, isCompact: true)
        XCTAssertEqual(layout.inline, [.call, .search])
        XCTAssertEqual(layout.more, [.pins, .details])
    }

    /// iPad and Mac have the width for every action.
    func testAWideRoomShowsEveryAction() {
        let layout = ConversationToolbarLayout(isRoom: true, isCompact: false)
        XCTAssertEqual(layout.inline, [.call, .search, .pins, .details])
        XCTAssertEqual(layout.more, [])
    }

    func testAWideDirectChatHasVoiceAndVideoButtons() {
        let layout = ConversationToolbarLayout(isRoom: false, isCompact: false)
        XCTAssertEqual(layout.inline, [.voiceCall, .videoCall, .search, .pins, .details])
        XCTAssertEqual(layout.more, [])
    }
}
