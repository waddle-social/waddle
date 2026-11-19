/// Only reports not yet handed to the port may wait for a connection.
@MainActor public final class CallLifecycleOutbox {
    private struct Entry {
        let report: CallLifecycleReport
        let madeAt: ContinuousClock.Instant
    }

    private let port: any CallPort
    private let now: @MainActor () -> ContinuousClock.Instant
    private var queue: [Entry] = []
    private var online = false
    private var stopped = false
    private var unsupported = false
    private var pausedUntil: ContinuousClock.Instant?
    private var generation = 0
    private var connectionRevision = 0
    private var drainTask: Task<Void, Never>?

    public init(
        port: any CallPort,
        now: @escaping @MainActor () -> ContinuousClock.Instant = { ContinuousClock.now }
    ) {
        self.port = port
        self.now = now
    }

    public func submit(_ report: CallLifecycleReport) {
        guard !stopped, !unsupported, !isPaused else { return }
        queue.append(Entry(report: report, madeAt: now()))
        startSending()
    }

    /// A fresh connection may advertise support that the previous one lacked.
    public func setOnline(_ online: Bool) {
        self.online = online
        guard online else { return }
        stopped = false
        connectionRevision += 1
        unsupported = false
        let connectedAt = now()
        queue.removeAll { $0.madeAt.duration(to: connectedAt) > .seconds(3600) }
        startSending()
    }

    /// A suspended port call has already claimed its report; its result cannot
    /// apply a support or rate-limit decision to a later signed-in session.
    public func stop() {
        generation += 1
        stopped = true
        online = false
        unsupported = false
        pausedUntil = nil
        queue.removeAll()
    }

    private var isPaused: Bool {
        guard let pausedUntil else { return false }
        if now() < pausedUntil { return true }
        self.pausedUntil = nil
        return false
    }

    private func startSending() {
        guard online, !unsupported, !isPaused, !queue.isEmpty, drainTask == nil else { return }
        drainTask = Task { [weak self] in
            await self?.sendQueuedReports()
        }
    }

    private func sendQueuedReports() async {
        while online, !unsupported, !isPaused, !queue.isEmpty {
            // Removal precedes the suspension: no outcome can retry this report.
            let entry = queue.removeFirst()
            let expectedGeneration = generation
            let expectedConnection = connectionRevision
            let result = await port.reportCallLifecycle(entry.report)
            guard generation == expectedGeneration else { continue }
            switch result {
            case .accepted, .failed: break
            case .unsupported:
                if connectionRevision == expectedConnection { unsupported = true }
            case .rateLimited:
                queue.removeAll()
                pausedUntil = now().advanced(by: .seconds(600))
            }
        }
        drainTask = nil
    }
}
