import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Call lifecycle outbox") struct CallLifecycleOutboxTests {
    private func report(_ outcome: CallLifecycleReport.SetupOutcome) -> CallLifecycleReport {
        CallLifecycleReport(kind: .direct, setupOutcome: outcome, endReason: .hangup, duration: .none, reconnects: .none)
    }

    @Test func onlineSubmissionsSendImmediately() async {
        let port = FakePort()
        let outbox = CallLifecycleOutbox(port: port)
        outbox.setOnline(true)
        let report = report(.proposed)
        outbox.submit(report)
        await eventually { port.callLifecycleReports == [report] }
        #expect(port.callLifecycleReports == [report])
    }

    @Test func offlineReportsFlushInOrderOneAtATime() async {
        let port = FakePort()
        port.holdCallLifecycleReports = true
        let outbox = CallLifecycleOutbox(port: port)
        let first = report(.proposed)
        let second = report(.declined)
        outbox.submit(first)
        outbox.submit(second)
        #expect(port.callLifecycleReports.isEmpty)
        outbox.setOnline(true)
        await eventually { port.callLifecycleReports.count == 1 }
        #expect(port.callLifecycleReports == [first])
        port.holdCallLifecycleReports = false
        port.releaseCallLifecycleReports()
        await eventually { port.callLifecycleReports.count == 2 }
        #expect(port.callLifecycleReports == [first, second])
    }

    @Test func reconnectDropsOnlyReportsOlderThanOneHour() async {
        let port = FakePort()
        var now = ContinuousClock.now
        let outbox = CallLifecycleOutbox(port: port, now: { now })
        outbox.submit(report(.failed))
        now = now.advanced(by: .seconds(1))
        let boundary = report(.proposed)
        outbox.submit(boundary)
        now = now.advanced(by: .seconds(3600))
        let recent = report(.declined)
        outbox.submit(recent)
        outbox.setOnline(true)
        await eventually { port.callLifecycleReports.count == 2 }
        #expect(port.callLifecycleReports == [boundary, recent])
    }

    @Test(arguments: [CallLifecycleReportResult.accepted, .unsupported, .rateLimited, .failed])
    func handedOffReportsAreNeverRetried(result: CallLifecycleReportResult) async {
        let port = FakePort()
        port.callLifecycleReportResult = result
        var now = ContinuousClock.now
        let outbox = CallLifecycleOutbox(port: port, now: { now })
        let first = report(.proposed)
        outbox.setOnline(true)
        outbox.submit(first)
        await eventually { port.callLifecycleReports.count == 1 }
        outbox.setOnline(false)
        now = now.advanced(by: .seconds(600))
        port.callLifecycleReportResult = .accepted
        outbox.setOnline(true)
        let second = report(.declined)
        outbox.submit(second)
        await eventually { port.callLifecycleReports.count == 2 }
        #expect(port.callLifecycleReports == [first, second])
    }

    @Test func unsupportedStopsTheQueueAndDropsNewSubmissionsUntilConnected() async {
        let port = FakePort()
        port.callLifecycleReportResult = .unsupported
        let outbox = CallLifecycleOutbox(port: port)
        let first = report(.proposed)
        let queued = report(.accepted)
        outbox.submit(first)
        outbox.submit(queued)
        outbox.setOnline(true)
        await eventually { port.callLifecycleReports.count == 1 }
        outbox.submit(report(.failed))
        port.callLifecycleReportResult = .accepted
        outbox.setOnline(false)
        outbox.submit(report(.timeout))
        outbox.setOnline(true)
        let last = report(.declined)
        outbox.submit(last)
        await eventually { port.callLifecycleReports.count == 3 }
        #expect(port.callLifecycleReports == [first, queued, last])
    }

    @Test func rateLimitDropsTheQueueAndPausesForTenMinutesAcrossReconnects() async {
        let port = FakePort()
        port.callLifecycleReportResult = .rateLimited
        var now = ContinuousClock.now
        let outbox = CallLifecycleOutbox(port: port, now: { now })
        let first = report(.proposed)
        outbox.submit(first)
        outbox.submit(report(.failed))
        outbox.setOnline(true)
        await eventually { port.callLifecycleReports.count == 1 }
        outbox.submit(report(.timeout))
        now = now.advanced(by: .seconds(599))
        outbox.setOnline(false)
        outbox.setOnline(true)
        outbox.submit(report(.accepted))
        now = now.advanced(by: .seconds(1))
        port.callLifecycleReportResult = .accepted
        let last = report(.declined)
        outbox.submit(last)
        await eventually { port.callLifecycleReports.count == 2 }
        #expect(port.callLifecycleReports == [first, last])
    }

    @Test func disconnectHoldsReportsNotYetHandedToThePort() async {
        let port = FakePort()
        port.holdCallLifecycleReports = true
        let outbox = CallLifecycleOutbox(port: port)
        let first = report(.proposed)
        let second = report(.declined)
        outbox.setOnline(true)
        outbox.submit(first)
        outbox.submit(second)
        await eventually { port.callLifecycleReports.count == 1 }
        outbox.setOnline(false)
        port.holdCallLifecycleReports = false
        port.releaseCallLifecycleReports()
        // A reconnect is also the completion barrier for the held queue.
        outbox.setOnline(true)
        await eventually { port.callLifecycleReports.count == 2 }
        #expect(port.callLifecycleReports == [first, second])
    }

    @Test func stopEmptiesOfflineReports() async {
        let port = FakePort()
        let outbox = CallLifecycleOutbox(port: port)
        outbox.submit(report(.failed))
        outbox.stop()
        outbox.submit(report(.timeout))
        let last = report(.declined)
        outbox.setOnline(true)
        outbox.submit(last)
        await eventually { port.callLifecycleReports.count == 1 }
        #expect(port.callLifecycleReports == [last])
    }

    @Test(arguments: [CallLifecycleReportResult.unsupported, .rateLimited])
    func stopFencesSuspendedResults(result: CallLifecycleReportResult) async {
        let port = FakePort()
        port.callLifecycleReportResult = result
        port.holdCallLifecycleReports = true
        let outbox = CallLifecycleOutbox(port: port)
        let first = report(.proposed)
        outbox.setOnline(true)
        outbox.submit(first)
        outbox.submit(report(.failed))
        await eventually { port.callLifecycleReports.count == 1 }
        outbox.stop()
        outbox.setOnline(true)
        let last = report(.declined)
        outbox.submit(last)
        port.callLifecycleReportResult = .accepted
        port.holdCallLifecycleReports = false
        port.releaseCallLifecycleReports()
        await eventually { port.callLifecycleReports.count == 2 }
        #expect(port.callLifecycleReports == [first, last])
    }

    @Test func reconnectFencesAnEarlierUnsupportedResult() async {
        let port = FakePort()
        port.callLifecycleReportResult = .unsupported
        port.holdCallLifecycleReports = true
        let outbox = CallLifecycleOutbox(port: port)
        let first = report(.proposed)
        outbox.setOnline(true)
        outbox.submit(first)
        await eventually { port.callLifecycleReports.count == 1 }
        outbox.setOnline(false)
        outbox.setOnline(true)
        let last = report(.declined)
        outbox.submit(last)
        port.callLifecycleReportResult = .accepted
        port.holdCallLifecycleReports = false
        port.releaseCallLifecycleReports()
        await eventually { port.callLifecycleReports.count == 2 }
        #expect(port.callLifecycleReports == [first, last])
    }
}
