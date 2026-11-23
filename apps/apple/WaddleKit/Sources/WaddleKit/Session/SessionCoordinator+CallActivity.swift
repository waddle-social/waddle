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
            guard let self else { return }
            try await write(self)
        }
        activityWriteTail = task
        return task
    }

    func scheduleCallActivity() {
        guard !isStopped, connection == .online else { return }
        let state = calls.state
        enqueueActivityWrite { session in try await session.reconcileCallActivity(for: state) }
    }

    /// A publish already on its way can still succeed during sign-out.
    /// Let it settle and restore the activity before closing the stream.
    func finishCallActivity() async {
        let cleanup = enqueueActivityWrite { session in
            try? await session.reconcileCallActivity(for: .idle, closingStream: true)
            session.ownedCallActivity = nil
        }
        _ = await cleanup.result
        if activityWriteTail == cleanup { activityWriteTail = nil }
    }

    private func reconcileCallActivity(for state: CallState, closingStream: Bool = false) async throws {
        guard closingStream || (!isStopped && connection == .online && !isConnectResetting) else { return }
        let desired = CallActivity.mark(for: state)
        guard desired == nil || calls.state == state else { return }
        guard desired != ownedCallActivity?.mark else { return }

        let epoch = connectionEpoch
        let published = try await port.fetchOwnActivity()
        guard closingStream || (!isStopped && !isConnectResetting && epoch == connectionEpoch) else { return }
        // Skip a publish if the call moved on during the lookup. Cleanup
        // still runs so the next call can read what the previous one left.
        guard desired == nil || calls.state == state else { return }

        switch CallActivity.change(desired: desired, published: published, owned: ownedCallActivity) {
        case .write(let activity, let keeping):
            try await writeOwnActivity(activity)
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
