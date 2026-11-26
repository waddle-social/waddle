import Foundation

extension CallStore {
    /// Returns nil when quitting needn't wait. Otherwise, calls `onComplete`
    /// when hang-up finishes or the deadline is reached.
    public func hangUpForTermination(
        timeout: TimeInterval = 2, runLoopMode: RunLoop.Mode = .default,
        onComplete: @escaping @MainActor @Sendable () -> Void
    ) -> ContinuousClock.Instant? {
        guard let sid = sidToHangUpOnTermination else { return nil }
        let epoch = generation
        let deadline = ContinuousClock.now + .seconds(timeout)

        // A run-loop timer can finish quitting even if a nested loop can't
        // resume the main actor's queued work.
        let timer = Timer(fire: Date(timeIntervalSinceNow: timeout), interval: 0, repeats: false) { timer in
            timer.invalidate()
            MainActor.assumeIsolated { onComplete() }
        }
        RunLoop.main.add(timer, forMode: runLoopMode)

        Task {
            guard timer.isValid, ContinuousClock.now < deadline else { return }
            if generation == epoch, sidToHangUpOnTermination == sid { await hangUp() }
            guard timer.isValid else { return }
            timer.invalidate()
            onComplete()
        }
        return deadline
    }

    private var sidToHangUpOnTermination: String? {
        // Another device can still answer a call that only rang here.
        if case .incoming(let call) = state, !call.isAccepting { return nil }
        return state.isLive ? state.sid : nil
    }
}
