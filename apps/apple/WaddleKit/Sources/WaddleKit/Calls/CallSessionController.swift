/// Media follows the call slot; it never owns signaling state.
@MainActor final class CallSessionController {
    private let media: any CallMediaController
    private let hangUp: @MainActor (String, JingleReason) async -> Void
    private var targetSid: String?
    private var connectedSid: String?
    private var revision = 0
    private var connectionTask: Task<Void, Never>?
    private var disconnectTask: Task<Void, Never>?

    init(media: any CallMediaController, hangUp: @escaping @MainActor (String, JingleReason) async -> Void) {
        self.media = media
        self.hangUp = hangUp
    }

    func update(_ state: CallState) {
        let active: ActiveCall?
        if case .active(let call) = state { active = call } else { active = nil }
        if let active, targetSid == active.sid { return }

        revision += 1
        let expectedRevision = revision
        targetSid = active?.sid
        let previousConnection = connectionTask
        previousConnection?.cancel()
        if connectedSid != nil {
            connectedSid = nil
            media.onFailure = nil
            let previousDisconnect = disconnectTask
            // Closing the microphone never waits for a suspended connect.
            disconnectTask = Task {
                await previousDisconnect?.value
                await media.disconnect()
            }
        }
        guard let call = active else { return }
        let pendingDisconnect = disconnectTask
        connectionTask = Task { [self] in
            // A late completion must clean up before a replacement connects;
            // otherwise its disconnect could close the replacement's media.
            await previousConnection?.value
            await pendingDisconnect?.value
            guard revision == expectedRevision, !Task.isCancelled else { return }
            connectedSid = call.sid
            media.onFailure = { [weak self] in
                guard let self, self.revision == expectedRevision else { return }
                Task {
                    guard self.revision == expectedRevision else { return }
                    await self.hangUp(call.sid, .connectivityError)
                }
            }
            do {
                try await media.connect(join: call.join, media: call.media)
                if revision != expectedRevision || Task.isCancelled {
                    await media.disconnect()
                }
            } catch {
                guard revision == expectedRevision, !Task.isCancelled else {
                    await media.disconnect()
                    return
                }
                connectedSid = nil
                media.onFailure = nil
                await media.disconnect()
                // Teardown changes the slot; it must not cancel its own
                // terminate/finish sends through the media state task.
                Task {
                    guard revision == expectedRevision else { return }
                    await hangUp(call.sid, .gone)
                }
            }
        }
    }
}
