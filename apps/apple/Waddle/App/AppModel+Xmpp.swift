import Foundation
import os

// MARK: - XMPP Connection Lifecycle

private let logger = Logger(subsystem: "social.waddle.ios", category: "AppModel")

extension AppModel {
    func openXMPPAdmissions(
        sessionIdentity: XMPPLoginSessionIdentity
    ) {
        xmppLifecycle.openLogin(sessionIdentity: sessionIdentity)
    }

    func closeXMPPAdmissions() async {
        await xmppLifecycle.closeLogin()
    }

    func connectXMPP(using session: WaddleSession) async {
        let sessionIdentity = XMPPLoginSessionIdentity(value: session.sessionID)
        reconnectTask?.cancel()
        reconnectTask = nil

        guard let admissionGeneration = await xmppLifecycle.beginReplacement(
            sessionIdentity: sessionIdentity
        ) else {
            return
        }

        failPendingRoomJoins(with: XMPPServiceError.disconnected)
        joinedRoomJIDs.removeAll()
        presenceByRoomJID.removeAll()
        guard
            xmppLifecycle.reconnectAllowed,
            self.session?.sessionID == sessionIdentity.value
        else {
            return
        }

        let rustConfig = WaddleConfig(
            serverUrl: session.xmppWebsocketURL,
            jid: session.jid,
            accessToken: session.sessionID,
            resource: session.xmppCredentials.resource,
            deliveryAttempt: WaddleDeliveryAttemptRef(
                attemptId: WaddleDeliveryAttemptId(value: UUID().uuidString),
                // A fresh UUID starts a new delivery-attempt lineage. Rust
                // advances this generation only for an in-line resume
                // fallback under that lineage.
                connectionGeneration: WaddleConnectionGeneration(value: 0)
            ),
            // The Apple client does not persist XEP-0198 resume state yet;
            // every connect starts a fresh stream.
            resumeState: nil
        )
        let candidate = RustXmppClient(config: rustConfig)
        guard xmppLifecycle.install(
            client: candidate,
            loginGeneration: admissionGeneration,
            sessionIdentity: sessionIdentity
        ) else {
            await candidate.disconnect()
            return
        }

        updateConnectionBanner(for: .connecting)

        let connected = await candidate.connect()
        guard
            connected,
            xmppLifecycle.admits(
                client: candidate,
                loginGeneration: admissionGeneration,
                sessionIdentity: sessionIdentity
            ),
            self.session?.sessionID == sessionIdentity.value
        else {
            await xmppLifecycle.remove(
                client: candidate,
                loginGeneration: admissionGeneration,
                sessionIdentity: sessionIdentity
            )
            return
        }

        let started = xmppLifecycle.startConsumer(
            for: candidate,
            loginGeneration: admissionGeneration,
            sessionIdentity: sessionIdentity
        ) { @MainActor [weak self, weak candidate] in
            guard let self, let candidate else { return }
            await self.consumeXMPPEvents(
                from: candidate,
                admissionGeneration: admissionGeneration,
                sessionIdentity: sessionIdentity
            )
        }
        if !started {
            await xmppLifecycle.remove(
                client: candidate,
                loginGeneration: admissionGeneration,
                sessionIdentity: sessionIdentity
            )
        }
    }

    private func consumeXMPPEvents(
        from client: RustXmppClient,
        admissionGeneration: UInt64,
        sessionIdentity: XMPPLoginSessionIdentity
    ) async {
        while !Task.isCancelled {
            guard
                xmppLifecycle.admits(
                    client: client,
                    loginGeneration: admissionGeneration,
                    sessionIdentity: sessionIdentity
                ),
                self.session?.sessionID == sessionIdentity.value
            else {
                return
            }

            let result = await client.nextEvent()
            guard
                !Task.isCancelled,
                xmppLifecycle.admits(
                    client: client,
                    loginGeneration: admissionGeneration,
                    sessionIdentity: sessionIdentity
                ),
                self.session?.sessionID == sessionIdentity.value
            else {
                return
            }

            switch result {
            case .event(let event):
                await handleXMPPEvent(
                    event,
                    from: client,
                    admissionGeneration: admissionGeneration,
                    sessionIdentity: sessionIdentity
                )
            case .consumed:
                continue
            case .closed:
                return
            }
        }
    }

