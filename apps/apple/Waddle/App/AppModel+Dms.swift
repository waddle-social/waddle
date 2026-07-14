import Foundation

// MARK: - Direct Messages

extension AppModel {
    func openDm(peerJID: String, peerUsername: String) async {
        let bareJID = barePeerJID(peerJID)
        ensureDmConversation(peerJID: bareJID, peerUsername: peerUsername)
        chatStore.activeDmPeerJID = bareJID
        markDmRead(peerJID: bareJID)
        chatStore.dmMessages = dmMessagesByPeer[bareJID] ?? []

        guard let rustClient, let session else { return }
        let archive = await rustClient.fetchDmHistory(peerJID: bareJID, max: 50)
        let messages = archive.messages.compactMap { archiveMsg -> ChatTimelineMessage? in
            let event = archiveMsg.message
            let text = (event.body ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty || !event.sharedFiles.isEmpty else { return nil }
            let senderBare = barePeerJID(event.from ?? "")
            let senderName = XMPPJID(string: event.from ?? "")?.localpart ?? senderBare
            let isOutgoing = senderBare == barePeerJID(session.jid)
            let messageID = event.id ?? event.stanzaID ?? archiveMsg.mamID ?? UUID().uuidString
            return ChatTimelineMessage(
                id: messageID,
                roomID: bareJID,
                senderID: senderBare,
                senderDisplayName: isOutgoing ? session.username : peerUsername,
                body: text,
                sentAt: archiveMsg.delayedDeliveryTimestamp ?? event.timestamp ?? Date(),
                editedAt: nil,
                deliveryState: .delivered,
                isOutgoing: isOutgoing,
                isAction: false,
                senderInitials: initials(from: isOutgoing ? session.username : peerUsername),
                reactions: nil,
                isRetracted: false,
                sharedFiles: event.sharedFiles.isEmpty ? nil : event.sharedFiles
            )
        }
        dmMessagesByPeer[bareJID] = messages.sorted { $0.sentAt < $1.sentAt }
        if chatStore.activeDmPeerJID == bareJID {
            chatStore.dmMessages = dmMessagesByPeer[bareJID] ?? []
        }
    }

    func sendDm(body: String, sharedFiles: [WaddleSharedFile] = [], peerJID: String? = nil) async {
        guard let targetPeerJID = peerJID ?? chatStore.activeDmPeerJID,
              let rustClient,
              let session else {
            return
        }

        let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
        let wireBody = !text.isEmpty ? text : (sharedFiles.first?.url ?? "")
        guard !wireBody.isEmpty || !sharedFiles.isEmpty else { return }

        let optimistic = ChatTimelineMessage(
            id: UUID().uuidString,
            roomID: targetPeerJID,
            senderID: barePeerJID(session.jid),
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
            sharedFiles: sharedFiles.isEmpty ? nil : sharedFiles.map(timelineSharedFile(from:))
        )
        var messages = dmMessagesByPeer[targetPeerJID] ?? []
        messages.append(optimistic)
        dmMessagesByPeer[targetPeerJID] = messages
        if chatStore.activeDmPeerJID == targetPeerJID {
            chatStore.dmMessages = messages
        }

        let options = sharedFiles.isEmpty
            ? nil
            : WaddleSendOptions(
                stanzaId: nil,
                subject: nil,
                reply: nil,
                fallback: nil,
                thread: nil,
                markupSpans: [],
                references: [],
                sharedFiles: sharedFiles,
                linkPreviewToken: nil,
                requestDisplayedMarker: false,
                mucPm: false
            )
        await rustClient.sendDirectMessage(peerJID: targetPeerJID, body: wireBody, options: options)
        updateDmConversation(
            peerJID: targetPeerJID,
            body: dmConversationPreview(body: text, sharedFiles: sharedFiles.map(timelineSharedFile(from:))),
            date: Date()
        )
    }

    func closeDm() {
        chatStore.activeDmPeerJID = nil
        chatStore.dmMessages = []
        chatStore.dmComposerText = ""
    }

    func handleIncomingDm(_ event: XMPPMessageEvent) {
        guard let session else { return }
        guard event.type == "chat" || event.type == "normal" || event.type == nil else { return }
        let text = (event.body ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty || !event.sharedFiles.isEmpty else { return }

        let fromBare = barePeerJID(event.from ?? "")
        let toBare = barePeerJID(event.to ?? "")
        let selfBare = barePeerJID(session.jid)
        let isSelf = fromBare == selfBare
        let peerJID = isSelf ? toBare : fromBare
        let peerUsername = XMPPJID(string: isSelf ? (event.to ?? "") : (event.from ?? ""))?.localpart ?? peerJID

        if peerJID.contains("@muc.") { return }

        let messageID = event.id ?? event.stanzaID ?? UUID().uuidString
        let message = ChatTimelineMessage(
            id: messageID,
            roomID: peerJID,
            senderID: fromBare,
            senderDisplayName: isSelf ? session.username : peerUsername,
            body: text,
            sentAt: event.timestamp ?? Date(),
            editedAt: nil,
            deliveryState: .delivered,
            isOutgoing: isSelf,
            isAction: false,
            senderInitials: initials(from: isSelf ? session.username : peerUsername),
            reactions: nil,
            isRetracted: false,
            markupSpans: event.markupSpans.isEmpty ? nil : event.markupSpans,
            sharedFiles: event.sharedFiles.isEmpty ? nil : event.sharedFiles
        )

        if isSelf {
            var msgs = dmMessagesByPeer[peerJID] ?? []
            msgs.removeAll { $0.isOutgoing && $0.deliveryState == .sending && $0.body == message.body }
            msgs.append(message)
            dmMessagesByPeer[peerJID] = msgs
        } else {
            var msgs = dmMessagesByPeer[peerJID] ?? []
            msgs.append(message)
            dmMessagesByPeer[peerJID] = msgs
        }

        ensureDmConversation(peerJID: peerJID, peerUsername: peerUsername)
        updateDmConversation(
            peerJID: peerJID,
            body: dmConversationPreview(body: text, sharedFiles: event.sharedFiles),
            date: event.timestamp ?? Date()
        )

        if !isSelf, chatStore.activeDmPeerJID != peerJID {
            incrementDmUnread(peerJID: peerJID)
        }

        if chatStore.activeDmPeerJID == peerJID {
            chatStore.dmMessages = dmMessagesByPeer[peerJID] ?? []
        }
    }

    private func ensureDmConversation(peerJID: String, peerUsername: String) {
        if !chatStore.dmConversations.contains(where: { $0.peerJID == peerJID }) {
            chatStore.dmConversations.append(DmConversation(
                id: peerJID,
                peerJID: peerJID,
                peerUsername: peerUsername,
                unreadCount: 0,
                presenceShow: dmPresence[peerJID] ?? .offline
            ))
        }
    }

    private func updateDmConversation(peerJID: String, body: String, date: Date) {
        if let idx = chatStore.dmConversations.firstIndex(where: { $0.peerJID == peerJID }) {
            chatStore.dmConversations[idx].lastMessageBody = body
            chatStore.dmConversations[idx].lastMessageAt = date
        }
    }

    private func incrementDmUnread(peerJID: String) {
        if let idx = chatStore.dmConversations.firstIndex(where: { $0.peerJID == peerJID }) {
            chatStore.dmConversations[idx].unreadCount += 1
        }
    }

    private func markDmRead(peerJID: String) {
        if let idx = chatStore.dmConversations.firstIndex(where: { $0.peerJID == peerJID }) {
            chatStore.dmConversations[idx].unreadCount = 0
        }
    }

    private func dmConversationPreview(body: String, sharedFiles: [XMPPSharedFile]) -> String {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty, !sharedFiles.contains(where: { $0.url == trimmed }) {
            return trimmed
        }
        if let first = sharedFiles.first {
            return first.name ?? "Sent an attachment"
        }
        return trimmed
    }
}
