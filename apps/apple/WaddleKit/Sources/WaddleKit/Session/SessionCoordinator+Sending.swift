import Foundation

extension SessionCoordinator {
    /// Sends a draft. The row appears immediately as a local echo; its
    /// delivery state lives in `deliveries` under the returned client id.
    /// Offline sends queue and go out on the next ready session.
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
        await dispatch(message)
        return message.clientID
    }

    /// Re-sends a failed or queued own message under its original id.
    public func retry(clientID: String) async {
        if let queued = outboundQueue.first(where: { $0.clientID == clientID }) {
            outboundQueue.removeAll { $0.clientID == clientID }
            await dispatch(queued)
        } else if let failed = failedOutbound[clientID] {
            failedOutbound[clientID] = nil
            await dispatch(failed)
        }
    }

    /// Drops a failed or queued own message.
    public func discard(clientID: String, in conversation: ConversationID) {
        outboundQueue.removeAll { $0.clientID == clientID }
        failedOutbound[clientID] = nil
        deliveries.forget(clientID)
        timelines.removeLocalEcho(id: clientID, in: conversation)
    }

    func dispatch(_ message: OutboundMessage) async {
        deliveries.began(message.clientID)
        guard connection == .online else {
            enqueue(message)
            return
        }
        let outcome = await port.send(message)
        deliveries.outcome(outcome, for: message.clientID)
        switch outcome {
        case .sent:
            break
        case .notConnected, .transportError:
            enqueue(message)
        case .rejected:
            failedOutbound[message.clientID] = message
        }
    }

    func flushOutboundQueue() async {
        while connection == .online, !outboundQueue.isEmpty {
            let next = outboundQueue.removeFirst()
            await dispatch(next)
        }
    }

    private func enqueue(_ message: OutboundMessage) {
        deliveries.queued(message.clientID)
        if !outboundQueue.contains(where: { $0.clientID == message.clientID }) {
            outboundQueue.append(message)
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
        let senderKey = item.conversation.isRoom ? from.description : from.bare.description
        timelines.applyLocalMutation(
            .reaction(targetID: target, from: from, senderKey: senderKey, isMine: true, emojis: mine),
            in: item.conversation
        )
        return true
    }

    /// XEP-0308 correction of one of our own messages.
    public func edit(_ item: TimelineItem, to text: String) async -> Bool {
        guard item.isMine,
              let target = item.correctionTargetID,
              let from = ownJID(in: item.conversation),
              let composed = MessageComposer.compose(Draft(text: text, thread: item.message.thread))
        else { return false }
        let outcome = await port.sendCorrection(of: target, body: composed.body, in: item.conversation, options: composed.options)
        guard case .sent = outcome else { return false }
        timelines.applyLocalMutation(.correction(targetID: target, from: from, body: composed.body), in: item.conversation)
        return true
    }

    /// XEP-0424 retraction of one of our own messages.
    public func retract(_ item: TimelineItem) async -> Bool {
        guard item.isMine, let target = item.actionTargetID, let from = ownJID(in: item.conversation) else { return false }
        guard await port.sendRetraction(of: target, in: item.conversation) else { return false }
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