    private func handleXMPPEvent(
        _ event: XMPPEvent,
        from client: RustXmppClient,
        admissionGeneration: UInt64,
        sessionIdentity: XMPPLoginSessionIdentity
    ) async {
        switch event {
        case .streamFeatures:
            dlog(" streamFeatures received")
            updateConnectionBanner(for: .negotiating)
        case .authenticated:
            dlog(" authenticated")
            updateConnectionBanner(for: .authenticating)
        case .resourceBound(let jid):
            dlog(" resourceBound: \(jid)")
            updateConnectionBanner(for: .binding)
        case let .sessionReady(kind, attempt):
            dlog(" sessionReady kind=\(kind) attempt=\(attempt.id)")
            reconnectTask?.cancel()
            reconnectTask = nil
            updateConnectionBanner(for: .ready)
            switch kind.bootstrapPlan {
            case .establishFreshSession:
                await xmppLifecycle.performFreshBootstrap(
                    client: client,
                    loginGeneration: admissionGeneration,
                    sessionIdentity: sessionIdentity,
                    sendPresence: {
                        await client.sendPresence()
                    },
                    admitRoomLoad: { @MainActor [weak self, weak client] in
                        guard
                            let self,
                            let client,
                            self.session?.sessionID == sessionIdentity.value,
                            self.xmppLifecycle.admits(
                                client: client,
                                loginGeneration: admissionGeneration,
                                sessionIdentity: sessionIdentity
                            )
                        else {
                            return
                        }
                        dlog(" presence sent")
                        // Room loading runs independently so this pull barrier
                        // can admit MUC self-presence on the next pull.
                        Task { @MainActor [weak self, weak client] in
                            guard
                                let self,
                                let client,
                                !Task.isCancelled,
                                self.session?.sessionID == sessionIdentity.value,
                                self.xmppLifecycle.admits(
                                    client: client,
                                    loginGeneration: admissionGeneration,
                                    sessionIdentity: sessionIdentity
                                )
                            else {
                                return
                            }
                            dlog(" loading rooms")
                            await self.loadRooms()
                        }
                    }
                )
            case .preserveResumedSession:
                // XEP-0198 resume preserves the bound resource and prior
                // presence. Replaying fresh-stream bootstrap would duplicate
                // presence and room joins.
                dlog(" resumed session; preserving bound session bootstrap")
            }
        case .message(let message):
            handleIncomingMessage(message)
        case .presence(let presence):
            handleIncomingPresence(presence)
        case .messageDeliveryAcked(let signal):
            dlog(" messageDeliveryAcked: \(signal.stanzaID) attempt=\(signal.attempt.id)")
        case .messageDeliveryFailed(let signal):
            dlog(" messageDeliveryFailed: \(signal.stanzaID) attempt=\(signal.attempt.id)")
        case .authenticationFailed(let condition):
            let message = "XMPP authentication failed: \(saslConditionDescription(condition))"
            dlog(" authenticationFailed: \(message)")
            let decision = xmppLifecycle.recordAuthenticationFailure(
                condition
            )
            if decision.disposition == .retry || !decision.wasAlreadyStopped {
                errorMessage = message
                chatStore.setBannerState(.error(message: message))
                updateChatSurfaceState()
            }
        case .streamError(let name, let text):
            let message = text ?? name
            dlog(" streamError: \(name) \(text ?? "")")
            errorMessage = message
            chatStore.setBannerState(.error(message: message))
            updateChatSurfaceState()
        case .error(let message):
            errorMessage = message
            chatStore.setBannerState(.error(message: message))
            updateChatSurfaceState()
        case .disconnected:
            joinedRoomJIDs.removeAll()
            presenceByRoomJID.removeAll()
            failPendingRoomJoins(with: XMPPServiceError.disconnected)
            if xmppLifecycle.reconnectAllowed {
                chatStore.setBannerState(.disconnected(message: "Disconnected from live chat."))
                scheduleReconnectIfNeeded()
            }
            updateChatSurfaceState()
        case .call(let callEvent):
            // The Rust FFI delivers fully-typed XEP-0353 JMI + XEP-0166
            // Jingle session control events here. The dedicated call UI
            // and Jingle/LiveKit pipeline lands in a follow-up PR; for
            // now we log the event so it is visible during development
            // builds without surfacing any UI. Letting this case fall
            // through is intentional — the ringing UI is gated on the
            // call surface PR, not on FFI plumbing.
            logger.info("XMPP call event sid=\(callEvent.sid, privacy: .public) kind=\(String(describing: callEvent.kind), privacy: .public)")
        }
    }

