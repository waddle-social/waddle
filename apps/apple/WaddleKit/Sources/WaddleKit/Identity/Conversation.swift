import Foundation

/// Which kind of XMPP conversation a timeline belongs to.
public enum ConversationKind: Hashable, Sendable {
    /// A XEP-0045 room: a channel or a group DM.
    case room
    /// A 1:1 `type='chat'` conversation with a peer's bare JID.
    case direct
}

/// Stable identity of a conversation: the room bare JID or the peer bare
/// JID, plus its kind. Every store keys on this value.
public struct ConversationID: Hashable, Sendable, CustomStringConvertible {
    public let jid: BareJID
    public let kind: ConversationKind

    public init(jid: BareJID, kind: ConversationKind) {
        self.jid = jid
        self.kind = kind
    }

    public static func room(_ jid: BareJID) -> ConversationID {
        ConversationID(jid: jid, kind: .room)
    }

    public static func direct(_ jid: BareJID) -> ConversationID {
        ConversationID(jid: jid, kind: .direct)
    }

    public var isRoom: Bool { kind == .room }

    public var description: String {
        switch kind {
        case .room: return "room:\(jid)"
        case .direct: return "direct:\(jid)"
        }
    }
}

/// The signed-in account as the routing layer needs it.
public struct AccountIdentity: Hashable, Sendable {
    public let jid: BareJID
    /// The nickname used when joining rooms. Own MUC reflections arrive
    /// from `room/nick`, so authorship in rooms compares against this.
    public let nick: String

    public init(jid: BareJID, nick: String) {
        self.jid = jid
        self.nick = nick
    }
}

/// Where a message routes and whether the account authored it.
public struct MessageRoute: Hashable, Sendable {
    public let conversation: ConversationID
    public let isMine: Bool
}

public extension AccountIdentity {
    /// Routes a message: groupchat keys on the room bare JID and compares
    /// the occupant nick for authorship; 1:1 keys on the non-own side and
    /// compares bare JIDs (carbons of our own sends route to the peer).
    func route(from: JID?, to: JID?, isGroupchat: Bool) -> MessageRoute? {
        if isGroupchat {
            guard let room = from?.bare ?? to?.bare else { return nil }
            let isMine = from?.resource == nick
            return MessageRoute(conversation: .room(room), isMine: isMine)
        }
        let isMine = from?.bare == jid
        let peer = isMine ? (to?.bare ?? from?.bare) : (from?.bare ?? to?.bare)
        guard let peer else { return nil }
        return MessageRoute(conversation: .direct(peer), isMine: isMine)
    }
}
