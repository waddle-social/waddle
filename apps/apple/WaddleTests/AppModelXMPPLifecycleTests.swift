import XCTest
@testable import Waddle_macOS

@MainActor
final class AppModelXMPPLifecycleTests: XCTestCase {
    func testLogoutInvalidatesBeforeDisconnectAndJoinsCanceledConsumer() async throws {
        let controller = XMPPClientLifecycleController<FakeManagedXMPPClient>()
        let session = XMPPLoginSessionIdentity(value: "session-a")
        let client = FakeManagedXMPPClient()
        controller.openLogin(sessionIdentity: session)
        let initialGeneration = await controller.beginReplacement(
            sessionIdentity: session
        )
        let generation = try XCTUnwrap(initialGeneration)
        XCTAssertTrue(
            controller.install(
                client: client,
                loginGeneration: generation,
                sessionIdentity: session
            )
        )

        var consumerCompleted = false
        XCTAssertTrue(
            controller.startConsumer(
                for: client,
                loginGeneration: generation,
                sessionIdentity: session
            ) {
                await client.waitForDisconnectEvent()
                consumerCompleted = true
            }
        )
        await waitUntil { client.isWaitingForDisconnectEvent }

        var invalidatedBeforeDisconnect = false
        client.onDisconnect = {
            invalidatedBeforeDisconnect =
                controller.currentClient == nil
                && !controller.reconnectAllowed
                && !controller.admitsLogin(
                    generation: generation,
                    sessionIdentity: session
                )
        }

        await controller.closeLogin()

        XCTAssertTrue(invalidatedBeforeDisconnect)
        XCTAssertTrue(consumerCompleted)
        XCTAssertEqual(client.disconnectCalls, 1)
        XCTAssertNil(controller.currentClient)
    }

    func testLogoutDropsExpectedOldEventWithoutMutationOrReconnect() async throws {
        let controller = XMPPClientLifecycleController<FakeManagedXMPPClient>()
        let session = XMPPLoginSessionIdentity(value: "session-a")
        let client = FakeManagedXMPPClient()
        controller.openLogin(sessionIdentity: session)
        let initialGeneration = await controller.beginReplacement(
            sessionIdentity: session
        )
        let generation = try XCTUnwrap(initialGeneration)
        XCTAssertTrue(
            controller.install(
                client: client,
                loginGeneration: generation,
                sessionIdentity: session
            )
        )

        var staleMutations = 0
        var reconnectSchedules = 0
        XCTAssertTrue(
            controller.startConsumer(
                for: client,
                loginGeneration: generation,
                sessionIdentity: session
            ) {
                await client.waitForDisconnectEvent()
                guard
                    !Task.isCancelled,
                    controller.admits(
                        client: client,
                        loginGeneration: generation,
                        sessionIdentity: session
                    )
                else {
                    return
                }
                staleMutations += 1
                if controller.reconnectAllowed {
                    reconnectSchedules += 1
                }
            }
        )
        await waitUntil { client.isWaitingForDisconnectEvent }

        await controller.closeLogin()

        XCTAssertEqual(staleMutations, 0)
        XCTAssertEqual(reconnectSchedules, 0)
        XCTAssertFalse(controller.reconnectAllowed)
        XCTAssertEqual(client.disconnectCalls, 1)
    }

    func testReplacementJoinsOldConsumerAndAdmitsOnlyNewClient() async throws {
        let controller = XMPPClientLifecycleController<FakeManagedXMPPClient>()
        let session = XMPPLoginSessionIdentity(value: "session-a")
        let oldClient = FakeManagedXMPPClient()
        let newClient = FakeManagedXMPPClient()
        controller.openLogin(sessionIdentity: session)
        let initialGeneration = await controller.beginReplacement(
            sessionIdentity: session
        )
        let generation = try XCTUnwrap(initialGeneration)
        XCTAssertTrue(
            controller.install(
                client: oldClient,
                loginGeneration: generation,
                sessionIdentity: session
            )
        )

        var oldConsumerCompleted = false
        XCTAssertTrue(
            controller.startConsumer(
                for: oldClient,
                loginGeneration: generation,
                sessionIdentity: session
            ) {
                await oldClient.waitForDisconnectEvent()
                oldConsumerCompleted = true
            }
        )
        await waitUntil { oldClient.isWaitingForDisconnectEvent }

        let replacementGenerationValue = await controller.beginReplacement(
            sessionIdentity: session
        )
        let replacementGeneration = try XCTUnwrap(
            replacementGenerationValue
        )
        XCTAssertTrue(oldConsumerCompleted)
        XCTAssertEqual(oldClient.disconnectCalls, 1)
        XCTAssertFalse(
            controller.admits(
                client: oldClient,
                loginGeneration: generation,
                sessionIdentity: session
            )
        )
        XCTAssertTrue(
            controller.install(
                client: newClient,
                loginGeneration: replacementGeneration,
                sessionIdentity: session
            )
        )
        XCTAssertTrue(
            controller.admits(
                client: newClient,
                loginGeneration: replacementGeneration,
                sessionIdentity: session
            )
        )
        XCTAssertFalse(
            controller.admits(
                client: oldClient,
                loginGeneration: replacementGeneration,
                sessionIdentity: session
            )
        )
    }

