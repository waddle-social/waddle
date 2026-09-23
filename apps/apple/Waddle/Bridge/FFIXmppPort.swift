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
    let signals: FFISessionSignals
    /// XEP-0363 service, discovered once; rediscovered while unknown.
    let uploadService = FFILocked<BareJID?>(nil)
    /// Last known Waddle rich-payload opt-in per conversation, repeated on
    /// every XEP-0492 mode change so the merge never clears it.
    let richPayloadOptIns = FFILocked<[ConversationID: Bool]>([:])
    private let continuation: AsyncStream<XmppEvent>.Continuation

    init(config: WaddleConfig) {
        let (events, continuation) = AsyncStream<XmppEvent>.makeStream()
        let signals = FFISessionSignals()
        self.events = events
        self.continuation = continuation
        self.signals = signals
        self.account = FFIInbound.jid(config.jid)?.bare
        self.client = WaddleClient(
            config: config,
            listener: FFIEventListener(continuation: continuation, signals: signals)
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

    func sendPresence(_ availability: Availability, status: String?) async {
        await client.sendPresence(status: status, show: availability.showValue, idleSince: nil)
    }
}
