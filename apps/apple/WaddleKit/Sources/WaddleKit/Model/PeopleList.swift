import Foundation

/// Someone on the account's contact list: their address and the name the
/// account saved for them, when it saved one.
public struct Contact: Hashable, Sendable {
    public let jid: BareJID
    public let name: String?

    public init(jid: BareJID, name: String? = nil) {
        self.jid = jid
        self.name = name
    }
}

/// Someone the account knows by address, as the People view lists them.
public struct Person: Hashable, Sendable, Identifiable {
    public let jid: BareJID
    public let name: String
    public let availability: Availability
    public let status: String?

    public var id: BareJID { jid }

    public init(jid: BareJID, name: String, availability: Availability, status: String?) {
        self.jid = jid
        self.name = name
        self.availability = availability
        self.status = status
    }
}

/// Everyone the account knows by address: its contacts, anyone else whose
/// presence it sees, and those it has direct messages with, split into who
/// is here now and who is not.
public struct PeopleList: Equatable, Sendable {
    /// Available, free to chat or do-not-disturb.
    public let here: [Person]
    /// Away, or not online.
    public let away: [Person]

    public var isEmpty: Bool { here.isEmpty && away.isEmpty }

    /// `own` is left out: the account's other devices send presence too.
    /// A contact's saved name is used when there is one, else `name`.
    /// `query` keeps people whose name or address contains it.
    public static func build(
        contacts: [Contact],
        presence: [BareJID: PresenceStore.ContactPresence],
        directPeers: [BareJID],
        own: BareJID,
        name: (BareJID) -> String,
        query: String = ""
    ) -> PeopleList {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let savedNames = Dictionary(contacts.map { ($0.jid, $0.name) }, uniquingKeysWith: { first, _ in first })
        let people = Set(savedNames.keys).union(presence.keys).union(directPeers).subtracting([own])
            .map { jid in
                Person(
                    jid: jid, name: savedNames[jid].flatMap { $0 } ?? name(jid),
                    availability: presence[jid]?.availability ?? .offline, status: presence[jid]?.status
                )
            }
            .filter { person in
                query.isEmpty || person.name.localizedStandardContains(query)
                    || person.jid.description.localizedStandardContains(query)
            }
            .sorted(by: inOrder)
        return PeopleList(here: people.filter(\.availability.isHere), away: people.filter { !$0.availability.isHere })
    }

    /// How many people are here now, without building the list: for a
    /// badge that is drawn far more often than the list is opened.
    public static func hereCount(presence: [BareJID: PresenceStore.ContactPresence], own: BareJID) -> Int {
        presence.count { $0.key != own && $0.value.availability.isHere }
    }

    private static func inOrder(_ a: Person, _ b: Person) -> Bool {
        if a.availability.peopleRank != b.availability.peopleRank {
            return a.availability.peopleRank < b.availability.peopleRank
        }
        let names = a.name.localizedStandardCompare(b.name)
        if names != .orderedSame { return names == .orderedAscending }
        return a.jid.description < b.jid.description
    }
}

private extension Availability {
    /// The web app's "around" rule: someone who is at their device, even
    /// if they asked not to be disturbed.
    var isHere: Bool {
        switch self {
        case .available, .chat, .doNotDisturb: true
        case .away, .extendedAway, .offline: false
        }
    }

    /// The web app's order in the list. It is not `Availability`'s own
    /// order, which ranks devices of one person and puts away first.
    var peopleRank: Int {
        switch self {
        case .available, .chat: 0
        case .doNotDisturb: 1
        case .away: 2
        case .extendedAway: 3
        case .offline: 4
        }
    }
}
