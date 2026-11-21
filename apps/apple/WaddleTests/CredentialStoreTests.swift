import Foundation
import Security
import XCTest
@testable import Waddle

final class CredentialStoreTests: XCTestCase {
    func testOnlyItemNotFoundMeansMissing() {
        XCTAssertEqual(CredentialStore.sessionIDRead(status: errSecItemNotFound, data: nil), .missing)
        for status in [errSecInteractionNotAllowed, errSecAuthFailed, errSecNotAvailable, errSecDecode] {
            XCTAssertEqual(CredentialStore.sessionIDRead(status: status, data: nil), .unavailable)
        }
    }

    func testSuccessfulReadRequiresAValidCredential() {
        XCTAssertEqual(CredentialStore.sessionIDRead(status: errSecSuccess, data: Data("session".utf8)), .stored("session"))
        XCTAssertEqual(CredentialStore.sessionIDRead(status: errSecSuccess, data: nil), .unavailable)
        XCTAssertEqual(CredentialStore.sessionIDRead(status: errSecSuccess, data: Data([0xff])), .unavailable)
        XCTAssertEqual(CredentialStore.sessionIDRead(status: errSecSuccess, data: Data()), .unavailable)
    }
}