    func testTerminalSaslStopsReconnectWhileTemporarySaslPermitsIt() {
        let controller = XMPPClientLifecycleController<FakeManagedXMPPClient>()
        let session = XMPPLoginSessionIdentity(value: "session-a")
        controller.openLogin(sessionIdentity: session)

        let temporary = controller.recordAuthenticationFailure(
            .temporaryAuthFailure
        )
        XCTAssertEqual(temporary.disposition, .retry)
        XCTAssertFalse(temporary.wasAlreadyStopped)
        XCTAssertTrue(controller.reconnectAllowed)

        let terminal = controller.recordAuthenticationFailure(.notAuthorized)
        XCTAssertEqual(terminal.disposition, .stopCredential)
        XCTAssertFalse(terminal.wasAlreadyStopped)
        XCTAssertFalse(controller.reconnectAllowed)

        let repeated = controller.recordAuthenticationFailure(.invalidMechanism)
        XCTAssertEqual(repeated.disposition, .stopConfiguration)
        XCTAssertTrue(repeated.wasAlreadyStopped)
        XCTAssertFalse(controller.reconnectAllowed)
    }

    func testLogoutDuringSuspendedPresenceDoesNotAdmitRoomLoad() async throws {
        let controller = XMPPClientLifecycleController<FakeManagedXMPPClient>()
        let session = XMPPLoginSessionIdentity(value: "session-a")
        let client = FakeManagedXMPPClient()
        let presence = SuspendedPresence()
        controller.openLogin(sessionIdentity: session)
        let initialGeneration = await controller.beginReplacement(
            sessionIdentity: session
        )
        let generation = try XCTUnwrap(initialGeneration)
        XCTAssertTrue(
            controller.install(
                client: client,
                loginGeneration: generation,
                sessionIdentity: session
            )
        )

        var roomMutations = 0
        XCTAssertTrue(
            controller.startConsumer(
                for: client,
                loginGeneration: generation,
                sessionIdentity: session
            ) {
                await controller.performFreshBootstrap(
                    client: client,
                    loginGeneration: generation,
                    sessionIdentity: session,
                    sendPresence: {
                        await presence.wait()
                    },
                    admitRoomLoad: {
                        roomMutations += 1
                    }
                )
            }
        )
        await waitUntil { presence.isWaiting }

        let logout = Task { @MainActor in
            await controller.closeLogin()
        }
        await waitUntil { client.disconnectCalls == 1 }
        presence.finish()
        await logout.value

        XCTAssertEqual(roomMutations, 0)
        XCTAssertNil(controller.currentClient)
        XCTAssertFalse(controller.reconnectAllowed)
    }

    func testReplacementDuringSuspendedPresenceDoesNotAdmitRoomLoad() async throws {
        let controller = XMPPClientLifecycleController<FakeManagedXMPPClient>()
        let session = XMPPLoginSessionIdentity(value: "session-a")
        let oldClient = FakeManagedXMPPClient()
        let newClient = FakeManagedXMPPClient()
        let presence = SuspendedPresence()
        controller.openLogin(sessionIdentity: session)
        let initialGeneration = await controller.beginReplacement(
            sessionIdentity: session
        )
        let generation = try XCTUnwrap(initialGeneration)
        XCTAssertTrue(
            controller.install(
                client: oldClient,
                loginGeneration: generation,
                sessionIdentity: session
            )
        )

        var roomMutations = 0
        XCTAssertTrue(
            controller.startConsumer(
                for: oldClient,
                loginGeneration: generation,
                sessionIdentity: session
            ) {
                await controller.performFreshBootstrap(
                    client: oldClient,
                    loginGeneration: generation,
                    sessionIdentity: session,
                    sendPresence: {
                        await presence.wait()
                    },
                    admitRoomLoad: {
                        roomMutations += 1
                    }
                )
            }
        )
        await waitUntil { presence.isWaiting }

        let replacement = Task { @MainActor in
            await controller.beginReplacement(sessionIdentity: session)
        }
        await waitUntil { oldClient.disconnectCalls == 1 }
        presence.finish()
        let replacementGenerationValue = await replacement.value
        let replacementGeneration = try XCTUnwrap(
            replacementGenerationValue
        )

        XCTAssertEqual(roomMutations, 0)
        XCTAssertTrue(
            controller.install(
                client: newClient,
                loginGeneration: replacementGeneration,
                sessionIdentity: session
            )
        )
        XCTAssertTrue(
            controller.admits(
                client: newClient,
                loginGeneration: replacementGeneration,
                sessionIdentity: session
            )
        )
    }

    private func waitUntil(
        _ predicate: @MainActor () -> Bool,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        for _ in 0..<200 {
            if predicate() {
                return
            }
            await Task.yield()
        }
        XCTFail("Timed out waiting for lifecycle state", file: file, line: line)
    }
}

@MainActor
private final class FakeManagedXMPPClient: XMPPManagedClient {
    private(set) var disconnectCalls = 0
    private(set) var isWaitingForDisconnectEvent = false
    var onDisconnect: (() -> Void)?

    private var disconnectEventContinuation: CheckedContinuation<Void, Never>?

    func disconnect() async {
        disconnectCalls += 1
        onDisconnect?()
        let continuation = disconnectEventContinuation
        disconnectEventContinuation = nil
        isWaitingForDisconnectEvent = false
        continuation?.resume()
    }

    func waitForDisconnectEvent() async {
        isWaitingForDisconnectEvent = true
        await withCheckedContinuation { continuation in
            disconnectEventContinuation = continuation
        }
    }
}

@MainActor
private final class SuspendedPresence {
    private(set) var isWaiting = false
    private var continuation: CheckedContinuation<Void, Never>?

    func wait() async {
        isWaiting = true
        await withCheckedContinuation { continuation in
            self.continuation = continuation
        }
        isWaiting = false
    }

    func finish() {
        let continuation = continuation
        self.continuation = nil
        continuation?.resume()
    }
}
