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

    @ObservationIgnored private var resources: [BareJID: [String: ContactPresence]] = [:]
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

    public func markLeft(_ room: BareJID) {
        joinedRooms.remove(room)
        occupants[room] = nil
    }

    public func clear() {
        contacts.removeAll()
        occupants.removeAll()
        joinedRooms.removeAll()
        resources.removeAll()
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
            occupants[room, default: [:]][nick] = occupant
            if isSelf, !joinedRooms.contains(room) {
                joinedRooms.insert(room)
                return .joined(room: room)
            }
            return .occupant(room: room)
        case .unavailable:
            occupants[room]?[nick] = nil
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
        let best = resources[bare]?.values.min { $0.availability < $1.availability }
        contacts[bare] = best
        return .contact
    }
}
