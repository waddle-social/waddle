import Foundation

extension SessionCoordinator {
    public func lookupLinkPreview(_ request: LinkPreviewRequest) async throws -> LinkPreviewLookup {
        guard !isStopped else { throw PortError.notConnected }
        return try await port.lookupLinkPreview(request)
    }

    /// Inserts a local echo and queues the draft in the session's FIFO
    /// outbox, which drains once the session has rejoined its rooms.
    /// `onQueued` runs once the outbox accepts the draft, before transport IO.
    @discardableResult
    public func send(
        _ draft: Draft, in conversation: ConversationID, linkPreview: LinkPreviewPayload? = nil,
        onQueued: @MainActor @Sendable () -> Void = { /* the caller keeps no draft to clear */ }
    ) async -> String? {
        guard var composed = MessageComposer.compose(draft) else { return nil }
        if canFlushOutboundQueue { composed.options.linkPreview = linkPreview?.ifFresh(at: now()) }
        return await send(composed, preview: draft.text, in: conversation, onQueued: onQueued)
    }

    func takeOutboundAttempt(_ next: inout OutboundMessage) -> OutboundMessage {
        let attempt = next.withFreshLinkPreview(at: now())
        // On reconnect or replay, send the message without the authorization.
        next = next.withoutLinkPreview()
        outboundQueue[0] = next
        return attempt
    }
}

extension OutboundMessage {
    func withFreshLinkPreview(at now: Date) -> OutboundMessage {
        guard let expiry = options.linkPreview?.expiresAt, expiry <= now else { return self }
        return withoutLinkPreview()
    }

    func withoutLinkPreview() -> OutboundMessage {
        guard options.linkPreview != nil else { return self }
        var options = self.options
        options.linkPreview = nil
        return OutboundMessage(clientID: clientID, conversation: conversation, body: body, options: options)
    }
}
