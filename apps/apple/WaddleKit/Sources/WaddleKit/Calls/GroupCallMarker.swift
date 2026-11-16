/// XEP-0272 `<muji/>` on room presence, plus Waddle's in-call markers.
public struct GroupCallMarker: Hashable, Sendable {
    public let isPreparing: Bool
    /// Non-nil only when the occupant declares active call contents.
    public let media: CallMedia?
    public let isHandRaised: Bool
    public let isMuted: Bool

    public init(isPreparing: Bool, media: CallMedia?, isHandRaised: Bool = false, isMuted: Bool = false) {
        self.isPreparing = isPreparing
        self.media = media
        self.isHandRaised = isHandRaised
        self.isMuted = isMuted
    }
}

/// An active room-call occupant, attributed to the real JID when disclosed.
public struct GroupCallParticipant: Hashable, Sendable {
    public let nick: String
    public let realJID: JID?
    public let media: CallMedia
    public let isHandRaised: Bool
    public let isMuted: Bool

    public init(nick: String, realJID: JID?, media: CallMedia, isHandRaised: Bool = false, isMuted: Bool = false) {
        self.nick = nick
        self.realJID = realJID
        self.media = media
        self.isHandRaised = isHandRaised
        self.isMuted = isMuted
    }
}