    private func saslConditionDescription(_ condition: XMPPSaslCondition) -> String {
        switch condition {
        case .aborted: return "aborted"
        case .accountDisabled: return "account disabled"
        case .credentialsExpired: return "credentials expired"
        case .encryptionRequired: return "encryption required"
        case .incorrectEncoding: return "incorrect encoding"
        case .invalidAuthzid: return "invalid authorization identity"
        case .invalidMechanism: return "invalid mechanism"
        case .malformedRequest: return "malformed request"
        case .mechanismTooWeak: return "mechanism too weak"
        case .notAuthorized: return "not authorized"
        case .temporaryAuthFailure: return "temporary authentication failure"
        case .unknown: return "unknown condition"
        }
    }

    func updateConnectionBanner(for state: XMPPConnectionState) {
        switch state {
        case .disconnected:
            chatStore.setBannerState(.disconnected(message: "Live chat is offline."))
        case .connecting:
            chatStore.setBannerState(.connecting(message: "Connecting to XMPP…"))
        case .negotiating:
            chatStore.setBannerState(.connecting(message: "Negotiating live session…"))
        case .authenticating:
            chatStore.setBannerState(.connecting(message: "Authenticating live session…"))
        case .binding:
            chatStore.setBannerState(.connecting(message: "Binding live resource…"))
        case .ready:
            chatStore.setBannerState(.connecting(message: "Preparing live chat…"))
        case .disconnecting:
            chatStore.setBannerState(.reconnecting(message: "Disconnecting live chat…"))
        case .failed(let message):
            chatStore.setBannerState(.error(message: message))
        }
    }

    func handleAppBecameActive() {
        guard let session else { return }
        guard xmppLifecycle.reconnectAllowed else {
            return
        }
        guard let rustClient else {
            Task { await connectXMPP(using: session) }
            return
        }
        switch rustClient.connectionState {
        case .ready:
            break
        case .connecting, .negotiating, .authenticating, .binding:
            break
        case .disconnected, .failed, .disconnecting:
            Task { await connectXMPP(using: session) }
        }
    }

    private func scheduleReconnectIfNeeded() {
        guard
            let session,
            reconnectTask == nil,
            xmppLifecycle.reconnectAllowed
        else {
            return
        }

        let generation = xmppLifecycle.loginGeneration
        let sessionIdentity = XMPPLoginSessionIdentity(value: session.sessionID)
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            guard
                !Task.isCancelled,
                let self,
                self.xmppLifecycle.admitsLogin(
                    generation: generation,
                    sessionIdentity: sessionIdentity
                ),
                self.xmppLifecycle.reconnectAllowed,
                self.session?.sessionID == sessionIdentity.value
            else {
                return
            }
            await self.connectXMPP(using: session)
        }
    }
}
