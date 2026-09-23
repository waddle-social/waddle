import Foundation

extension SessionCoordinator {
    /// Sends a draft. The row appears immediately as a local echo; its
    /// delivery state lives in `deliveries` under the returned client id.
    /// Every send goes through one FIFO queue that only drains once the
    /// session has rejoined its rooms, so offline and reconnect-window sends
    /// keep their order and never reach a room we are not in yet.
    @discardableResult
    public func send(_ draft: Draft, in conversation: ConversationID) async -> String? {
        guard let composed = MessageComposer.compose(draft) else { return nil }
        let message = OutboundMessage(
            clientID: Self.makeClientID(),
            conversation: conversation,
            body: composed.body,
            options: composed.options
        )
        timelines.insertLocalEcho(localEcho(of: message), in: conversation)
        if conversation.kind == .direct {
            directory.touchDirect(conversation.jid, at: Date(), preview: draft.text)
        }
        stopTyping(in: conversation)
        enqueue(message)
        await flushOutboundQueue()
        return message.clientID
    }

    /// Re-sends a failed own message under its original id.
    public func retry(clientID: String) async {
        guard let failed = failedOutbound.removeValue(forKey: clientID) else { return }
        enqueue(failed)
        await flushOutboundQueue()
    }

    /// Drops a failed or queued own message.
    public func discard(clientID: String, in conversation: ConversationID) {
        outboundQueue.removeAll { $0.clientID == clientID }
        failedOutbound[clientID] = nil
        deliveries.forget(clientID)
        timelines.removeLocalEcho(id: clientID, in: conversation)
    }

    /// Sends queued messages in order. A transient failure stops the drain
    /// and keeps the message at the head for the next ready session.
    func flushOutboundQueue() async {
        guard isSendReady, !isFlushing else { return }
        isFlushing = true
        defer { isFlushing = false }
        while isSendReady, let next = outboundQueue.first {
            deliveries.began(next.clientID)
            let outcome = await port.send(next)
            deliveries.outcome(outcome, for: next.clientID)
            // Act on the settled state: an ack or failure that arrived while
            // the send was suspended outranks the call's own result.
            switch (outcome, deliveries.state(of: next.clientID)) {
            case (_, .acknowledged?):
                outboundQueue.removeAll { $0.clientID == next.clientID }
                rememberSent(next)
            case (.rejected, _), (_, .failed?):
                outboundQueue.removeAll { $0.clientID == next.clientID }
                failedOutbound[next.clientID] = next
            case (.sent, _):
                outboundQueue.removeAll { $0.clientID == next.clientID }
                rememberSent(next)
            case (.notConnected, _), (.transportError, _):
                return
            }
        }
    }

    private func enqueue(_ message: OutboundMessage) {
        deliveries.queued(message.clientID)
        if !outboundQueue.contains(where: { $0.clientID == message.clientID }) {
            outboundQueue.append(message)
        }
    }

    /// Keeps written messages (bounded) so a later XEP-0198 failure or an
    /// error bounce can still be retried.
    private func rememberSent(_ message: OutboundMessage) {
        sentOutbound[message.clientID] = message
        sentOrder.append(message.clientID)
        if sentOrder.count > 200 {
            sentOutbound[sentOrder.removeFirst()] = nil
        }
    }

    /// The written message failed after all (XEP-0198 or an error bounce):
    /// make it retryable.
    func sentMessageFailed(_ clientID: String, bounced: Bool) {
        if let message = sentOutbound.removeValue(forKey: clientID) {
            failedOutbound[clientID] = message
        }
        if bounced {
            deliveries.bounced(clientID)
        } else {
            deliveries.failed(clientID)
        }
    }

    /// The optimistic row: our occupant JID in a room (so the reflection
    /// dedupes on the same sender), our bare JID in 1:1.
    func localEcho(of message: OutboundMessage) -> WireMessage {
        let conversation = message.conversation
        let from = ownJID(in: conversation)
        return WireMessage(
            type: conversation.isRoom ? .groupchat : .chat,
            from: from,
            to: JID(bare: conversation.jid, resource: nil),
            identity: MessageIdentity(messageID: message.clientID, originID: message.clientID),
            body: message.body,
            thread: message.options.thread,
            reply: message.options.reply.map {
                WireMessage.ReplyTarget(id: $0.targetID, author: $0.author, fallback: $0.fallback)
            },
            markupSpans: message.options.markupSpans,
            references: message.options.references,
            sharedFiles: message.options.sharedFiles
        )
    }

    func ownJID(in conversation: ConversationID) -> JID? {
        if conversation.isRoom {
            return conversation.jid.with(resource: account.nick)
        }
        return JID(bare: account.jid, resource: nil)
    }

    static func makeClientID() -> String {
        UUID().uuidString.lowercased()
    }

    // MARK: - Message actions

