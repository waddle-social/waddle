import Foundation
import Observation

/// A room occupant as its presence describes it.
public struct Occupant: Hashable, Sendable, Identifiable {
    public var id: String { nick }
    public let nick: String
    public let availability: Availability
    public let status: String?
    public let affiliation: RoomAffiliation
    public let role: RoomRole
    public let realJID: BareJID?
    public let hats: [Hat]

    public init(
        nick: String,
        availability: Availability,
        status: String?,
        affiliation: RoomAffiliation,
        role: RoomRole,
        realJID: BareJID?,
        hats: [Hat]
    ) {
        self.nick = nick
        self.availability = availability
        self.status = status
        self.affiliation = affiliation
        self.role = role
        self.realJID = realJID
        self.hats = hats
    }

    /// Extension bots wear the server-assigned bot hat; they refuse DMs.
    public var isBot: Bool {
        hats.contains { $0.uri == Hat.botURI }
    }

    /// XEP-0045 §5: moderators and admins/owners may moderate messages.
    public var canModerate: Bool {
        role == .moderator || affiliation == .owner || affiliation == .admin
    }
}

/// Contact availability (1:1) and room occupants (MUC).
@MainActor
@Observable
public final class PresenceStore {
    /// Bare JID → best-known availability across resources.
    public private(set) var contacts: [BareJID: ContactPresence] = [:]
    /// Room → nick → occupant.
    public private(set) var occupants: [BareJID: [String: Occupant]] = [:]
    /// Rooms whose self-presence (status 110) has arrived.
    public private(set) var joinedRooms: Set<BareJID> = []
    /// Room → the nick our self-presence (status 110) carries: the one the
    /// room actually gave us, which may differ from the configured one
    /// (status 210).
    public private(set) var selfNicks: [BareJID: String] = [:]
    /// Bare JID → what they last said they are doing (XEP-0108), heard on
    /// this stream or looked up. A call's activity is forgotten when they
    /// go offline, and everything on a new stream: a call that ended while
    /// the stream was down sends nothing on the next one.
    public private(set) var activities: [BareJID: UserActivity] = [:]

    @ObservationIgnored private var resources: [BareJID: [String: ContactPresence]] = [:]
    /// Counts changes to `activities`, so a lookup can tell whether one
    /// came after it began.
    @ObservationIgnored private var activityClock = 0
    @ObservationIgnored private var activityChangedAt: [BareJID: Int] = [:]
    @ObservationIgnored private var activitiesClearedAt = 0
    @ObservationIgnored private let roomJIDs: @MainActor (BareJID) -> Bool

    /// `isRoom` tells a MUC occupant presence from a contact presence.
    public init(isRoom: @escaping @MainActor (BareJID) -> Bool) {
        self.roomJIDs = isRoom
    }

    public struct ContactPresence: Hashable, Sendable {
        public let availability: Availability
        public let status: String?
        public let idleSince: Date?

        public init(availability: Availability, status: String?, idleSince: Date?) {
            self.availability = availability
            self.status = status
            self.idleSince = idleSince
        }
    }

    public enum Update: Equatable, Sendable {
        case contact
        case occupant(room: BareJID)
        /// Our own join completed.
        case joined(room: BareJID)
        /// We left or were removed.
        case left(room: BareJID)
        /// The room's owner deleted it, which removed us (XEP-0045 §10.9).
        case destroyed(room: BareJID)
        /// The room rejected the join.
        case joinFailed(room: BareJID, condition: String?)
        case ignored
    }

    @discardableResult
    public func apply(_ presence: WirePresence) -> Update {
        let bare = presence.from.bare
        if presence.occupant != nil || roomJIDs(bare) {
            return applyOccupant(presence, room: bare)
        }
        return applyContact(presence, bare: bare)
    }

    public func occupant(named nick: String, in room: BareJID) -> Occupant? {
        occupants[room]?[nick]
    }

    public func availability(of jid: BareJID) -> Availability {
        contacts[jid]?.availability ?? .offline
    }

    /// Whether someone is in a call (ADR-010): their activity shows one and
    /// they are online. A device that goes away without ending its call
    /// leaves the mark on their account.
    public func isInCall(_ jid: BareJID) -> Bool {
        availability(of: jid).isOnline && activities[jid]?.showsACall == true
    }

