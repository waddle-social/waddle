import Foundation

// MARK: - Message Sending, Chat States & Retraction

extension AppModel {
    func sendMessage(
        _ text: String,
        room: ChatRoomSelection?,
        replyTo: ChatTimelineMessage? = nil,
        threadRootID: String? = nil,
        sharedFiles: [WaddleSharedFile] = []
    ) async throws {
        guard let session else {
            throw ChatSendError.noSession
        }

        let channelID = room?.id ?? selectedChannelID
        guard let channelID else {
            throw ChatSendError.noRoom
        }

        guard let rustClient else {
            throw ChatSendError.noSession
        }

        guard let roomJID = channels.first(where: { $0.id == channelID })?.roomJid, !roomJID.isEmpty else {
            throw ChatSendError.noRoom
        }
        let trimmedText = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let bodyForWire = !trimmedText.isEmpty ? trimmedText : (sharedFiles.first?.url ?? "")
        guard !bodyForWire.isEmpty || !sharedFiles.isEmpty else {
            return
        }

        // Build the wire body. For replies we prepend a XEP-0428 fallback
        // quote (so non-supporting clients see the context) and compute the
        // Unicode-scalar range that supporting clients will strip.
        let (wireBody, fallbackRange) = composeWireBody(userText: bodyForWire, replyTo: replyTo)

        let optimisticID = UUID().uuidString
        let optimistic = ChatTimelineMessage(
            id: optimisticID,
            roomID: roomJID,
            senderID: session.username.lowercased(),
            senderDisplayName: session.username,
            body: wireBody,
            sentAt: Date(),
            editedAt: nil,
            deliveryState: .sending,
            isOutgoing: true,
            isAction: false,
            senderInitials: initials(from: session.username),
            reactions: nil,
            isRetracted: false,
            replyToID: replyTo?.id,
            replyToSenderName: replyTo?.senderDisplayName,
            replyToBody: replyTo?.displayBody,
            replyFallbackRange: fallbackRange,
            sharedFiles: sharedFiles.isEmpty ? nil : sharedFiles.map(timelineSharedFile(from:)),
            threadID: threadRootID
        )

        let messages = (messagesByRoomJID[roomJID] ?? []).appendingTimelineMessages([optimistic])
        messagesByRoomJID[roomJID] = messages
        pendingEchoBodies.insert(wireBody)

        if roomJID == currentRoomJID {
            syncChatMessages()
        }

        let (_, markupSpans) = parseMarkdownToMarkupSpans(trimmedText)

        // Compose structured send-options once so reply, thread, and fallback
        // metadata all travel together down the FFI in a single typed payload.
        let replyTarget: WaddleReplyTarget? = replyTo.map { target in
            // For MUC the reply `to` must be the occupant full JID
            // (room@muc.domain/nick) per XEP-0461; senderDisplayName is the
            // nick that will render as the resource.
            WaddleReplyTarget(
                authorJid: "\(roomJID)/\(target.senderDisplayName)",
                messageId: target.id
            )
        }
        let fallbackOpt: WaddleFallbackRange? = fallbackRange.map { range in
            WaddleFallbackRange(
                start: UInt32(range.lowerBound),
                end: UInt32(range.upperBound)
            )
        }
        let threadTarget: WaddleThreadTarget? = threadRootID.map { rootID in
            WaddleThreadTarget(id: rootID, parent: nil)
        }

        let hasOptions = replyTarget != nil || fallbackOpt != nil || threadTarget != nil || !sharedFiles.isEmpty
        let options: WaddleSendOptions? = hasOptions
            ? WaddleSendOptions(
                stanzaId: nil,
                subject: nil,
                reply: replyTarget,
                fallback: fallbackOpt,
                thread: threadTarget,
                markupSpans: [],
                references: [],
                sharedFiles: sharedFiles,
                linkPreviewToken: nil,
                requestDisplayedMarker: false,
                mucPm: false
            )
            : nil

        if options == nil, !markupSpans.isEmpty {
            await rustClient.sendGroupchatMessageWithMarkup(
                roomJID: roomJID,
                body: wireBody,
                spans: markupSpans
            )
        } else {
            await rustClient.sendGroupchatMessage(
                roomJID: roomJID,
                body: wireBody,
                options: options
            )
        }
    }

    /// Compose the outbound body for a send. For replies this prepends a
    /// XEP-0428 fallback quote (so non-supporting clients still see what is
    /// being quoted) and returns the Unicode-scalar range covering that
    /// prefix. Supporting clients use the range to hide the quote and render
    /// the structured reply-to indicator instead.
    private func composeWireBody(
        userText: String,
        replyTo: ChatTimelineMessage?
    ) -> (body: String, fallbackRange: Range<Int>?) {
        guard let replyTo else {
            return (userText, nil)
        }
        // Truncate quoted lines to keep fallback quotes readable when the
        // original body is long; supporting clients hide it anyway.
        let maxQuoteChars = 240
        let sourceBody = replyTo.displayBody
        let quoteBody: String = {
            if sourceBody.unicodeScalars.count <= maxQuoteChars {
                return sourceBody
            }
            let scalars = sourceBody.unicodeScalars
            let cutoff = scalars.index(scalars.startIndex, offsetBy: maxQuoteChars)
            return String(scalars[..<cutoff]) + "…"
        }()

        var quote = ""
        quote += "> "
        quote += replyTo.senderDisplayName
        quote += " wrote:\n"
        for line in quoteBody.split(separator: "\n", omittingEmptySubsequences: false) {
            quote += "> "
            quote += String(line)
            quote += "\n"
        }
        quote += "\n"

        let body = quote + userText
        let fallbackEnd = quote.unicodeScalars.count
        return (body, 0..<fallbackEnd)
    }

    func retractMessage(_ message: ChatTimelineMessage) async {
        guard let roomJID = currentRoomJID, let rustClient else { return }
        await rustClient.retractMessage(roomJID: roomJID, messageID: message.id)
    }

    func handleChatState(_ state: String, from nick: String) {
        if state == "composing" {
            addTypingUser(nick)
        } else {
            removeTypingUser(nick)
        }
    }

    private func addTypingUser(_ nick: String) {
        var users = chatStore.typingUsers
        if !users.contains(nick) {
            users.append(nick)
            chatStore.typingUsers = users
        }
        typingTimers[nick]?.cancel()
        typingTimers[nick] = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            self?.removeTypingUser(nick)
        }
    }

    func removeTypingUser(_ nick: String) {
        typingTimers[nick]?.cancel()
        typingTimers.removeValue(forKey: nick)
        var users = chatStore.typingUsers
        users.removeAll { $0 == nick }
        chatStore.typingUsers = users
    }

    func sendDisplayedMarkerForCurrentRoom(messageID: String) {
        guard let roomJID = currentRoomJID, let rustClient else { return }
        Task {
            await rustClient.sendDisplayedMarker(roomJID: roomJID, messageID: messageID)
        }
    }

    func notifyComposing() {
        guard let roomJID = currentRoomJID, let rustClient else { return }
        if lastSentChatState != "composing" {
            lastSentChatState = "composing"
            Task { await rustClient.sendChatState(roomJID: roomJID, state: "composing") }
        }
        composingTimer?.cancel()
        composingTimer = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            guard let self, self.lastSentChatState == "composing" else { return }
            self.lastSentChatState = "paused"
            if let roomJID = self.currentRoomJID, let rustClient = self.rustClient {
                await rustClient.sendChatState(roomJID: roomJID, state: "paused")
            }
        }
    }
}
