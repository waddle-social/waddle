import XCTest
@testable import Waddle_macOS

final class RustXmppClientMappingTests: XCTestCase {
    func testAttemptFenceIncludesUUIDAndGeneration() {
        let baseline = attempt(id: "00000000-0000-4000-8000-000000000001", generation: 7)
        XCTAssertTrue(sameAttempt(baseline, baseline))
        XCTAssertFalse(
            sameAttempt(
                baseline,
                attempt(id: "00000000-0000-4000-8000-000000000002", generation: 7)
            )
        )
        XCTAssertFalse(
            sameAttempt(
                baseline,
                attempt(id: baseline.attemptId.value, generation: 8)
            )
        )
    }

    func testTypedDeliverySignalPreservesAttemptAndStanza() {
        let nativeAttempt = attempt(
            id: "00000000-0000-4000-8000-000000000001",
            generation: 11
        )
        let signal = makeDeliverySignal(
            WaddleNativeDeliverySignal(
                attempt: nativeAttempt,
                stanzaId: WaddleDeliveryStanzaId(value: "message-1")
            )
        )
        XCTAssertEqual(signal.stanzaID, "message-1")
        XCTAssertEqual(signal.attempt.id, nativeAttempt.attemptId.value)
        XCTAssertEqual(signal.attempt.connectionGeneration, 11)
    }

    func testReadyAttemptMatrixAcceptsExactAndSelfFencesStaleAttempts() {
        let exact = attempt(
            id: "00000000-0000-4000-8000-000000000001",
            generation: 7
        )
        let mapper = NativeXMPPEventMapper(expectedAttempt: exact)

        XCTAssertEqual(
            mapper.map(.sessionReady(kind: .fresh, attempt: exact)),
            .event(
                .sessionReady(
                    kind: .fresh,
                    attempt: XMPPDeliveryAttemptRef(
                        id: exact.attemptId.value,
                        connectionGeneration: 7
                    )
                )
            )
        )
        assertSelfFence(
            mapper.map(
                .sessionReady(
                    kind: .fresh,
                    attempt: attempt(
                        id: "00000000-0000-4000-8000-000000000002",
                        generation: 7
                    )
                )
            )
        )
        assertSelfFence(
            mapper.map(
                .sessionReady(
                    kind: .fresh,
                    attempt: attempt(id: exact.attemptId.value, generation: 8)
                )
            )
        )
    }

    func testAckAttemptMatrixAcceptsExactAndSelfFencesStaleAttempts() {
        let exact = attempt(
            id: "00000000-0000-4000-8000-000000000001",
            generation: 7
        )
        let mapper = NativeXMPPEventMapper(expectedAttempt: exact)

        XCTAssertEqual(
            mapper.map(.deliveryAcked(signal: signal(attempt: exact))),
            .event(
                .messageDeliveryAcked(
                    XMPPDeliverySignal(
                        attempt: XMPPDeliveryAttemptRef(
                            id: exact.attemptId.value,
                            connectionGeneration: 7
                        ),
                        stanzaID: "message-1"
                    )
                )
            )
        )
        assertSelfFence(
            mapper.map(
                .deliveryAcked(
                    signal: signal(
                        attempt: attempt(
                            id: "00000000-0000-4000-8000-000000000002",
                            generation: 7
                        )
                    )
                )
            )
        )
        assertSelfFence(
            mapper.map(
                .deliveryAcked(
                    signal: signal(
                        attempt: attempt(
                            id: exact.attemptId.value,
                            generation: 8
                        )
                    )
                )
            )
        )
    }

    func testFailureAttemptMatrixAcceptsExactAndSelfFencesStaleAttempts() {
        let exact = attempt(
            id: "00000000-0000-4000-8000-000000000001",
            generation: 7
        )
        let mapper = NativeXMPPEventMapper(expectedAttempt: exact)

        XCTAssertEqual(
            mapper.map(.deliveryFailed(signal: signal(attempt: exact))),
            .event(
                .messageDeliveryFailed(
                    XMPPDeliverySignal(
                        attempt: XMPPDeliveryAttemptRef(
                            id: exact.attemptId.value,
                            connectionGeneration: 7
                        ),
                        stanzaID: "message-1"
                    )
                )
            )
        )
        assertSelfFence(
            mapper.map(
                .deliveryFailed(
                    signal: signal(
                        attempt: attempt(
                            id: "00000000-0000-4000-8000-000000000002",
                            generation: 7
                        )
                    )
                )
            )
        )
        assertSelfFence(
            mapper.map(
                .deliveryFailed(
                    signal: signal(
                        attempt: attempt(
                            id: exact.attemptId.value,
                            generation: 8
                        )
                    )
                )
            )
        )
    }

