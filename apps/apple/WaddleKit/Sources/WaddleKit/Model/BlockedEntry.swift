/// One line of the block list as a person reads it. The list holds
/// addresses of several kinds (XEP-0191 §2): the app blocks people, and
/// another client may have blocked a whole server or one device.
public struct BlockedEntry: Hashable, Identifiable, Sendable {
    public let jid: JID
    /// The person's name, or what the address covers when it is not a person.
    public let title: String
    /// The address, when the title is not already it.
    public let detail: String?

    public var id: JID { jid }

    /// `name` gives what the app calls a person, as the directory does; nil when it knows no better than the address.
    public init(_ jid: JID, name: (BareJID) -> String?) {
        self.jid = jid
        let address = jid.description
        if jid.bare.localpart == nil {
            title = jid.resource == nil ? "Everyone at \(jid.bare.domain)" : address
            detail = nil
        } else if jid.resource != nil {
            // Not the device's own label: that is the protocol's word for it, and often a random string.
            title = "One of \(name(jid.bare) ?? jid.bare.description)'s devices"
            detail = address
        } else if let name = name(jid.bare), name != address {
            title = name
            detail = address
        } else {
            title = address
            detail = nil
        }
    }

    public var unblockLabel: String { "Unblock \(title)" }
}

/// What the app says about blocking, in one place.
public enum BlockText {
    public static func confirmTitle(_ name: String) -> String { "Block \(name)?" }

    /// What the server does: their direct messages and calls to us are
    /// refused, and ours to them. In a room you both are in, what they say
    /// still arrives, since the room sends it.
    public static let confirmMessage =
        "They can't message or call you directly, and you can't message them, until you unblock them. You still see each other in rooms you share."

    public static let blockedNote =
        "You blocked this person. They can't message or call you directly, and you can't message them."

    /// The person is blocked with their whole server, which another client did: not this person's to undo here.
    public static func coveredNote(_ entry: BlockedEntry) -> String {
        "\(entry.title) is blocked. Unblock them in Settings."
    }

    public static let offline = "The list loads when you're back online."

    public static let listTitle = "Blocked people"

    public static let howToBlock = "Block someone from their profile."

    public static let tryAgain = "Try again"

    public static let unblock = "Unblock"

    public static let failed = "That didn't go through. Check your connection and try again."

    public static let empty = "Nobody is blocked."

    public static let couldNotLoad = "The list couldn't be loaded, so it may be out of date."
}
