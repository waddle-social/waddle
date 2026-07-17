import XCTest
@testable import Waddle_macOS

@MainActor
final class RustXmppClientLifecycleTests: XCTestCase {
    func testOneOutstandingPullAndNoPrefetchAcrossConsumerBarrier() async {
        let attempt = makeAttempt(generation: 1)
        let client = FakeNativeXMPPEventClient()
        let pump = makePump(client: client, attempt: attempt)
        let connected = await pump.connect()
        XCTAssertTrue(connected)

        let firstPull = Task { @MainActor in
            await pump.nextEvent()
        }
        await waitUntil { client.nextEventCalls == 1 }
        XCTAssertEqual(client.inFlightNextEvents, 1)
        XCTAssertEqual(client.maxInFlightNextEvents, 1)

        client.emit(.error(description: "first"))
        let firstResult = await firstPull.value
        XCTAssertEqual(firstResult, .event(.error("first")))

        for _ in 0..<5 {
            await Task.yield()
        }
        XCTAssertEqual(client.nextEventCalls, 1)
        XCTAssertEqual(client.inFlightNextEvents, 0)

        let secondPull = Task { @MainActor in
            await pump.nextEvent()
        }
        await waitUntil { client.nextEventCalls == 2 }
        XCTAssertEqual(client.maxInFlightNextEvents, 1)
        client.emit(.error(description: "second"))
        let secondResult = await secondPull.value
        XCTAssertEqual(secondResult, .event(.error("second")))
    }

    func testPendingPollDisconnectWakesAndJoinsWithOneNativeClose() async {
        let attempt = makeAttempt(generation: 1)
        let client = FakeNativeXMPPEventClient()
        let pump = makePump(client: client, attempt: attempt)
        let connected = await pump.connect()
        XCTAssertTrue(connected)

        let pull = Task { @MainActor in
            await pump.nextEvent()
        }
        await waitUntil { client.inFlightNextEvents == 1 }

        let firstClose = Task { @MainActor in
            await pump.disconnect()
        }
        let secondClose = Task { @MainActor in
            await pump.disconnect()
        }
        await firstClose.value
        await secondClose.value
        let pullResult = await pull.value

        XCTAssertEqual(pullResult, .closed)
        XCTAssertEqual(client.disconnectCalls, 1)
        XCTAssertEqual(client.inFlightNextEvents, 0)
        XCTAssertEqual(pump.state, .closed)
    }

    func testDisconnectDuringSuspendedConnectCannotInstallZombie() async {
        let attempt = makeAttempt(generation: 1)
        let client = FakeNativeXMPPEventClient()
        client.suspendConnect = true
        let pump = makePump(client: client, attempt: attempt)

        let connect = Task { @MainActor in
            await pump.connect()
        }
        await waitUntil { client.connectCalls == 1 }

        await pump.disconnect()
        client.finishConnect()
        let connected = await connect.value

        XCTAssertFalse(connected)
        XCTAssertEqual(client.disconnectCalls, 1)
        XCTAssertEqual(client.nextEventCalls, 0)
        XCTAssertEqual(pump.state, .closed)
    }

    func testOldGenerationCompletionAfterReplacementIsDropped() async {
        let attempt = makeAttempt(generation: 1)
        let client = FakeNativeXMPPEventClient()
        let pump = makePump(client: client, attempt: attempt)
        let firstConnected = await pump.connect()
        XCTAssertTrue(firstConnected)

        let oldPull = Task { @MainActor in
            await pump.nextEvent()
        }
        await waitUntil { client.inFlightNextEvents == 1 }
        client.wakePollOnDisconnect = false

        let replaceGeneration = Task { @MainActor in
            await pump.disconnect()
        }
        await waitUntil { client.disconnectCalls == 1 }
        client.emit(.error(description: "old-generation"))

        let oldResult = await oldPull.value
        await replaceGeneration.value
        XCTAssertEqual(oldResult, .closed)

        client.wakePollOnDisconnect = true
        let replacementConnected = await pump.connect()
        XCTAssertTrue(replacementConnected)
        let replacementPull = Task { @MainActor in
            await pump.nextEvent()
        }
        await waitUntil { client.nextEventCalls == 2 }
        client.emit(.error(description: "replacement"))
        let replacementResult = await replacementPull.value

        XCTAssertEqual(replacementResult, .event(.error("replacement")))
        XCTAssertEqual(client.maxInFlightNextEvents, 1)
    }

