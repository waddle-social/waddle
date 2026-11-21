import Foundation
import WaddleKit
import XCTest
@testable import Waddle

@MainActor
final class PushRegistrationStoreTests: XCTestCase {
    private let owner = PushRegistrationStore.Owner(
        server: URL(string: "https://push-store-tests.waddle.test")!,
        account: BareJID(parsing: "penguin@waddle.test")!
    )
    private let alert = PushRegistration(serviceJID: "push.waddle.test", node: "store-test-alert", deviceID: "alert-device")

    func testAlertRegistrationKeepsItsExistingDefaultsKeys() throws {
        defer { PushRegistrationStore.forget(owner) }
        PushRegistrationStore.save(alert, token: "alert-token", environment: .sandbox, appID: "waddle.test", for: owner)

        let key = "\(owner.server.absoluteString)|\(owner.account)"
        let registrations = UserDefaults.standard.dictionary(forKey: "waddle.apple.push-registrations") as? [String: Data]
        let contexts = UserDefaults.standard.dictionary(forKey: "waddle.apple.push-registration-contexts") as? [String: Data]
        XCTAssertEqual(try JSONDecoder().decode(PushRegistration.self, from: XCTUnwrap(registrations?[key])), alert)
        XCTAssertNotNil(contexts?[key])
        XCTAssertTrue(PushRegistrationStore.contextMatches(token: "alert-token", environment: .sandbox, appID: "waddle.test", for: owner))
        XCTAssertFalse(PushRegistrationStore.contextMatches(token: "alert-token", environment: .production, appID: "waddle.test", for: owner))
    }

    func testRetirementSurvivesForgettingTheRegistration() {
        defer { PushRegistrationStore.removePendingRetirement(alert, for: owner) }
        PushRegistrationStore.save(alert, token: "alert-token", environment: .sandbox, appID: "waddle.test", for: owner)
        PushRegistrationStore.enqueueRetirement(alert, for: owner)
        PushRegistrationStore.enqueueRetirement(alert, for: owner)
        PushRegistrationStore.forget(owner)

        XCTAssertNil(PushRegistrationStore.registration(for: owner))
        XCTAssertEqual(PushRegistrationStore.pendingRetirements(for: owner), [alert])
    }

    func testKindsKeepIndependentRegistrationContextAndRetirements() {
        let voip = PushRegistration(serviceJID: "push.waddle.test", node: "store-test-voip", deviceID: "voip-device")
        defer {
            for kind in PushTokenKind.allCases {
                PushRegistrationStore.forget(owner, kind: kind)
                PushRegistrationStore.removePendingRetirement(alert, for: owner, kind: kind)
                PushRegistrationStore.removePendingRetirement(voip, for: owner, kind: kind)
            }
        }
        PushRegistrationStore.save(alert, token: "alert-token", environment: .sandbox, appID: "waddle.test", for: owner)
        PushRegistrationStore.save(voip, token: "voip-token", environment: .production, appID: "waddle.test.voip", for: owner, kind: .voip)
        PushRegistrationStore.enqueueRetirement(alert, for: owner)
        PushRegistrationStore.enqueueRetirement(voip, for: owner, kind: .voip)

        XCTAssertEqual(PushRegistrationStore.registration(for: owner), alert)
        XCTAssertEqual(PushRegistrationStore.registration(for: owner, kind: .voip), voip)
        XCTAssertTrue(PushRegistrationStore.contextMatches(token: "voip-token", environment: .production, appID: "waddle.test.voip", for: owner, kind: .voip))
        XCTAssertFalse(PushRegistrationStore.contextMatches(token: "voip-token", environment: .production, appID: "waddle.test.voip", for: owner))
        XCTAssertEqual(PushRegistrationStore.pendingRetirements(for: owner), [alert])
        XCTAssertEqual(PushRegistrationStore.pendingRetirements(for: owner, kind: .voip), [voip])

        let registrations = UserDefaults.standard.dictionary(forKey: "waddle.apple.push-registrations") as? [String: Data]
        XCTAssertNotNil(registrations?["\(owner.server.absoluteString)|\(owner.account)|voip"])
        PushRegistrationStore.forget(owner, kind: .voip)
        XCTAssertEqual(PushRegistrationStore.registration(for: owner), alert)
        XCTAssertNil(PushRegistrationStore.registration(for: owner, kind: .voip))
    }

