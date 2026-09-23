import Foundation
import WaddleKit

/// Receives `WaddleClient` callbacks on Rust's threads and republishes
/// them as typed `XmppEvent`s.
final class FFIEventListener: WaddleEventListener {
    private let continuation: AsyncStream<XmppEvent>.Continuation
    private let signals: FFISessionSignals

    init(continuation: AsyncStream<XmppEvent>.Continuation, signals: FFISessionSignals) {
        self.continuation = continuation
        self.signals = signals
    }

    func onEvent(event: WaddleClientEvent) {
        guard let translated = translate(event) else { return }
        continuation.yield(translated)
    }

    // Exhaustive by design: a new `WaddleClientEvent` variant is a compile
    // error here until the bridge decides how to surface it.
    private func translate(_ event: WaddleClientEvent) -> XmppEvent? {
        switch event {
        case .connected:
            signals.setConnected(true)
            return .connected
        case .disconnected:
            signals.setConnected(false)
            return .disconnected
        case let .message(message):
            return FFIInbound.wireMessage(message).map(XmppEvent.message)
        case let .presence(presence):
            return FFIInbound.wirePresence(presence).map(XmppEvent.presence)
        case .mamResult:
            // Pages come back from the fetch and search calls.
            return nil
        case let .deliveryAcked(stanzaId):
            return .deliveryAcked(stanzaID: stanzaId)
        case let .deliveryFailed(stanzaId):
            return .deliveryFailed(stanzaID: stanzaId)
        case .call:
            BridgeLog.debug("dropped call event: calls are not bridged")
            return nil
        case let .inboxPush(entry):
            return FFIInbound.inboxEntry(entry).map(XmppEvent.inboxPush)
        case .authenticationFailed:
            signals.setConnected(false)
            return .authenticationFailed
        case let .error(description):
            // Diagnostics stay at the logging boundary. Verbs that answer a
            // failure with an empty value consult the signal instead.
            signals.recordError()
            BridgeLog.error(description)
            return nil
        }
    }
}