    /// Toggles our `emoji` reaction on `item` (XEP-0444 sends the full set).
    public func toggleReaction(_ emoji: String, on item: TimelineItem) async -> Bool {
        guard let target = item.actionTargetID, let from = ownJID(in: item.conversation) else { return false }
        var mine = item.reactions.filter(\.includesMine).map(\.emoji)
        if let index = mine.firstIndex(of: emoji) {
            mine.remove(at: index)
        } else {
            mine.append(emoji)
        }
        guard await port.sendReaction(to: target, emojis: mine, in: item.conversation) else { return false }
        // A room reflects the reaction back; only 1:1 needs a local apply.
        guard !item.conversation.isRoom else { return true }
        let senderKey = item.conversation.isRoom ? from.description : from.bare.description
        timelines.applyLocalMutation(
            .reaction(targetID: target, from: from, senderKey: senderKey, isMine: true, emojis: mine),
            in: item.conversation
        )
        return true
    }

    /// XEP-0308 correction of one of our own messages. The correction
    /// re-sends the whole message (reply, thread, attachments) with only
    /// the text changed, as the XEP requires.
    public func edit(_ item: TimelineItem, to text: String) async -> Bool {
        guard item.isMine,
              item.tombstone == nil,
              let target = item.correctionTargetID,
              let from = ownJID(in: item.conversation),
              let composed = MessageComposer.compose(correctionDraft(of: item, text: text))
        else { return false }
        let outcome = await port.sendCorrection(of: target, body: composed.body, in: item.conversation, options: composed.options)
        guard case .sent = outcome else { return false }
        // A room reflects the correction, and may reject it (for example
        // after a reconnect changed our occupancy); apply only in 1:1.
        guard !item.conversation.isRoom else { return true }
        timelines.applyLocalMutation(
            .correction(targetID: target, from: from, content: CorrectedContent(body: composed.body, options: composed.options)),
            in: item.conversation
        )
        return true
    }

    private func correctionDraft(of item: TimelineItem, text: String) -> Draft {
        let timeline = timelines.timeline(for: item.conversation)
        let reply = item.message.reply.flatMap { target -> ReplyContext? in
            guard let author = target.author else { return nil }
            let parent = timeline.item(withID: target.id)
            return ReplyContext(
                targetID: target.id,
                author: author,
                parentBody: parent?.body ?? "",
                parentAuthorName: parent?.authorName ?? ""
            )
        }
        return Draft(text: text, reply: reply, thread: item.message.thread, attachments: item.message.sharedFiles)
    }

    /// XEP-0424 retraction of one of our own messages. A room may reject
    /// it, so rooms wait for the reflected retraction.
    public func retract(_ item: TimelineItem) async -> Bool {
        guard item.isMine, let target = item.retractionTargetID, let from = ownJID(in: item.conversation) else { return false }
        guard await port.sendRetraction(of: target, in: item.conversation) else { return false }
        guard !item.conversation.isRoom else { return true }
        timelines.applyLocalMutation(.retraction(targetID: target, from: from), in: item.conversation)
        return true
    }

    /// XEP-0425 moderation of any message in a room we moderate. The room
    /// broadcasts the tombstone, so nothing is applied locally.
    public func moderate(_ item: TimelineItem, reason: String?) async -> Bool {
        guard item.conversation.isRoom, let target = item.actionTargetID else { return false }
        return await port.sendModeration(of: target, in: item.conversation.jid, reason: reason)
    }

    /// Whether we may moderate others' messages in `room`.
    public func canModerate(in conversation: ConversationID) -> Bool {
        guard conversation.isRoom else { return false }
        return selfOccupant(in: conversation.jid)?.canModerate == true
    }

    public func setPinned(_ pinned: Bool, _ item: TimelineItem) async -> Bool {
        guard let target = item.actionTargetID else { return false }
        return await port.setPinned(pinned, targetID: target, in: item.conversation)
    }

    public func isPinned(_ item: TimelineItem) -> Bool {
        guard item.conversation.isRoom, let target = item.actionTargetID else { return false }
        return pins.isPinned(target, in: item.conversation.jid)
    }

    // MARK: - Typing (XEP-0085)

    /// Call on every composer edit. Sends `composing` once, then `paused`
    /// after a quiet period.
    public func userTyped(in conversation: ConversationID) {
        guard connection == .online else { return }
        if sentChatStates[conversation] != .composing {
            sentChatStates[conversation] = .composing
            let port = self.port
            Task { _ = await port.sendChatState(.composing, in: conversation) }
        }
        typingPauseTasks[conversation]?.cancel()
        typingPauseTasks[conversation] = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            guard !Task.isCancelled, let self else { return }
            self.typingPauseTasks[conversation] = nil
            guard self.sentChatStates[conversation] == .composing else { return }
            self.sentChatStates[conversation] = .paused
            _ = await self.port.sendChatState(.paused, in: conversation)
        }
    }

    /// Ends our composing state without a message (draft cleared, screen
    /// left). A sent message ends it implicitly.
    public func stopTyping(in conversation: ConversationID, notify: Bool = false) {
        typingPauseTasks[conversation]?.cancel()
        typingPauseTasks[conversation] = nil
        let wasComposing = sentChatStates[conversation] == .composing
        sentChatStates[conversation] = nil
        if notify, wasComposing, connection == .online {
            let port = self.port
            Task { _ = await port.sendChatState(.active, in: conversation) }
        }
    }
}
