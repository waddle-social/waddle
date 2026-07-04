import Foundation

// MARK: - Timeline & Incoming Group Messages

private struct TimelineEventDescriptor {
    let event: XMPPMessageEvent
    let fallbackID: String?
}

private struct TimelineCorrectionUpdate {
    let targetID: String
    let body: String
    let timestamp: Date?
}

private struct TimelineReactionUpdate {
    let targetID: String
    let senderName: String
    let emojis: [String]
}

extension AppModel {
    func loadRoomHistory(for room: ChatRoomSelection, before: Date?) async -> ChatRoomHistoryPage {
        dlog(" loadRoomHistory called for room.id=\(room.id) room.title=\(room.title) before=\(String(describing: before))")
        guard let session,
              let rustClient,
              let roomJID = roomJID(for: room.id) else {
            dlog(" loadRoomHistory: guard failed — session=\(self.session != nil) rustClient=\(self.rustClient != nil) roomJID=\(self.roomJID(for: room.id) ?? "nil")")
            return ChatRoomHistoryPage(messages: [], hasMoreOlderMessages: false)
        }

        dlog(" loadRoomHistory: roomJID=\(roomJID)")
        let requestBefore = before == nil ? "" : roomHistoryBeforeCursorByRoomJID[roomJID]
        if before != nil, requestBefore == nil {
            dlog(" loadRoomHistory: no cursor for older load, returning empty")
            return ChatRoomHistoryPage(messages: [], hasMoreOlderMessages: false)
        }

        let archivePage = await rustClient.fetchRoomHistory(
            roomJID: roomJID,
            max: UInt32(roomHistoryPageSize),
            before: requestBefore
        )

        let deltaMessages = timelineMessages(
            from: archivePage.messages.map { TimelineEventDescriptor(event: $0.message, fallbackID: $0.mamID ?? $0.stanzaID) },
            roomJID: roomJID,
            session: session
        )

        var mergedMessages = (messagesByRoomJID[roomJID] ?? []).appendingTimelineMessages(deltaMessages)

        // Back-fill reply previews for messages whose parent wasn't loaded yet
        for i in mergedMessages.indices {
            if mergedMessages[i].replyToID != nil && mergedMessages[i].replyToBody == nil {
                if let parent = mergedMessages.first(where: { $0.id == mergedMessages[i].replyToID }) {
                    mergedMessages[i].replyToSenderName = parent.senderDisplayName
                    mergedMessages[i].replyToBody = String(parent.body.prefix(100))
                }
            }
        }

        messagesByRoomJID[roomJID] = mergedMessages
        if roomJID == currentRoomJID {
            syncChatMessages()
        }
        dlog(" loadRoomHistory: roomJID=\(roomJID) archive=\(archivePage.messages.count) delta=\(deltaMessages.count) merged=\(mergedMessages.count)")

        let nextBeforeCursor = archivePage.pageInfo.first ?? archivePage.messages.first?.mamID ?? archivePage.messages.first?.stanzaID
        let hasMoreOlderMessages = !archivePage.pageInfo.isComplete
            && nextBeforeCursor != nil
            && nextBeforeCursor != requestBefore

        if let nextBeforeCursor, hasMoreOlderMessages {
            roomHistoryBeforeCursorByRoomJID[roomJID] = nextBeforeCursor
        } else {
            roomHistoryBeforeCursorByRoomJID.removeValue(forKey: roomJID)
        }

        syncChatRooms()

        return ChatRoomHistoryPage(
            messages: deltaMessages,
            hasMoreOlderMessages: hasMoreOlderMessages
        )
    }

