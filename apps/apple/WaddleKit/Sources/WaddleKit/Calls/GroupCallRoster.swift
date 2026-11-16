/// One person on the in-call roster of a room's call.
public struct GroupCallRosterRow: Hashable, Sendable, Identifiable {
    /// The label: a room nick, or an account's localpart until presence names it.
    public let nick: String
    /// The account behind the row, for its avatar; nil when unknown.
    public let jid: BareJID?
    public var isHandRaised: Bool
    public var isMuted: Bool
    public var isSpeaking: Bool
    /// The row is our own account.
    public var isSelf: Bool

    /// Rows are unique by label and account: two people may share a nick.
    public struct ID: Hashable, Sendable {
        public let nick: String
        public let jid: BareJID?
    }

    public var id: ID { ID(nick: nick, jid: jid) }

    public init(
        nick: String, jid: BareJID?, isHandRaised: Bool = false, isMuted: Bool = false,
        isSpeaking: Bool = false, isSelf: Bool = false
    ) {
        self.nick = nick
        self.jid = jid
        self.isHandRaised = isHandRaised
        self.isMuted = isMuted
        self.isSpeaking = isSpeaking
        self.isSelf = isSelf
    }
}

/// Who the in-call surface lists, as Android's `mucRosterOf` and
/// `rosterWithVoices` decide it.
public enum GroupCallRoster {
    /// While we are in the call the SFU is the truth for who is there;
    /// otherwise the room's Muji presence is.
    ///
    /// A live identity takes its label and badges from the presence entry
    /// whose real JID is that exact identity, or from the entry that matched
    /// another device of the same account; without one it is labelled by
    /// its localpart and shows no badges, since badges belong to a room nick.
    /// One person on two devices is one row, in the first device's place
    /// and under its nick even when the devices hold different nicks; the
    /// row has a hand up if either does and is muted only if both are.
    ///
    /// Then rows whose account is in `speaking` light up, and our own row
    /// moves to the front; everyone else keeps their order, so nobody jumps
    /// around while they talk.
    ///
    /// - Parameter live: the SFU's participant identities, ours included,
    ///   while we are in the call; empty when we are not.
    public static func rows(presence: [GroupCallParticipant], live: [JID], speaking: Set<BareJID>, own: BareJID) -> [GroupCallRosterRow] {
        let roster = onePerAccount(live.isEmpty ? presence.map(row(for:)) : liveRows(live, presence: presence))
        let marked = roster.map { row in
            var row = row
            row.isSpeaking = row.jid.map(speaking.contains) ?? false
            row.isSelf = row.jid == own
            return row
        }
        return marked.filter(\.isSelf) + marked.filter { !$0.isSelf }
    }

    /// The accounts behind the SFU's speaking identities. Two devices of one
    /// account are one person; an identity that is not an account's JID is
    /// dropped rather than guessed at.
    public static func speakers(_ identities: some Sequence<String>) -> Set<BareJID> {
        Set(identities.compactMap { JID(parsing: $0)?.bare }.filter { $0.localpart != nil })
    }

    /// Rows whose account is unknown are left alone: a shared nick is not
    /// proof of one person.
    private static func onePerAccount(_ rows: [GroupCallRosterRow]) -> [GroupCallRosterRow] {
        var merged: [GroupCallRosterRow] = []
        for row in rows {
            guard let jid = row.jid, let index = merged.firstIndex(where: { $0.jid == jid }) else {
                merged.append(row)
                continue
            }
            merged[index].isHandRaised = merged[index].isHandRaised || row.isHandRaised
            merged[index].isMuted = merged[index].isMuted && row.isMuted
        }
        return merged
    }

    private static func row(for participant: GroupCallParticipant) -> GroupCallRosterRow {
        GroupCallRosterRow(
            nick: participant.nick, jid: participant.realJID.flatMap { account($0) },
            isHandRaised: participant.isHandRaised, isMuted: participant.isMuted)
    }

    private static func liveRows(_ live: [JID], presence: [GroupCallParticipant]) -> [GroupCallRosterRow] {
        var ownerByIdentity: [JID: GroupCallParticipant] = [:]
        for participant in presence {
            if let realJID = participant.realJID { ownerByIdentity[realJID] = participant }
        }
        // Another device of an owner-matched live identity is the same account.
        var ownerByAccount: [BareJID: GroupCallParticipant] = [:]
        for identity in live {
            guard let owner = ownerByIdentity[identity], ownerByAccount[identity.bare] == nil else { continue }
            ownerByAccount[identity.bare] = owner
        }
        var rows: [GroupCallRosterRow] = []
        var isOwned: [Bool] = []
        for identity in live {
            let owner = ownerByIdentity[identity] ?? ownerByAccount[identity.bare]
            let row = GroupCallRosterRow(
                nick: owner?.nick ?? identity.bare.localpart ?? identity.bare.domain, jid: account(identity),
                isHandRaised: owner?.isHandRaised ?? false, isMuted: owner?.isMuted ?? false)
            if let index = rows.firstIndex(where: { $0.id == row.id }) {
                // Whichever device comes first, the row keeps the owner's badges.
                if !isOwned[index], owner != nil {
                    rows[index] = row
                    isOwned[index] = true
                }
            } else {
                rows.append(row)
                isOwned.append(owner != nil)
            }
        }
        return rows
    }

    /// Only a JID with a localpart is a person with an avatar.
    private static func account(_ jid: JID) -> BareJID? {
        jid.bare.localpart == nil ? nil : jid.bare
    }
}
