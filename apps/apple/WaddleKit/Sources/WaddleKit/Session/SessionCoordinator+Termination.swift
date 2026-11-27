import Foundation

extension SessionCoordinator {
    /// Returns true when the caller should defer quitting until `onComplete`.
    /// One timeout covers the hang-up and observation of the stream's close.
    public func beginCallTermination(
        timeout: Duration = .seconds(2),
        onComplete: @escaping @MainActor @Sendable () -> Void
    ) -> Bool {
        guard !isStopped, !isTerminating, calls.hasCallToHangUpForTermination else { return false }
        isTerminating = true
        reconnectTask?.cancel()
        foregroundProbeTask?.cancel()
        connectWatchdog?.cancel()
        readyTask?.cancel()

        let deadline = ContinuousClock.now + timeout
        let (seconds, attoseconds) = timeout.components
        let interval = Double(seconds) + Double(attoseconds) / 1e18
        // Common modes let the timer run even in AppKit's modal loop or
        // when UIKit's nested run loop can't service the main dispatch queue.
        let timer = Timer(fire: Date(timeIntervalSinceNow: interval), interval: 0, repeats: false) { [weak self] timer in
            timer.invalidate()
            MainActor.assumeIsolated {
                self?.onTerminationDisconnect = nil
                onComplete()
            }
        }
        RunLoop.main.add(timer, forMode: .common)

        Task {
            // A stream being opened has no handle to send through until its
            // connect returns, whether we are reconnecting or the listener
            // has already reported it connected. A hang-up sent before that
            // goes nowhere and still clears the slot.
            while isConnectInFlight, timer.isValid, ContinuousClock.now < deadline {
                try? await Task.sleep(for: .milliseconds(10))
            }
            guard timer.isValid, ContinuousClock.now < deadline else { return }
            await calls.hangUpForTermination()
            guard timer.isValid, ContinuousClock.now < deadline else { return }

            if case .offline = connection {
                timer.invalidate()
                onComplete()
                return
            }
            onTerminationDisconnect = {
                timer.invalidate()
                onComplete()
            }
            // If a reset already took the FFI handle, disconnect can emit a
            // synthetic event and finish early. The hang-up stanzas could
            // not have been sent in that state, and the timer still bounds the wait.
            await port.disconnect()
        }
        return true
    }

    func completeTerminationOnDisconnect() {
        status.connection = .offline(retryAt: nil)
        let complete = onTerminationDisconnect
        onTerminationDisconnect = nil
        complete?()
    }
}
