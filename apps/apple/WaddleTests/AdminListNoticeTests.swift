import WaddleKit
import XCTest
@testable import Waddle

/// What a list of the admin console says in place of its rows.
final class AdminListNoticeTests: XCTestCase {
    func testRowsAreShownWithoutANotice() {
        XCTAssertNil(AdminListNotice(isEmpty: false, hasLoaded: true, failure: nil, query: ""))
        XCTAssertNil(AdminListNotice(isEmpty: false, hasLoaded: true, failure: .timeout, query: ""))
    }

    /// "No one" is only said once the server has said so.
    func testAListNotLoadedYetIsLoadingNotEmpty() {
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: nil, query: ""), .loading)
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: nil, query: "ada"), .loading)
    }

    func testAnEmptyAnswerIsNoOneOrNoMatches() {
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: true, failure: nil, query: " "), .empty)
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: true, failure: nil, query: "ada"), .noMatches)
    }

    func testSomeoneWhoIsNotAnOwnerIsToldSo() {
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: .rejected, query: ""), .notAllowed)
    }

    func testAFailureSaysWhetherItIsTheConnection() {
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: .notConnected, query: ""), .offline)
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: .timeout, query: ""), .failed)
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: true, failure: .failed, query: "ada"), .failed)
    }
}

/// Which of the server's answers mean "not an owner".
final class AdminErrorTests: XCTestCase {
    func testOnlyForbiddenMeansNotAnOwner() {
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.Stanza(condition: "forbidden", text: nil)), .rejected)
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.Stanza(condition: "internal-server-error", text: nil)), .failed)
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.Stanza(condition: "bad-request", text: nil)), .failed)
    }

    func testOtherErrorsKeepTheirMeaning() {
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.NotConnected), .notConnected)
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.Timeout), .timeout)
    }
}
