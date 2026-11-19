/// Media follows the call slot; it never owns signaling state.
@MainActor final class CallSessionController {
    private enum Failure: Error { case invalidGrant }
    private let media: any CallMediaController
    private let fetchExternalServices: @MainActor () async -> [ExternalService]
    private let mediaConnected: @MainActor (String) -> Void
    private let phaseChanged: @MainActor (String, CallMediaPhase) -> Void
    /// The flag is true when the SFU ended the session, which is not a failure.
    private let hangUp: @MainActor (String, JingleReason, Bool) async -> Void
    private var targetSid: String?
    private var connectedSid: String?
    private var revision = 0
    private var connectionTask: Task<Void, Never>?
    private var disconnectTask: Task<Void, Never>?
    /// The call the microphone and camera choices were last reset for.
    private var choicesSid: String?

    init(
        media: any CallMediaController,
        fetchExternalServices: @escaping @MainActor () async -> [ExternalService],
        mediaConnected: @escaping @MainActor (String) -> Void,
        phaseChanged: @escaping @MainActor (String, CallMediaPhase) -> Void,
        hangUp: @escaping @MainActor (String, JingleReason, Bool) async -> Void
    ) {
        self.media = media
        self.fetchExternalServices = fetchExternalServices
        self.mediaConnected = mediaConnected
        self.phaseChanged = phaseChanged
        self.hangUp = hangUp
    }

    func update(_ state: CallState) {
        let active = state.mediaSession
        forgetChoicesIfANewCallBegins(state)
        if let active, targetSid == active.sid { return }

        revision += 1
        let expectedRevision = revision
        targetSid = active?.sid
        let previousConnection = connectionTask
        previousConnection?.cancel()
        if connectedSid != nil {
            connectedSid = nil
            media.onFailure = nil
            media.onPhaseChange = nil
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
                guard let self else { return }
                self.endCall(call.sid, reason: .connectivityError, revision: expectedRevision, endedByServer: self.media.wasEndedByServer)
            }
            media.onPhaseChange = { [weak self] phase in
                guard let self, self.revision == expectedRevision else { return }
                switch phase {
                case .reconnecting, .connected: self.phaseChanged(call.sid, phase)
                case .idle, .connecting: break
                }
            }
            do {
                guard call.join.grantDefect == nil else { throw Failure.invalidGrant }
                let services = await fetchExternalServices()
                // Hang-up or replacement can supersede a suspended fetch.
                guard revision == expectedRevision, !Task.isCancelled else { return }
                try await media.connect(join: call.join, media: call.media, iceServers: IceServer.servers(from: services))
                if revision != expectedRevision || Task.isCancelled {
                    await media.disconnect()
                } else {
                    mediaConnected(call.sid)
                }
            } catch {
                guard revision == expectedRevision, !Task.isCancelled else {
                    await media.disconnect()
                    return
                }
                connectedSid = nil
                media.onFailure = nil
                media.onPhaseChange = nil
                await media.disconnect()
                endCall(call.sid, reason: .gone, revision: expectedRevision)
            }
        }
    }

    /// Choices are dropped where a new call begins, not where one ends: a
    /// replaced session can pass through `.ended` on its way to an
    /// already-accepted invitation, and that is still the same call. Nor
    /// are they dropped again while that call is still being placed or is
    /// still ringing: it can be muted on the system's call screen by then.
    private func forgetChoicesIfANewCallBegins(_ state: CallState) {
        let beginning: String
        switch state {
        case .outgoing(let call): beginning = call.sid
        case .joiningGroup(let call): beginning = call.sid
        case .incoming(let call) where !call.isAccepting: beginning = call.sid
        case .idle:
            // The slot is free: whatever comes next is a new call, even
            // one that happens to reuse a session id.
            choicesSid = nil
            return
        case .incoming, .active, .inGroup, .ended: return
        }
        guard beginning != choicesSid else { return }
        choicesSid = beginning
        media.forgetCaptureChoices()
    }

    /// Runs in its own task: the hang-up changes the slot, and must not
    /// cancel its own terminate and finish sends through the media task.
    private func endCall(_ sid: String, reason: JingleReason, revision expectedRevision: Int, endedByServer: Bool = false) {
        guard revision == expectedRevision else { return }
        Task {
            guard revision == expectedRevision else { return }
            await hangUp(sid, reason, endedByServer)
        }
    }
}

private extension CallState {
    /// Both engines hand the same credentials and agreed tracks to media.
    var mediaSession: (sid: String, join: CallJoin, media: CallMedia)? {
        switch self {
        case .active(let call): (call.sid, call.join, call.media)
        case .inGroup(let call): (call.sid, call.join, call.media)
        case .idle, .incoming, .outgoing, .joiningGroup, .ended: nil
        }
    }
}
