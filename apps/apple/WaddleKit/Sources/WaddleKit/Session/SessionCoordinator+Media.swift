extension SessionCoordinator {
    public func remoteMediaDecision(for item: TimelineItem) -> RemoteMediaDecision {
        let decision = mediaAccess.policy.decision(for: item) { matchesLocalRoomSend(item) }
        guard case .withheld = decision else { return decision }
        return mediaAccess.consent.contains(item, urls: MediaURLDigest(item)) ? .allowed : decision
    }

    public func loadRemoteMedia(for item: TimelineItem) {
        guard !isStopped else { return }
        mediaAccess.consent.grant(item)
    }

    func recordRoomMediaCorrection(
        _ originID: String, targetID: String, replacing item: TimelineItem, content: CorrectedContent
    ) {
        // The editor re-sends the row's files on a text edit. That action
        // cannot authorize inherited files that were still withheld.
        guard !isStopped, content.sharedFiles.isEmpty || remoteMediaDecision(for: item) == .allowed else { return }
        let key = RoomMediaCorrection(conversation: item.conversation, originID: originID, targetID: targetID)
        mediaAccess.sentCorrections[key] = MediaURLDigest(body: content.body, files: content.sharedFiles)
    }

    private func matchesLocalRoomSend(_ item: TimelineItem) -> Bool {
        // A room sees origin IDs and can copy them onto altered messages.
        // Require the same media destinations that this client sent.
        if item.isEdited {
            guard let correction = item.mediaCorrection, let originID = correction.originID else { return false }
            let key = RoomMediaCorrection(conversation: item.conversation, originID: originID, targetID: correction.targetID)
            guard let sent = mediaAccess.sentCorrections[key] else { return false }
            return sent == MediaURLDigest(item)
        }
        guard let originID = item.identity.originID,
              let sent = sentOutbound[originID] ?? recentlyAcknowledgedOutbound[originID] ?? failedOutbound[originID]
                  ?? outboundQueue.first(where: { $0.clientID == originID }),
              sent.conversation == item.conversation
        else { return false }
        let body = ReplyFallback.strip(sent.body, range: sent.options.reply?.fallback)
        return MediaURLDigest(body: body, files: sent.options.sharedFiles) == MediaURLDigest(item)
    }
}

struct RoomMediaCorrection: Hashable {
    let conversation: ConversationID
    let originID: String
    let targetID: String
}

struct SessionMediaAccess {
    let policy: RemoteMediaPolicy
    var consent = RemoteMediaConsent()
    var sentCorrections: [RoomMediaCorrection: MediaURLDigest] = [:]

    mutating func clear() {
        consent = RemoteMediaConsent()
        sentCorrections.removeAll()
    }
}
