/// XEP-0272 setup owns the slot before its first presence send.
public struct GroupCallAttempt: Hashable, Sendable {
    public let room: BareJID
    public let sid: String
    public let media: CallMedia
    public let nick: String
    /// Only a content-declaring presence makes a pending mixer session terminable.
    public var isAdvertised: Bool

    public init(room: BareJID, sid: String, media: CallMedia, nick: String, isAdvertised: Bool = false) {
        self.room = room
        self.sid = sid
        self.media = media
        self.nick = nick
        self.isAdvertised = isAdvertised
    }
}

/// The mixer accepted this room's session and issued media credentials.
public struct GroupCall: Hashable, Sendable {
    public let room: BareJID
    public let sid: String
    public let media: CallMedia
    public let join: CallJoin
    public let nick: String

    public init(room: BareJID, sid: String, media: CallMedia, join: CallJoin, nick: String) {
        self.room = room
        self.sid = sid
        self.media = media
        self.join = join
        self.nick = nick
    }
}
