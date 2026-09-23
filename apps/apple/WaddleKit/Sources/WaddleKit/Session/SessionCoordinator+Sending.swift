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
        guard let failed = failedOutbound[clientID] else { return }
        let bouncedRetry = deliveries.wasBounced(clientID)
        failedOutbound[clientID] = nil
        retryingOutboundIDs.insert(clientID)
        enqueue(failed)
        if bouncedRetry, status.connection == .online {
            // The reused id can receive a delayed ack from the bounced
            // attempt. Retire that stream before sending the retry so the
            // old ack cannot settle the new attempt.
            await resetStaleConnection()
            return
        }
        await flushOutboundQueue()
    }

    /// Drops a failed or queued own message.
    public func discard(clientID: String, in conversation: ConversationID) {
        outboundQueue.removeAll { $0.clientID == clientID }
        retryingOutboundIDs.remove(clientID)
        resetBeforeRetryIDs.remove(clientID)
        failedOutbound[clientID] = nil
        sentOutbound[clientID] = nil
        sentOrder.removeAll { $0 == clientID }
        deliveries.forget(clientID)
        timelines.removeLocalEcho(id: clientID, in: conversation)
        persistOutbox()
    }

    /// Sends queued messages in order. A transient failure stops the drain
    /// and keeps the message at the head for the next ready session.
    func flushOutboundQueue() async {
        guard canFlushOutboundQueue, !isFlushing else { return }
        isFlushing = true
        defer {
            isFlushing = false
            if canFlushOutboundQueue, !outboundQueue.isEmpty {
                Task { [weak self] in await self?.flushOutboundQueue() }
            }
        }
        while canFlushOutboundQueue, let next = outboundQueue.first {
            if resetBeforeRetryIDs.remove(next.clientID) != nil {
                await resetStaleConnection()
                return
            }
            retryingOutboundIDs.remove(next.clientID)
            deliveries.began(next.clientID)
            let sendEpoch = connectionEpoch
            let outcome = await port.send(next)
            let remainsTracked = outboundQueue.contains { $0.clientID == next.clientID }
                || failedOutbound[next.clientID] != nil
                || sentOutbound[next.clientID] != nil
                || deliveries.state(of: next.clientID) != nil
            // The user may discard a failed item while the port call is
            // suspended. Do not let its late result recreate that message.
            guard remainsTracked else { continue }
            // A retry may have been queued while this attempt was suspended.
            // Ignore the earlier continuation; the queued retry owns the id.
            guard !retryingOutboundIDs.contains(next.clientID) else {
                persistOutbox()
                return
            }
            deliveries.outcome(outcome, for: next.clientID)
            // Act on the settled state: an ack or failure that arrived while
            // the send was suspended outranks the call's own result.
            let state = deliveries.state(of: next.clientID)
            if state == .acknowledged {
                sentMessageAcknowledged(next.clientID)
                persistOutbox()
                continue
            }
            if outcome == .rejected || state == .failed {
                outboundQueue.removeAll { $0.clientID == next.clientID }
                failedOutbound[next.clientID] = next
                persistOutbox()
                continue
            }
            if case .notConnected = outcome {
                deliveries.queued(next.clientID)
                if !isConnectResetting,
                   status.connection == .online,
                   sendEpoch == connectionEpoch
                {
                    await resetStaleConnection()
                }
                persistOutbox()
                return
            }
            if case .transportError = outcome {
                deliveries.queued(next.clientID)
                if !isConnectResetting,
                   status.connection == .online,
                   sendEpoch == connectionEpoch
                {
                    await resetStaleConnection()
                }
                persistOutbox()
                return
            }
            // A send can finish after its stream was retired. Its result is
            // uncertain even if it reports that the old driver accepted it;
            // leave it queued for a fresh stream under the same client id.
            guard !isConnectResetting,
                  status.connection == .online,
                  sendEpoch == connectionEpoch
            else {
                deliveries.queued(next.clientID)
                persistOutbox()
                return
            }
            if case .sent = outcome {
                outboundQueue.removeAll { $0.clientID == next.clientID }
                rememberSent(next)
                persistOutbox()
            }
        }
    }

    private var canFlushOutboundQueue: Bool {
        isSendReady
            && foregroundProbeTask == nil
            && !isConnectResetting
            && status.connection == .online
    }

    private func enqueue(_ message: OutboundMessage) {
        deliveries.queued(message.clientID)
        if !outboundQueue.contains(where: { $0.clientID == message.clientID }) {
            outboundQueue.append(message)
        }
        // Saved before the port sees it, so a kill mid-send loses nothing.
        persistOutbox()
    }

    /// Keeps every unconfirmed written message so disconnect replay and
    /// outbox persistence cannot drop one before an ack or server echo.
    private func rememberSent(_ message: OutboundMessage) {
        removeRecentlyAcknowledged(message.clientID)
        sentOutbound[message.clientID] = message
        sentOrder.removeAll { $0 == message.clientID }
        sentOrder.append(message.clientID)
    }

    /// Moves a confirmed stanza out of the retry set but retains a bounded
    /// window for recipient bounces that follow stream acknowledgement.
    func sentMessageAcknowledged(_ clientID: String) {
        let message = sentOutbound.removeValue(forKey: clientID)
            ?? failedOutbound.removeValue(forKey: clientID)
            ?? outboundQueue.first { $0.clientID == clientID }
            ?? recentlyAcknowledgedOutbound[clientID]
        sentOrder.removeAll { $0 == clientID }
        failedOutbound[clientID] = nil
        outboundQueue.removeAll { $0.clientID == clientID }
        retryingOutboundIDs.remove(clientID)
        resetBeforeRetryIDs.remove(clientID)
        if let message {
            rememberRecentlyAcknowledged(message)
        }
    }

    /// The written message failed after all (XEP-0198 or an error bounce):
    /// make it retryable.
    func sentMessageFailed(_ clientID: String, bounced: Bool) {
        let awaitingRetry = retryingOutboundIDs.contains(clientID)
            && outboundQueue.contains { $0.clientID == clientID }
        if awaitingRetry {
            if bounced {
                deliveries.bounced(clientID)
                if status.connection == .online, !isConnectResetting {
                    resetBeforeRetryIDs.insert(clientID)
                }
            }
            // A failure event from the previous attempt must not cancel an
            // explicit retry before that retry gets its own send attempt.
            persistOutbox()
            return
        }
        let message = sentOutbound.removeValue(forKey: clientID)
            ?? outboundQueue.first { $0.clientID == clientID }
            ?? recentlyAcknowledgedOutbound.removeValue(forKey: clientID)
            ?? failedOutbound[clientID]
        sentOrder.removeAll { $0 == clientID }
        if message != nil {
            outboundQueue.removeAll { $0.clientID == clientID }
        }
        if bounced {
            deliveries.bounced(clientID)
        } else {
            deliveries.failed(clientID)
        }
        if deliveries.state(of: clientID) == .failed, let message {
            failedOutbound[clientID] = message
        } else if deliveries.state(of: clientID) == .acknowledged, let message {
            failedOutbound[clientID] = nil
            rememberRecentlyAcknowledged(message)
        }
        persistOutbox()
    }

    private func rememberRecentlyAcknowledged(_ message: OutboundMessage) {
        let clientID = message.clientID
        recentlyAcknowledgedOutbound[clientID] = message
        recentlyAcknowledgedOrder.removeAll { $0 == clientID }
        recentlyAcknowledgedOrder.append(clientID)
        if recentlyAcknowledgedOrder.count > 200 {
            let expired = recentlyAcknowledgedOrder.removeFirst()
            recentlyAcknowledgedOutbound[expired] = nil
            if deliveries.state(of: expired) == .acknowledged {
                deliveries.forget(expired)
            }
        }
    }

    private func removeRecentlyAcknowledged(_ clientID: String) {
        recentlyAcknowledgedOutbound[clientID] = nil
        recentlyAcknowledgedOrder.removeAll { $0 == clientID }
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
    /// re-sends the whole message, as the XEP requires: the draft's text
    /// and mentions (markup and references are recomputed against the new
    /// body) with the original's reply, thread and attachments. The draft's
    /// own reply, thread and attachments are ignored.
    public func edit(_ item: TimelineItem, draft: Draft) async -> Bool {
        guard item.isMine,
              item.tombstone == nil,
              let target = item.correctionTargetID,
              let from = ownJID(in: item.conversation),
              let composed = MessageComposer.compose(correctionDraft(of: item, draft: draft))
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

    private func correctionDraft(of item: TimelineItem, draft: Draft) -> Draft {
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
        return Draft(
            text: draft.text,
            mentions: draft.mentions,
            reply: reply,
            thread: item.message.thread,
            attachments: item.message.sharedFiles
        )
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
