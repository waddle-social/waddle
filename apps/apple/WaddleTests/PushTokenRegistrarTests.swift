import Foundation
@testable import WaddleKit
import XCTest
@testable import Waddle

/// Turning push off and on again, against a signed-in session over the
/// scripted `FakePort`.
@MainActor
final class PushTokenRegistrarTests: XCTestCase {
    private final class Host: PushSessionHost {
        let server = URL(string: "https://push-registrar-tests.waddle.test")!
        var pushCoordinator: SessionCoordinator?
    }

    private let stored = PushRegistration(serviceJID: "push.waddle.test", node: "registrar-test", deviceID: "old-device")
    private let fresh = PushRegistration(serviceJID: "push.waddle.test", node: "registrar-test", deviceID: "new-device")
    private let token = Data([0xAB, 0xCD])

    private var host: Host!
    private var port: FakePort!
    private var registrar: PushTokenRegistrar!
    private var owner: PushRegistrationStore.Owner!

    override func setUp() async throws {
        port = FakePort()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: BareJID(parsing: "penguin@waddle.test")!, nick: "penguin"), port: port)
        coordinator.status.connection = .online
        host = Host()
        host.pushCoordinator = coordinator
        owner = PushRegistrationStore.Owner(server: host.server, account: coordinator.account.jid)
        PushRegistrationStore.save(stored, token: "abcd", environment: .sandbox, appID: "waddle.test", for: owner)
        registrar = PushTokenRegistrar(kind: .alert)
        registrar.attach(host)
    }

    override func tearDown() async throws {
        PushRegistrationStore.forget(owner)
        for registration in PushRegistrationStore.pendingRetirements(for: owner) {
            PushRegistrationStore.removePendingRetirement(registration, for: owner)
        }
    }

    func testTurningPushOffRetiresTheStoredRegistration() async throws {
        registrar.turnOff()
        try await eventually { self.port.disabledPush == [self.stored] }
        XCTAssertNil(PushRegistrationStore.registration(for: owner))
        XCTAssertEqual(PushRegistrationStore.pendingRetirements(for: owner), [])
    }

    func testATokenThatArrivesWhilePushIsOffRegistersNothing() async throws {
        registrar.turnOff()
        try await eventually { !self.port.disabledPush.isEmpty }
        registrar.didRegister(deviceToken: token)
        registrar.startPendingRegistrationIfPossible()
        await settle()
        XCTAssertEqual(port.pushRegistrationRequests, [])
        XCTAssertEqual(port.enabledPush, [])
    }

    /// The server did not confirm the retirement, so it stays queued and
    /// is sent again when the session is next online, with push still off.
    func testAnUnconfirmedRetirementIsRetriedWhilePushIsOff() async throws {
        port.disablesPush = false
        registrar.turnOff()
        try await eventually { !self.port.disabledPush.isEmpty }
        await settle()
        XCTAssertNil(PushRegistrationStore.registration(for: owner))
        XCTAssertEqual(PushRegistrationStore.pendingRetirements(for: owner), [stored])

        port.disablesPush = true
        registrar.startPendingRegistrationIfPossible()
        try await eventually { PushRegistrationStore.pendingRetirements(for: self.owner).isEmpty }
        XCTAssertEqual(port.pushRegistrationRequests, [])
    }

    /// Push goes back on while a retirement is still queued: the old
    /// device row is retired before a new one is registered and stored.
    func testTurningPushOnAgainRetiresWhatIsQueuedThenRegisters() async throws {
        port.disablesPush = false
        registrar.turnOff()
        try await eventually { !self.port.disabledPush.isEmpty }
        await settle()

        port.disablesPush = true
        port.pushRegistrationResult = fresh
        registrar.setEnabled(true)
        registrar.didRegister(deviceToken: token)
        try await eventually { PushRegistrationStore.registration(for: self.owner) == self.fresh }
        XCTAssertEqual(PushRegistrationStore.pendingRetirements(for: owner), [])
        XCTAssertEqual(port.pushRegistrationRequests, ["abcd"])
        XCTAssertEqual(port.disabledPush.last, stored)
    }

    /// Lets the registrar's main-actor tasks run to a standstill.
    private func settle() async {
        for _ in 0..<50 { await Task.yield() }
    }

    private func eventually(
        _ condition: @MainActor () -> Bool, file: StaticString = #filePath, line: UInt = #line
    ) async throws {
        for _ in 0..<2000 {
            if condition() { return }
            await Task.yield()
        }
        XCTFail("The condition never held", file: file, line: line)
        throw CancellationError()
    }
}