    func handleIncomingMessage(_ event: XMPPMessageEvent) {
        guard let session else { return }

        if event.type == "chat" {
            handleIncomingDm(event)
            return
        }

        let roomJID = barePeerJID(event.from ?? event.to ?? "")
        guard parseManagedRoomBareJID(roomJID) != nil else {
            return
        }

        let senderNick = XMPPJID(string: event.from ?? "")?.resource ?? ""

        if let chatState = event.chatState, senderNick != session.username, roomJID == currentRoomJID {
            handleChatState(chatState, from: senderNick)
            if event.body == nil, event.subject == nil, event.replacesID == nil,
               event.retractsID == nil, event.reactionTargetID == nil, event.displayedMarkerID == nil {
                return
            }
        }

        if event.displayedMarkerID != nil {
            return
        }

        let deltaMessages = timelineMessages(
            from: [TimelineEventDescriptor(event: event, fallbackID: nil)],
            roomJID: roomJID,
            session: session
        )
        guard !deltaMessages.isEmpty else {
            return
        }

        var existing = messagesByRoomJID[roomJID] ?? []

        if senderNick == session.username {
            for delta in deltaMessages {
                if pendingEchoBodies.contains(delta.body) {
                    pendingEchoBodies.remove(delta.body)
                    existing.removeAll { $0.isOutgoing && $0.deliveryState == .sending && $0.body == delta.body }
                }
            }
        }

        let messages = existing.appendingTimelineMessages(deltaMessages)
        messagesByRoomJID[roomJID] = messages

        if senderNick != session.username, roomJID == currentRoomJID {
            removeTypingUser(senderNick)
        }

        syncChatRooms()
        if roomJID == currentRoomJID {
            syncChatMessages()
            updateChatSurfaceState()
        }

        if roomJID == currentRoomJID, !deltaMessages.isEmpty,
           let lastMessage = deltaMessages.last, !lastMessage.isOutgoing {
            sendDisplayedMarkerForCurrentRoom(messageID: lastMessage.id)
        }

        for message in deltaMessages where !message.isOutgoing {
            if message.broadcastMention != nil {
                let incomingRoomJID = roomJID
                let channelName = channels.first(where: { self.roomJID(for: $0.id) == incomingRoomJID })?.name
                showNotificationToast(sender: message.senderDisplayName, body: message.body, channelName: channelName)
            }
        }
    }