    func testNodeLookupReturnsTheWholeOwnerAcrossKindsAndServers() {
        let otherServer = PushRegistrationStore.Owner(server: URL(string: "https://other-push-store-tests.waddle.test")!, account: owner.account)
        let otherAccount = PushRegistrationStore.Owner(server: owner.server, account: BareJID(parsing: "puffin@waddle.test")!)
        let voip = PushRegistration(serviceJID: "push.waddle.test", node: "store-test-voip", deviceID: "voip-device")
        let secondAccount = PushRegistration(serviceJID: "push.waddle.test", node: "store-test-other-account", deviceID: "other-device")
        defer {
            PushRegistrationStore.forget(owner)
            PushRegistrationStore.forget(otherServer, kind: .voip)
            PushRegistrationStore.forget(otherAccount)
        }
        PushRegistrationStore.save(alert, token: "alert-token", environment: .sandbox, appID: "waddle.test", for: owner)
        PushRegistrationStore.save(voip, token: "voip-token", environment: .sandbox, appID: "waddle.test.voip", for: otherServer, kind: .voip)
        PushRegistrationStore.save(secondAccount, token: "alert-token", environment: .sandbox, appID: "waddle.test", for: otherAccount)

        XCTAssertEqual(PushRegistrationStore.account(forNode: alert.node), owner)
        XCTAssertEqual(PushRegistrationStore.account(forNode: voip.node), otherServer)
        XCTAssertEqual(PushRegistrationStore.account(forNode: secondAccount.node), otherAccount)
        XCTAssertNil(PushRegistrationStore.account(forNode: "store-test-missing"))
    }

    func testNodeLookupDoesNotChooseBetweenDistinctOwners() {
        let other = PushRegistrationStore.Owner(server: URL(string: "https://other-push-store-tests.waddle.test")!, account: owner.account)
        defer {
            PushRegistrationStore.forget(owner)
            PushRegistrationStore.forget(other, kind: .voip)
        }
        PushRegistrationStore.save(alert, token: "alert-token", environment: .sandbox, appID: "waddle.test", for: owner)
        PushRegistrationStore.save(alert, token: "voip-token", environment: .sandbox, appID: "waddle.test.voip", for: other, kind: .voip)

        XCTAssertNil(PushRegistrationStore.account(forNode: alert.node))
    }

    func testInvalidatingVoIPRetiresEveryOwnerAndKeepsAlertRegistrations() {
        let keys = [
            "waddle.apple.push-registrations", "waddle.apple.push-registration-contexts",
            "waddle.apple.push-tokens", "waddle.apple.push-retirements"
        ]
        let saved = keys.map { UserDefaults.standard.object(forKey: $0) }
        defer {
            for (key, value) in zip(keys, saved) {
                if let value { UserDefaults.standard.set(value, forKey: key) }
                else { UserDefaults.standard.removeObject(forKey: key) }
            }
        }
        let other = PushRegistrationStore.Owner(server: URL(string: "https://other-push-store-tests.waddle.test")!, account: owner.account)
        let first = PushRegistration(serviceJID: "push.waddle.test", node: "store-test-first-voip", deviceID: "first-voip-device")
        let second = PushRegistration(serviceJID: "push.waddle.test", node: "store-test-second-voip", deviceID: "second-voip-device")
        PushRegistrationStore.save(alert, token: "alert-token", environment: .sandbox, appID: "waddle.test", for: owner)
        PushRegistrationStore.save(first, token: "voip-token", environment: .sandbox, appID: "waddle.test.voip", for: owner, kind: .voip)
        PushRegistrationStore.save(second, token: "voip-token", environment: .sandbox, appID: "waddle.test.voip", for: other, kind: .voip)

        PushRegistrationStore.retireAll(kind: .voip)

        XCTAssertEqual(PushRegistrationStore.registration(for: owner), alert)
        XCTAssertTrue(PushRegistrationStore.contextMatches(token: "alert-token", environment: .sandbox, appID: "waddle.test", for: owner))
        XCTAssertNil(PushRegistrationStore.registration(for: owner, kind: .voip))
        XCTAssertNil(PushRegistrationStore.registration(for: other, kind: .voip))
        XCTAssertFalse(PushRegistrationStore.contextMatches(token: "voip-token", environment: .sandbox, appID: "waddle.test.voip", for: owner, kind: .voip))
        XCTAssertTrue(PushRegistrationStore.pendingRetirements(for: owner, kind: .voip).contains(first))
        XCTAssertTrue(PushRegistrationStore.pendingRetirements(for: other, kind: .voip).contains(second))
    }
}
