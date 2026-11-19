import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Session call lifecycle reports") struct CallLifecycleCoordinatorTests {
    @Test func sessionsWithoutCallHandlingNeverSendReportsEvenForManuallyDrivenCalls() async {
        let (coordinator, port) = session(handlesCalls: false)
        coordinator.handle(.connected)
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        await coordinator.calls.hangUp()
        #expect(coordinator.calls.declinePushedCall(sid: "unhandled-push"))
        await coordinator.stop()
        #expect(port.callLifecycleReports.isEmpty)
    }

    @Test func optedInOnlineSessionSendsTheCallOutcome() async {
        let (coordinator, port) = session()
        coordinator.handle(.connected)
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        await coordinator.calls.hangUp()
        await eventually { port.callLifecycleReports.count == 1 }
        #expect(port.callLifecycleReports == [proposed])
        await coordinator.stop()
        #expect(port.callLifecycleReports == [proposed])
    }

    @Test func optedInOfflineSessionFlushesOutcomesInOrderOnConnect() async {
        let (coordinator, port) = session()
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        await coordinator.calls.hangUp()
        #expect(coordinator.calls.declinePushedCall(sid: "offline-decline"))
        #expect(port.callLifecycleReports.isEmpty)
        coordinator.handle(.connected)
        await eventually { port.callLifecycleReports.count == 2 }
        #expect(port.callLifecycleReports == [proposed, declined])
        await coordinator.stop()
    }

    @Test func stopDropsOfflineQueueAndTheOutcomeProducedByTeardown() async {
        let (coordinator, port) = session()
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        await coordinator.calls.hangUp()
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        await coordinator.stop()
        #expect(coordinator.calls.state == .idle)
        #expect(port.callLifecycleReports.isEmpty)
        #expect(coordinator.calls.declinePushedCall(sid: "stopped-decline"))
        coordinator.start()
        port.emit(.connected)
        await eventually { coordinator.connection == .online }
        #expect(coordinator.calls.declinePushedCall(sid: "restarted-decline"))
        await eventually { port.callLifecycleReports.count == 1 }
        #expect(port.callLifecycleReports == [declined])
        await coordinator.stop()
    }

    @Test func authenticationFailureDropsQueuedAndFutureOutcomesUntilAConnection() async {
        let (coordinator, port) = session()
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        await coordinator.calls.hangUp()
        coordinator.handle(.authenticationFailed)
        #expect(coordinator.calls.declinePushedCall(sid: "failed-auth-decline"))
        #expect(port.callLifecycleReports.isEmpty)
        coordinator.start()
        port.emit(.connected)
        await eventually { coordinator.connection == .online }
        #expect(coordinator.calls.declinePushedCall(sid: "authenticated-decline"))
        await eventually { port.callLifecycleReports.count == 1 }
        #expect(port.callLifecycleReports == [declined])
        await coordinator.stop()
    }

    @Test func resettingAStaleTransportHoldsReportsBeforeDisconnectEventsAreConsumed() async {
        let (coordinator, port) = session()
        coordinator.handle(.connected)
        await coordinator.resetStaleConnection()
        #expect(port.disconnectCount == 1)
        // No event loop is running: the port's queued disconnect has not
        // reached the coordinator, so reset itself must stop report sends.
        #expect(coordinator.calls.declinePushedCall(sid: "reset-decline"))
        // Let a mistakenly scheduled online send run before supplying the
        // disconnect event; reset must already have marked the outbox offline.
        for _ in 0..<10 { await Task.yield() }
        #expect(port.callLifecycleReports.isEmpty)
        coordinator.handle(.disconnected)
        coordinator.handle(.connected)
        await eventually { port.callLifecycleReports.count == 1 }
        #expect(port.callLifecycleReports == [declined])
        await coordinator.stop()
    }

    private var proposed: CallLifecycleReport {
        CallLifecycleReport(kind: .direct, setupOutcome: .proposed, endReason: .hangup, duration: .none, reconnects: .none)
    }

    private var declined: CallLifecycleReport {
        CallLifecycleReport(kind: .direct, setupOutcome: .declined, endReason: .hangup, duration: .none, reconnects: .none)
    }

    private func session(handlesCalls: Bool = true) -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource),
            port: port, handlesCalls: handlesCalls)
        return (coordinator, port)
    }
}