    /// Where `activities` stands for `jid`, for `activityLookedUp`.
    func activityStamp(of jid: BareJID) -> Int {
        max(activityChangedAt[jid] ?? 0, activitiesClearedAt)
    }

    /// What a lookup that began at `stamp` read. Anything heard since is
    /// newer and stands. A call read for someone offline is one their
    /// device left behind, so it is not kept.
    func activityLookedUp(_ activity: UserActivity?, of jid: BareJID, since stamp: Int) {
        guard activityStamp(of: jid) == stamp else { return }
        let isLeftBehind = activity?.showsACall == true && !availability(of: jid).isOnline
        activityChanged(isLeftBehind ? nil : activity, of: jid)
    }

    /// What they are doing now, nil when they stopped saying: from their
    /// notification, a lookup or their going offline.
    func activityChanged(_ activity: UserActivity?, of jid: BareJID) {
        activityClock += 1
        activityChangedAt[jid] = activityClock
        if activities[jid] != activity {
            activities[jid] = activity
        }
    }

    public func markLeft(_ room: BareJID) {
        joinedRooms.remove(room)
        occupants[room] = nil
        selfNicks[room] = nil
    }

    public func clear() {
        contacts.removeAll()
        occupants.removeAll()
        joinedRooms.removeAll()
        selfNicks.removeAll()
        resources.removeAll()
        activities.removeAll()
        activityClock += 1
        activitiesClearedAt = activityClock
        activityChangedAt.removeAll()
    }

    private func applyOccupant(_ presence: WirePresence, room: BareJID) -> Update {
        if case let .error(condition) = presence.kind {
            return .joinFailed(room: room, condition: condition)
        }
        guard let nick = presence.from.resource else { return .ignored }
        let isSelf = presence.occupant?.isSelf == true
        switch presence.kind {
        case .available:
            let occupant = Occupant(
                nick: nick,
                availability: presence.availability,
                status: presence.status,
                affiliation: presence.occupant?.affiliation ?? .none,
                role: presence.occupant?.role ?? .participant,
                realJID: presence.occupant?.realJID?.bare,
                hats: presence.hats
            )
            if occupants[room]?[nick] != occupant {
                occupants[room, default: [:]][nick] = occupant
            }
            if isSelf, selfNicks[room] != nick {
                selfNicks[room] = nick
            }
            if isSelf, !joinedRooms.contains(room) {
                joinedRooms.insert(room)
                return .joined(room: room)
            }
            return .occupant(room: room)
        case .unavailable:
            if occupants[room]?[nick] != nil {
                occupants[room]?[nick] = nil
            }
            // A destruction is sent only to the occupant it removes, so it
            // is our own departure even without status 110. Only a room we
            // know or are in can say it is gone.
            if presence.occupant?.roomDestroyed == true, roomJIDs(room) || joinedRooms.contains(room) {
                markLeft(room)
                return .destroyed(room: room)
            }
            if isSelf {
                markLeft(room)
                return .left(room: room)
            }
            return .occupant(room: room)
        case .error, .other:
            return .ignored
        }
    }

    private func applyContact(_ presence: WirePresence, bare: BareJID) -> Update {
        let resource = presence.from.resource ?? ""
        switch presence.kind {
        case .available:
            resources[bare, default: [:]][resource] = ContactPresence(
                availability: presence.availability,
                status: presence.status,
                idleSince: presence.idleSince
            )
        case .unavailable:
            resources[bare]?[resource] = nil
        case .error, .other:
            return .ignored
        }
        // The most available resource represents the contact.
        let best = resources[bare]?.values.min {
            $0.availability.mostAvailableRank < $1.availability.mostAvailableRank
        }
        if contacts[bare] != best {
            contacts[bare] = best
        }
        // Offline, nobody is in a call; one their node still holds was
        // left behind and must not show when they return (ADR-010). Also
        // marks a lookup under way as older than this.
        if best == nil {
            activityChanged(activities[bare].flatMap { $0.showsACall ? nil : $0 }, of: bare)
        }
        return .contact
    }
}
