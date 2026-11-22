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

    /// People is pushed on the phone's Home stack, so an iPad that shrinks
    /// to compact width while showing People still shows People.
    func testShowingPeopleLeavesTheConversationAndPushesItOnHome() {
        let navigation = NavigationModel()
        navigation.open(lobby)

        navigation.show(.people)

        XCTAssertNil(navigation.selection)
        XCTAssertEqual(navigation.splitSection, .people)
        XCTAssertEqual(navigation.tab, .home)
        XCTAssertEqual(navigation.homePath, [.people])
    }

    /// The phone opens People by pushing it on Home. An iPad that widens
    /// then must show People, not the conversation it showed before.
    func testPushingPeopleOnHomeShowsItInTheSplitViewToo() {
        let navigation = NavigationModel()
        navigation.open(lobby)

        navigation.homePath = [.people]

        XCTAssertNil(navigation.selection)
        XCTAssertEqual(navigation.splitSection, .people)
    }

    func testGoingBackFromPeopleOnThePhoneLeavesItInTheSplitViewToo() {
        let navigation = NavigationModel()
        navigation.homePath = [.people]

        navigation.homePath = []

        XCTAssertEqual(navigation.splitSection, .home)
    }

    /// Opening a room replaces People on the Home stack; the stack and
    /// its path never disagree.
    func testOpeningARoomFromPeopleReplacesItOnTheHomeStack() {
        let navigation = NavigationModel()
        navigation.show(.people)

        navigation.open(lobby)

        XCTAssertEqual(navigation.homePath, [.conversation(lobby)])
    }

    // MARK: - Sub-threads

    func testASubThreadReplacesItsParentInTheInspectorAndBack() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.openThread("root-1", in: lobby, usesInspector: true)

        navigation.openThread("reply-1", in: lobby, usesInspector: true)
        XCTAssertEqual(navigation.inspector, .thread(rootID: "reply-1"))

        navigation.openEnclosingThread("root-1", from: "reply-1", in: lobby, usesInspector: true)
        XCTAssertEqual(navigation.inspector, .thread(rootID: "root-1"))
    }

    func testBackFromASubThreadOnThePhonePopsToItsParent() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.openThread("root-1", in: lobby, usesInspector: false)
        navigation.openThread("reply-1", in: lobby, usesInspector: false)
        XCTAssertEqual(navigation.homePath, [.conversation(lobby), .thread(lobby, rootID: "root-1"), .thread(lobby, rootID: "reply-1")])

        navigation.openEnclosingThread("root-1", from: "reply-1", in: lobby, usesInspector: false)

        XCTAssertEqual(navigation.homePath, [.conversation(lobby), .thread(lobby, rootID: "root-1")])
    }

    /// The breadcrumb can go back more than one level.
    func testTheBreadcrumbPopsBackToAThreadFurtherDown() {
        let navigation = NavigationModel()
        navigation.open(lobby)
        navigation.openThread("root-1", in: lobby, usesInspector: false)
        navigation.openThread("reply-1", in: lobby, usesInspector: false)
        navigation.openThread("reply-2", in: lobby, usesInspector: false)

        navigation.openEnclosingThread("root-1", from: "reply-2", in: lobby, usesInspector: false)

        XCTAssertEqual(navigation.homePath, [.conversation(lobby), .thread(lobby, rootID: "root-1")])
    }

    /// Opened straight from the Inbox, a sub-thread has no parent below it.
    func testBackFromASubThreadOpenedFromTheInboxShowsItsParentInItsPlace() {
        let navigation = NavigationModel()
        navigation.tab = .inbox
        navigation.inboxPath = [.thread(lobby, rootID: "reply-1")]

        navigation.openEnclosingThread("root-1", from: "reply-1", in: lobby, usesInspector: false)

        XCTAssertEqual(navigation.inboxPath, [.thread(lobby, rootID: "root-1")])
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

    /// The contact list is asked for at the end of connecting, so an
    /// account that is online with no lookup started yet is loading.
    func testPeopleSaysLoadingNotOfflineOnceConnected() {
        XCTAssertEqual(PeopleLoadNotice(load: .idle, isOnline: true), .loading)
        XCTAssertEqual(PeopleLoadNotice(load: .idle, isOnline: false), .offline)
        XCTAssertEqual(PeopleLoadNotice(load: .loading, isOnline: true), .loading)
        XCTAssertEqual(PeopleLoadNotice(load: .failed, isOnline: true), .failed)
    }

    /// The phone opens the admin console from You, so an iPad that shrinks
    /// to compact width while showing it still shows it.
    func testShowingAdminLeavesTheConversationAndPushesItOnYou() {
        let navigation = NavigationModel()
        navigation.open(lobby)

        navigation.show(.admin)

        XCTAssertNil(navigation.selection)
        XCTAssertEqual(navigation.splitSection, .admin)
        XCTAssertEqual(navigation.tab, .you)
        XCTAssertEqual(navigation.youPath, [.admin])
    }

    func testPushingAdminOnYouShowsItInTheSplitViewToo() {
        let navigation = NavigationModel()
        navigation.open(lobby)

        navigation.youPath = [.admin]

        XCTAssertNil(navigation.selection)
        XCTAssertEqual(navigation.splitSection, .admin)
    }

    func testGoingBackFromAdminOnThePhoneLeavesItInTheSplitViewToo() {
        let navigation = NavigationModel()
        navigation.youPath = [.admin]

        navigation.youPath = []

        XCTAssertEqual(navigation.splitSection, .home)
    }

    /// People stays pushed on Home while Admin is open on You.
    func testGoingBackFromAdminReturnsToPeopleWhenHomeShowsIt() {
        let navigation = NavigationModel()
        navigation.homePath = [.people]
        navigation.youPath = [.admin]

        navigation.youPath = []

        XCTAssertEqual(navigation.splitSection, .people)
    }
}
