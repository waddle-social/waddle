/// Media follows the call slot; it never owns signaling state.
@MainActor final class CallSessionController {
    private enum Failure: Error { case invalidGrant }
    private let media: any CallMediaController
    private let fetchExternalServices: @MainActor () async -> [ExternalService]
    private let hangUp: @MainActor (String, JingleReason) async -> Void
    private var targetSid: String?
    private var connectedSid: String?
    private var revision = 0
    private var connectionTask: Task<Void, Never>?
    private var disconnectTask: Task<Void, Never>?

    init(
        media: any CallMediaController,
        fetchExternalServices: @escaping @MainActor () async -> [ExternalService],
        hangUp: @escaping @MainActor (String, JingleReason) async -> Void
    ) {
        self.media = media
        self.fetchExternalServices = fetchExternalServices
        self.hangUp = hangUp
    }

    func update(_ state: CallState) {
        let active: ActiveCall?
        if case .active(let call) = state { active = call } else { active = nil }
        // A migration passes through `.incoming` on its way to the next
        // session; only an empty or ended slot is the end of the call.
        switch state {
        case .idle, .ended: media.forgetCaptureChoices()
        case .incoming, .outgoing, .active: break
        }
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
                self?.endCall(call.sid, reason: .connectivityError, revision: expectedRevision)
            }
            do {
                guard call.join.grantDefect == nil else { throw Failure.invalidGrant }
                let services = await fetchExternalServices()
                // Hang-up or replacement can supersede a suspended fetch.
                guard revision == expectedRevision, !Task.isCancelled else { return }
                try await media.connect(join: call.join, media: call.media, iceServers: IceServer.servers(from: services))
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
                endCall(call.sid, reason: .gone, revision: expectedRevision)
            }
        }
    }

    /// Runs in its own task: the hang-up changes the slot, and must not
    /// cancel its own terminate and finish sends through the media task.
    private func endCall(_ sid: String, reason: JingleReason, revision expectedRevision: Int) {
        guard revision == expectedRevision else { return }
        Task {
            guard revision == expectedRevision else { return }
            await hangUp(sid, reason)
        }
    }
}
