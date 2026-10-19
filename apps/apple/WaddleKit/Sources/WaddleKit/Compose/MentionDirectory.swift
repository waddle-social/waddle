import Foundation

/// Who a typed `@label` can address in a conversation. The Rust send
/// builder turns each typed token that names a target into an XEP-0372
/// mention reference, so a nick typed without the `@` popover still
/// addresses its occupant. Ranges the composer already references (popover
/// picks) are left alone.
public struct MentionDirectory: Hashable, Sendable, Codable {
    public struct Target: Hashable, Sendable, Codable {
        /// The label typed after `@` (a room nick). Matched case-insensitively.
        public let label: String
        public let jid: BareJID

        public init(label: String, jid: BareJID) {
            self.label = label
            self.jid = jid
        }
    }

    public var targets: [Target]
    /// Resolve `@everyone` / `@here` to broadcast mention URIs.
    public var broadcasts: Bool

    public init(targets: [Target], broadcasts: Bool) {
        self.targets = targets
        self.broadcasts = broadcasts
    }

    /// The people the `@` popover offers in a room: occupants whose real
    /// JID is known, other than us, then the XEP-0055 directory users the
    /// room's mention searches found (people with no MUC presence, labelled
    /// by username), plus the room broadcasts. An occupant's nick always
    /// wins: a directory user whose username collides with a label already
    /// listed is left out, as is our own account.
    public static func room(
        occupants: some Sequence<Occupant>,
        ownNick: String?,
        directoryUsers: [UserSearchResult] = [],
        ownJID: BareJID? = nil
    ) -> MentionDirectory {
        let present = occupants
            .filter { $0.nick != ownNick }
            .compactMap { occupant in occupant.realJID.map { Target(label: occupant.nick, jid: $0) } }
            .sorted { $0.label.lowercased() < $1.label.lowercased() }
        var labels = Set(present.map { $0.label.lowercased() })
        var absent: [Target] = []
        for user in directoryUsers.sorted(by: { $0.username.lowercased() < $1.username.lowercased() }) {
            guard user.jid != ownJID, labels.insert(user.username.lowercased()).inserted else { continue }
            absent.append(Target(label: user.username, jid: user.jid))
        }
        return MentionDirectory(targets: present + absent, broadcasts: true)
    }
}
