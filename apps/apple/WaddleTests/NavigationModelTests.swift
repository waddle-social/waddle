import WaddleKit
import XCTest
@testable import Waddle

@MainActor
final class NavigationModelTests: XCTestCase {
    private let lobby = ConversationID.room(BareJID(parsing: "lobby@muc.waddle.test")!)
    private let design = ConversationID.room(BareJID(parsing: "design@muc.waddle.test")!)

    func testThreadOpenedWithItsRoomSurvivesTheSelectionChange() {
        let navigation = NavigationModel()
        navigation.show(.inbox)

        navigation.open(lobby)
        navigation.openThread("root-1", in: lobby, usesInspector: true)

        XCTAssertEqual(navigation.selection, lobby)
        XCTAssertEqual(navigation.inspector, .thread(rootID: "root-1"))
    }

    func testSelectingAnotherConversationClosesTheThread() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.openThread("root-1", in: lobby, usesInspector: true)

        navigation.selection = design

        XCTAssertNil(navigation.inspector)
    }

    func testSelectingAnotherConversationKeepsDetailsAndPins() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.inspector = .pins

        navigation.selection = design

        XCTAssertEqual(navigation.inspector, .pins)
    }

    func testFocusingAMessageInTheSameConversationKeepsTheThread() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.openThread("root-1", in: lobby, usesInspector: true)

        navigation.focus("message-1", in: lobby)

        XCTAssertEqual(navigation.inspector, .thread(rootID: "root-1"))
    }

    func testShowingHomeReturnsThePhoneStackToItsRoot() {
        let navigation = NavigationModel()
        navigation.open(lobby)

        navigation.show(.home)

        XCTAssertEqual(navigation.tab, .home)
        XCTAssertEqual(navigation.homePath, [])
    }

    func testShowingTheFeedLeavesTheConversationAndFollowsOnThePhone() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.inspector = .details

        navigation.show(.feed)

        XCTAssertNil(navigation.selection)
        XCTAssertNil(navigation.inspector)
        XCTAssertEqual(navigation.splitSection, .feed)
        XCTAssertEqual(navigation.tab, .feed)
    }

    func testShowingEventsLeavesTheConversationAndOpensThemFromThePhonesFeed() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.inspector = .details

        navigation.show(.events)

        XCTAssertNil(navigation.selection)
        XCTAssertNil(navigation.inspector)
        XCTAssertEqual(navigation.splitSection, .events)
        XCTAssertEqual(navigation.tab, .feed)
        XCTAssertEqual(navigation.feedPath, [.events])
    }

    func testShowingTheFeedReturnsThePhonesFeedToItsRoot() {
        let navigation = NavigationModel()
        navigation.show(.events)

        navigation.show(.feed)

        XCTAssertEqual(navigation.splitSection, .feed)
        XCTAssertEqual(navigation.tab, .feed)
        XCTAssertEqual(navigation.feedPath, [])
    }

    func testShowingInboxReturnsThePhoneStackToItsRoot() {
        let navigation = NavigationModel()
        navigation.inboxPath = [.conversation(lobby)]

        navigation.show(.inbox)

        XCTAssertEqual(navigation.tab, .inbox)
        XCTAssertEqual(navigation.inboxPath, [])
    }

    func testClosingTheSelectedConversationSelectsNothingAndClosesTheInspector() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.inspector = .details

        navigation.close(lobby)

        XCTAssertNil(navigation.selection)
        XCTAssertNil(navigation.inspector)
    }

    func testClosingAnotherConversationKeepsTheSelection() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.inspector = .details

        navigation.close(design)

        XCTAssertEqual(navigation.selection, lobby)
        XCTAssertEqual(navigation.inspector, .details)
    }

    func testClosingAConversationPopsEveryPhoneStackBelowIt() {
        let navigation = NavigationModel()
        navigation.homePath = [.conversation(design), .conversation(lobby), .details(lobby)]
        navigation.inboxPath = [.thread(lobby, rootID: "root-1")]
        navigation.directPath = [.conversation(.direct(BareJID(parsing: "mika@waddle.test")!))]

        navigation.close(lobby)

        XCTAssertEqual(navigation.homePath, [.conversation(design)])
        XCTAssertEqual(navigation.inboxPath, [])
        XCTAssertEqual(navigation.directPath.count, 1)
    }

    func testClosingAConversationDismissesItsSearch() {
        let navigation = NavigationModel()
        navigation.sheet = .search(lobby)

        navigation.close(lobby)

        XCTAssertNil(navigation.sheet)
    }
}