    func testImpossibleResumeFailedSelfFencesWithoutAnotherPoll() async {
        let oldAttempt = makeAttempt(generation: 1)
        let freshAttempt = makeAttempt(
            id: "00000000-0000-4000-8000-000000000002",
            generation: 2
        )
        let client = FakeNativeXMPPEventClient()
        client.enqueue(
            .resumeFailed(
                transition: WaddleDeliveryAttemptTransition(
                    old: oldAttempt,
                    fresh: freshAttempt
                ),
                affected: [WaddleDeliveryStanzaId(value: "message-1")]
            )
        )
        let pump = makePump(client: client, attempt: oldAttempt)
        let connected = await pump.connect()
        XCTAssertTrue(connected)

        let result = await pump.nextEvent()
        guard case .event(.error(let description)) = result else {
            return XCTFail("Expected an error event, got \(result)")
        }
        XCTAssertTrue(description.contains("Unexpected failed-resume transition"))
        XCTAssertEqual(client.nextEventCalls, 1)
        XCTAssertEqual(client.disconnectCalls, 1)
        XCTAssertEqual(pump.state, .closed)

        let afterFence = await pump.nextEvent()
        XCTAssertEqual(afterFence, .closed)
        XCTAssertEqual(client.nextEventCalls, 1)
        XCTAssertEqual(client.disconnectCalls, 1)
    }

    private func makePump(
        client: FakeNativeXMPPEventClient,
        attempt: WaddleDeliveryAttemptRef
    ) -> NativeXMPPEventPump {
        NativeXMPPEventPump(
            client: client,
            mapper: NativeXMPPEventMapper(expectedAttempt: attempt)
        )
    }

    private func makeAttempt(
        id: String = "00000000-0000-4000-8000-000000000001",
        generation: UInt64
    ) -> WaddleDeliveryAttemptRef {
        WaddleDeliveryAttemptRef(
            attemptId: WaddleDeliveryAttemptId(value: id),
            connectionGeneration: WaddleConnectionGeneration(value: generation)
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
private final class FakeNativeXMPPEventClient: NativeXMPPEventClient {
    var suspendConnect = false
    var wakePollOnDisconnect = true

    private(set) var connectCalls = 0
    private(set) var disconnectCalls = 0
    private(set) var nextEventCalls = 0
    private(set) var inFlightNextEvents = 0
    private(set) var maxInFlightNextEvents = 0

    private var connectContinuation: CheckedContinuation<Void, Never>?
    private var pendingPolls: [CheckedContinuation<WaddleClientEvent, Never>] = []
    private var queuedEvents: [WaddleClientEvent] = []

    func connect() async {
        connectCalls += 1
        guard suspendConnect else {
            return
        }
        await withCheckedContinuation { continuation in
            connectContinuation = continuation
        }
    }

    func disconnect() async {
        disconnectCalls += 1
        guard wakePollOnDisconnect, !pendingPolls.isEmpty else {
            return
        }
        resumeFirstPoll(with: .disconnected)
    }

    func nextEvent() async -> WaddleClientEvent {
        nextEventCalls += 1
        inFlightNextEvents += 1
        maxInFlightNextEvents = max(
            maxInFlightNextEvents,
            inFlightNextEvents
        )

        if !queuedEvents.isEmpty {
            inFlightNextEvents -= 1
            return queuedEvents.removeFirst()
        }

        return await withCheckedContinuation { continuation in
            pendingPolls.append(continuation)
        }
    }

    func finishConnect() {
        let continuation = connectContinuation
        connectContinuation = nil
        suspendConnect = false
        continuation?.resume()
    }

    func enqueue(_ event: WaddleClientEvent) {
        queuedEvents.append(event)
    }

    func emit(_ event: WaddleClientEvent) {
        guard !pendingPolls.isEmpty else {
            queuedEvents.append(event)
            return
        }
        resumeFirstPoll(with: event)
    }

    private func resumeFirstPoll(with event: WaddleClientEvent) {
        let continuation = pendingPolls.removeFirst()
        inFlightNextEvents -= 1
        continuation.resume(returning: event)
    }
}