    func testSnapshotAttemptMatrixConsumesExactAndSelfFencesStaleAttempts() {
        let exact = attempt(
            id: "00000000-0000-4000-8000-000000000001",
            generation: 7
        )
        let mapper = NativeXMPPEventMapper(expectedAttempt: exact)
        let snapshot = WaddleSmResumeState(
            previd: "resume-1",
            inboundH: 3,
            outboundH: 5,
            maxResumeSeconds: 60,
            queuedEntries: []
        )

        XCTAssertEqual(
            mapper.map(.resumeStateChanged(attempt: exact, state: snapshot)),
            .consumed
        )
        assertSelfFence(
            mapper.map(
                .resumeStateChanged(
                    attempt: attempt(
                        id: "00000000-0000-4000-8000-000000000002",
                        generation: 7
                    ),
                    state: snapshot
                )
            )
        )
        assertSelfFence(
            mapper.map(
                .resumeStateChanged(
                    attempt: attempt(id: exact.attemptId.value, generation: 8),
                    state: snapshot
                )
            )
        )
    }

    func testReadySaslAndBootstrapEnumsMapExhaustively() {
        XCTAssertEqual(makeSessionReadyKind(.fresh), .fresh)
        XCTAssertEqual(makeSessionReadyKind(.resumed), .resumed)
        XCTAssertEqual(
            XMPPSessionReadyKind.fresh.bootstrapPlan,
            .establishFreshSession
        )
        XCTAssertEqual(
            XMPPSessionReadyKind.resumed.bootstrapPlan,
            .preserveResumedSession
        )
        let expected: [(WaddleSaslCondition, XMPPSaslCondition)] = [
            (.aborted, .aborted),
            (.accountDisabled, .accountDisabled),
            (.credentialsExpired, .credentialsExpired),
            (.encryptionRequired, .encryptionRequired),
            (.incorrectEncoding, .incorrectEncoding),
            (.invalidAuthzid, .invalidAuthzid),
            (.invalidMechanism, .invalidMechanism),
            (.malformedRequest, .malformedRequest),
            (.mechanismTooWeak, .mechanismTooWeak),
            (.notAuthorized, .notAuthorized),
            (.temporaryAuthFailure, .temporaryAuthFailure),
            (.unknown, .unknown),
        ]
        XCTAssertEqual(expected.count, 12)
        for (native, domain) in expected {
            XCTAssertEqual(makeSaslCondition(native), domain)
        }
    }

    func testTypedSaslFailuresHaveExhaustiveRetryDispositions() {
        let expected: [(XMPPSaslCondition, XMPPSaslRetryDisposition)] = [
            (.temporaryAuthFailure, .retry),
            (.notAuthorized, .stopCredential),
            (.accountDisabled, .stopCredential),
            (.credentialsExpired, .stopCredential),
            (.invalidAuthzid, .stopCredential),
            (.invalidMechanism, .stopConfiguration),
            (.mechanismTooWeak, .stopConfiguration),
            (.encryptionRequired, .stopConfiguration),
            (.incorrectEncoding, .stopConfiguration),
            (.malformedRequest, .stopConfiguration),
            (.aborted, .stopAborted),
            (.unknown, .stopUnknown),
        ]

        XCTAssertEqual(expected.count, 12)
        for (condition, disposition) in expected {
            XCTAssertEqual(condition.retryDisposition, disposition)
        }
    }

    func testFirstTerminalFailureSurvivesDisconnectUntilNewLoginGeneration() {
        let first = updatedStoppedXMPPAuthentication(
            nil,
            loginGeneration: 4,
            condition: .notAuthorized
        )
        let preserved = updatedStoppedXMPPAuthentication(
            first,
            loginGeneration: 4,
            condition: .invalidMechanism
        )

        XCTAssertEqual(first, preserved)
        var admission = XMPPConnectionAdmission()
        for _ in 0..<4 {
            admission.open()
        }
        XCTAssertEqual(admission.generation, 4)
        XCTAssertFalse(
            xmppReconnectAllowed(admission: admission, stopped: preserved)
        )
        admission.open()
        XCTAssertTrue(
            xmppReconnectAllowed(admission: admission, stopped: preserved)
        )
        XCTAssertNil(
            updatedStoppedXMPPAuthentication(
                nil,
                loginGeneration: 4,
                condition: .temporaryAuthFailure
            )
        )
    }

    private func signal(
        attempt: WaddleDeliveryAttemptRef
    ) -> WaddleNativeDeliverySignal {
        WaddleNativeDeliverySignal(
            attempt: attempt,
            stanzaId: WaddleDeliveryStanzaId(value: "message-1")
        )
    }

    private func assertSelfFence(
        _ mapping: NativeXMPPEventMapping,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        guard case .selfFence(.error(_)) = mapping else {
            XCTFail("Expected an error self-fence, got \(mapping)", file: file, line: line)
            return
        }
    }

    private func attempt(id: String, generation: UInt64) -> WaddleDeliveryAttemptRef {
        WaddleDeliveryAttemptRef(
            attemptId: WaddleDeliveryAttemptId(value: id),
            connectionGeneration: WaddleConnectionGeneration(value: generation)
        )
    }
}
