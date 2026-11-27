import Foundation
import WaddleKit

/// WaddleKit's `XmppPort` over the UniFFI `WaddleClient`. FFI strings are
/// parsed into typed values at this boundary (`FFIInbound`) and typed
/// values are rendered back only for the call (`FFIOutbound`).
final class FFIXmppPort: XmppPort {
    let events: AsyncStream<XmppEvent>
    let client: WaddleClient
    /// The signed-in account, from the config the client was built with.
    let account: BareJID?
    let linkPreviewMediaOrigin: URL?
    let signals: FFISessionSignals
    /// XEP-0363 service, discovered once; rediscovered while unknown.
    let uploadService = FFILocked<BareJID?>(nil)
    /// Story id to the author whose PEP service hosts the story and its
    /// reactions, filled by each stories fetch and post.
    let storyHosts = FFILocked<[String: BareJID]>([:])
    private let continuation: AsyncStream<XmppEvent>.Continuation

    init(config: WaddleConfig, linkPreviewMediaOrigin: String? = nil) {
        let (events, continuation) = AsyncStream<XmppEvent>.makeStream()
        let signals = FFISessionSignals()
        let mediaOrigin = LinkPreviewMedia.origin(explicit: linkPreviewMediaOrigin, webSocket: config.serverUrl)
        self.events = events
        self.continuation = continuation
        self.signals = signals
        self.account = FFIInbound.jid(config.jid)?.bare
        self.linkPreviewMediaOrigin = mediaOrigin
        self.client = WaddleClient(
            config: config,
            listener: FFIEventListener(continuation: continuation, signals: signals, linkPreviewMediaOrigin: mediaOrigin)
        )
    }

    // The stream outlives `disconnect()` so a reconnect reuses it.
    deinit {
        continuation.finish()
    }

    func connect() async {
        await client.connect()
    }

    func disconnect() async {
        signals.setConnected(false)
        await client.disconnect()
    }

    func probeConnection() async -> Bool {
        do {
            try await client.pingServer()
            return true
        } catch {
            return false
        }
    }

    func sendPresence(_ availability: Availability, status: String?, idleSince: Date?) async {
        await client.sendPresence(
            status: status, show: availability.showValue, idleSince: idleSince?.formatted(.iso8601))
    }
}