    private func showNotificationToast(sender: String, body: String, channelName: String?) {
        let toast = ChatNotificationToast(
            senderName: sender,
            body: String(body.prefix(100)),
            channelName: channelName
        )
        chatStore.notificationToast = toast
        toastDismissTask?.cancel()
        toastDismissTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            guard !Task.isCancelled else { return }
            if self?.chatStore.notificationToast?.id == toast.id {
                self?.chatStore.notificationToast = nil
            }
        }
    }

    private func timelineMessages(
        from descriptors: [TimelineEventDescriptor],
        roomJID: String,
        session: WaddleSession
    ) -> [ChatTimelineMessage] {
        let existingTimeline = messagesByRoomJID[roomJID] ?? []
        var workingByID = Dictionary(uniqueKeysWithValues: existingTimeline.map { ($0.id, $0) })
        var deltaByID: [String: ChatTimelineMessage] = [:]
        var corrections: [TimelineCorrectionUpdate] = []
        var retractions: [String] = []
        var reactions: [TimelineReactionUpdate] = []

        for descriptor in descriptors {
            let event = descriptor.event
            let senderName = XMPPJID(string: event.from ?? "")?.resource ?? "Unknown"

            if let targetID = event.reactionTargetID, !event.reactionEmojis.isEmpty {
                reactions.append(
                    TimelineReactionUpdate(
                        targetID: targetID,
                        senderName: senderName,
                        emojis: event.reactionEmojis
                    )
                )
                continue
            }

            if let targetID = event.retractsID {
                retractions.append(targetID)
                continue
            }

            if let targetID = event.replacesID {
                corrections.append(
                    TimelineCorrectionUpdate(
                        targetID: targetID,
                        body: (event.body ?? event.subject ?? "").trimmingCharacters(in: .whitespacesAndNewlines),
                        timestamp: event.timestamp
                    )
                )
                continue
            }

            guard let message = timelineMessage(from: event, fallbackID: descriptor.fallbackID, roomJID: roomJID, session: session) else {
                continue
            }

            let merged = workingByID[message.id]?.merged(with: message) ?? message
            workingByID[message.id] = merged
            deltaByID[message.id] = merged
        }

        for correction in corrections {
            guard var target = workingByID[correction.targetID] else {
                continue
            }

            if !correction.body.isEmpty {
                target.body = correction.body
            }
            target.editedAt = latestDate(target.editedAt, correction.timestamp ?? target.sentAt)
            workingByID[target.id] = target
            deltaByID[target.id] = target
        }

        for targetID in retractions {
            guard var target = workingByID[targetID] else {
                continue
            }

            target.body = ""
            target.isRetracted = true
            workingByID[target.id] = target
            deltaByID[target.id] = target
        }

        for reaction in reactions {
            guard var target = workingByID[reaction.targetID] else {
                continue
            }

            var mergedReactions = target.reactions ?? [:]
            for emoji in reaction.emojis {
                var senders = mergedReactions[emoji] ?? []
                if !senders.contains(reaction.senderName) {
                    senders.append(reaction.senderName)
                }
                mergedReactions[emoji] = senders
            }

            target.reactions = mergedReactions.isEmpty ? nil : mergedReactions
            workingByID[target.id] = target
            deltaByID[target.id] = target
        }

        return deltaByID.values.sorted {
            if $0.sentAt == $1.sentAt {
                return $0.id < $1.id
            }
            return $0.sentAt < $1.sentAt
        }
    }

    private func timelineMessage(
        from event: XMPPMessageEvent,
        fallbackID: String?,
        roomJID: String,
        session: WaddleSession
    ) -> ChatTimelineMessage? {
        let text = (event.body ?? event.subject ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty || !event.sharedFiles.isEmpty else {
            return nil
        }

        let senderName = XMPPJID(string: event.from ?? "")?.resource ?? "Unknown"
        let messageID = event.id ?? event.stanzaID ?? fallbackID ?? UUID().uuidString

        var replyToSenderName: String?
        var replyToBody: String?
        if let replyToID = event.replyToID {
            if let parentMessage = messagesByRoomJID[roomJID]?.first(where: { $0.id == replyToID }) {
                replyToSenderName = parentMessage.senderDisplayName
                // Preview should reflect what the user saw on their screen,
                // not the wire body (which may include a XEP-0428 fallback
                // quote).
                replyToBody = parentMessage.displayBody
            } else if let sender = event.replyToSender {
                replyToSenderName = XMPPJID(string: sender)?.resource ?? sender
            }
        }

        return ChatTimelineMessage(
            id: messageID,
            roomID: roomJID,
            senderID: senderName.lowercased(),
            senderDisplayName: senderName,
            body: text,
            sentAt: event.timestamp ?? Date(),
            editedAt: nil,
            deliveryState: .delivered,
            isOutgoing: senderName == session.username,
            isAction: event.type == "subject" || (event.body == nil && event.subject != nil),
            senderInitials: initials(from: senderName),
            reactions: nil,
            isRetracted: false,
            replyToID: event.replyToID,
            replyToSenderName: replyToSenderName,
            replyToBody: replyToBody,
            replyFallbackRange: event.replyFallbackRange,
            markupSpans: event.markupSpans.isEmpty ? nil : event.markupSpans,
            sharedFiles: event.sharedFiles.isEmpty ? nil : event.sharedFiles,
            broadcastMention: event.broadcastMention,
            hatTitles: hatsByRoomJID[roomJID]?[senderName]?.map(\.title),
            mentionURIs: event.mentionURIs.isEmpty ? nil : event.mentionURIs,
            forumPostKind: event.forumPostKind,
            forumTitle: event.forumTitle,
            threadID: event.threadID,
            parentThreadID: event.parentThreadID,
            isSticker: event.isSticker ? true : nil
        )
    }

    private func latestDate(_ lhs: Date?, _ rhs: Date?) -> Date? {
        switch (lhs, rhs) {
        case let (lhs?, rhs?):
            return max(lhs, rhs)
        case let (lhs?, nil):
            return lhs
        case let (nil, rhs?):
            return rhs
        case (nil, nil):
            return nil
        }
    }
}
