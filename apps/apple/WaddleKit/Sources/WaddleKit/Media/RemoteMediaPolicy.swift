/// Whether a message may load media, and which host to name when it waits.
public enum RemoteMediaDecision: Equatable, Sendable {
    case allowed
    case withheld(host: Host?)
}

/// Media trust follows the room and the sender. The URL's host does not
/// establish the provenance of the message that supplied it.
public struct RemoteMediaPolicy: Sendable {
    private let accountHost: Host

    public init(account: BareJID) {
        accountHost = Host(account.domain)
    }

    public func decision(for item: TimelineItem) -> RemoteMediaDecision {
        decision(for: item, matchesLocalRoomSend: { false })
    }

    func decision(for item: TimelineItem, matchesLocalRoomSend: () -> Bool) -> RemoteMediaDecision {
        // A local echo proves the original payload only. The timeline can
        // accept a replacement from a later occupant of the same nick.
        if item.isLocalEcho, !item.isEdited { return .allowed }
        if item.conversation.isRoom, let host = foreignRoomHost(item.conversation.jid) {
            return matchesLocalRoomSend() ? .allowed : .withheld(host: host)
        }
        // Keep checking the original sender because a correction may retain
        // the original files or preview images.
        if let host = item.authorJID.flatMap(foreignHost) ?? item.mediaCorrection?.author.flatMap(foreignHost) {
            return .withheld(host: host)
        }
        // A home room can withhold an occupant's address. In a DM without
        // a sender, there is no equivalent trusted room to fall back to.
        return item.authorJID != nil || item.conversation.isRoom ? .allowed : .withheld(host: nil)
    }

    private func foreignHost(_ jid: BareJID) -> Host? {
        let host = Host(jid.domain)
        return host.isHomeAccountHost(accountHost) ? nil : host
    }

    private func foreignRoomHost(_ jid: BareJID) -> Host? {
        let host = Host(jid.domain)
        return host.isHomeRoomHost(accountHost) ? nil : host
    }
}
