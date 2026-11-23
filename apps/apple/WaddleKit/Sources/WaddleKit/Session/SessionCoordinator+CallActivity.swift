extension SessionCoordinator {
    /// Both the profile editor and calls read the same item before writing it.
    /// Run each read and write together so they see earlier local edits.
    @discardableResult
    func enqueueActivityWrite(
        _ write: @escaping @MainActor (SessionCoordinator) async throws -> Void
    ) -> Task<Void, Error> {
        let earlier = activityWriteTail
        let task = Task { [weak self] in
            _ = await earlier?.result
            try Task.checkCancellation()
            guard let self else { return }
            try await write(self)
        }
        activityWriteTail = task
        return task
    }

    func scheduleCallActivity() {
        cancelCallActivityRetry()
        guard canReconcileCallActivity else { return }
        let state = calls.state
        let epoch = connectionEpoch
        enqueueActivityWrite { session in
            guard epoch == session.connectionEpoch else { return }
            session.cancelCallActivityRetry()
            let needsRetry: Bool
            do {
                try await session.reconcileCallActivity(for: state)
                needsRetry = session.ownedCallActivity?.isConfirmed == false
            } catch {
                needsRetry = true
            }
            if needsRetry, epoch == session.connectionEpoch { session.scheduleCallActivityRetry() }
        }
    }

    private var canReconcileCallActivity: Bool {
        !isStopped && connection == .online && !isConnectResetting
    }

    /// Recheck what the server holds after an error. A new call state or
    /// stream already schedules its own check, so it replaces this timer.
    private func scheduleCallActivityRetry() {
        guard canReconcileCallActivity else { return }
        cancelCallActivityRetry()
        callActivityRetryTask = Task { [weak self, callActivityRetryDelay] in
            do { try await Task.sleep(for: callActivityRetryDelay) } catch { return }
            guard !Task.isCancelled else { return }
            self?.scheduleCallActivity()
        }
    }

    func cancelCallActivityRetry() {
        callActivityRetryTask?.cancel()
        callActivityRetryTask = nil
    }

    /// A publish already on its way can still succeed during sign-out.
    /// Give it time to settle so we can restore the activity before disconnecting.
    func finishCallActivity() async {
        let cleanup = enqueueActivityWrite { session in
            try? await session.reconcileCallActivity(for: .idle, closingStream: true)
        }
        await waitForCallTeardown { _ = await cleanup.result }
        cleanup.cancel()
        activityWriteGeneration += 1
        ownedCallActivity = nil
        if activityWriteTail == cleanup { activityWriteTail = nil }
    }

    private func reconcileCallActivity(for state: CallState, closingStream: Bool = false) async throws {
        guard closingStream || canReconcileCallActivity else { return }
        let desired = CallActivity.mark(for: state)
        guard desired == nil || calls.state == state else { return }
        guard desired != ownedCallActivity?.mark || ownedCallActivity?.isConfirmed == false else { return }

        let epoch = connectionEpoch
        let published = try await port.fetchOwnActivity()
        guard !Task.isCancelled, epoch == connectionEpoch, closingStream || canReconcileCallActivity else { return }
        // Skip a publish if the call moved on during the lookup. Cleanup
        // still runs so the next call can read what the previous one left.
        guard desired == nil || calls.state == state else { return }

        if published == ownedCallActivity?.mark { ownedCallActivity?.isConfirmed = true }
        switch CallActivity.change(desired: desired, published: published, owned: ownedCallActivity) {
        case .write(let activity, let keeping):
            if var keeping {
                keeping.isConfirmed = false
                ownedCallActivity = keeping
            }
            try await writeOwnActivity(activity)
            guard !Task.isCancelled, epoch == connectionEpoch else { return }
            ownedCallActivity = keeping
            return
        case .forget:
            ownedCallActivity = nil
        case .keep:
            break
        }
        recordOwnActivity(published)
    }
}
