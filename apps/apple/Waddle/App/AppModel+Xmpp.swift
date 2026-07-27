import Foundation
import os

// MARK: - XMPP Connection Lifecycle

private let logger = Logger(subsystem: "social.waddle.ios", category: "AppModel")

extension AppModel {
    func connectXMPP(using session: WaddleSession) async {
        reconnectTask?.cancel()
        reconnectTask = nil
        xmppEventsTask?.cancel()
        failPendingRoomJoins(with: XMPPServiceError.disconnected)
        joinedRoomJIDs.removeAll()
        presenceByRoomJID.removeAll()
        if let rustClient {
            await rustClient.disconnect()
        }

        let rustConfig = WaddleConfig(
            serverUrl: session.xmppWebsocketURL,
            jid: session.jid,
            accessToken: session.sessionID,
            resource: session.xmppCredentials.resource
        )
        rustClient = RustXmppClient(config: rustConfig)

        updateConnectionBanner(for: .connecting)

        xmppEventsTask = Task { [weak self] in
            guard let self, let client = self.rustClient else { return }
            for await event in client.events {
                await self.handleXMPPEvent(event)
            }
        }

        await rustClient!.connect()
    }

    private func handleXMPPEvent(_ event: XMPPEvent) async {
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
        case .sessionReady:
            dlog(" sessionReady")
            reconnectTask?.cancel()
            reconnectTask = nil
            updateConnectionBanner(for: .ready)
            await rustClient?.sendPresence()
            dlog(" presence sent")
            // Fire room loading in a separate Task so this event-loop iteration
            // completes and can process incoming events (e.g. the MUC self-presence that
            // resolves the join) while the load is in flight. Without this, the
            // event loop deadlocks: joinSelectedChannel waits for a self-presence event that
            // can never be delivered because the event loop is blocked on joinSelectedChannel.
            Task { @MainActor [weak self] in
                guard let self else { return }
                dlog(" loading rooms")
                await self.loadRooms()
            }
        case .message(let message):
            handleIncomingMessage(message)
        case .presence(let presence):
            handleIncomingPresence(presence)
        case .messageDeliveryAcked(let stanzaID):
            dlog(" messageDeliveryAcked: \(stanzaID)")
        case .messageDeliveryFailed(let stanzaID):
            dlog(" messageDeliveryFailed: \(stanzaID)")
        case .authenticationFailed(let detail):
            let message = detail ?? "The server rejected the XMPP bearer token."
            dlog(" authenticationFailed: \(message)")
            errorMessage = message
            chatStore.setBannerState(.error(message: message))
            updateChatSurfaceState()
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
            chatStore.setBannerState(.disconnected(message: "Disconnected from live chat."))
            scheduleReconnectIfNeeded()
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
        guard let session, reconnectTask == nil else {
            return
        }

        reconnectTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            guard !Task.isCancelled else { return }
            await self?.connectXMPP(using: session)
        }
    }
}
