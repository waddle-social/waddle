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

    func testShowingInboxReturnsThePhoneStackToItsRoot() {
        let navigation = NavigationModel()
        navigation.inboxPath = [.conversation(lobby)]

        navigation.show(.inbox)

        XCTAssertEqual(navigation.tab, .inbox)
        XCTAssertEqual(navigation.inboxPath, [])
    }
}
