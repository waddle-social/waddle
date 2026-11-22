import Foundation
@testable import Waddle
@testable import WaddleKit

/// Occupants for the room details and members goldens.
extension SampleSession {
    /// Our own occupant in `room` (XEP-0045 status 110).
    func join(_ room: BareJID, as affiliation: RoomAffiliation, role: RoomRole) {
        enter(room, nick: Self.ana.nick, as: WirePresence.Occupant(
            affiliation: affiliation, role: role, realJID: Self.ana.jid.with(resource: "phone"), statusCodes: [110]
        ))
    }

    /// Someone else in `room`, with their show and hats if any.
    func occupy(
        _ room: BareJID, nick: String, jid: BareJID, affiliation: RoomAffiliation,
        role: RoomRole = .participant, show: String? = nil, hats: [Hat] = []
    ) {
        enter(room, nick: nick, as: WirePresence.Occupant(
            affiliation: affiliation, role: role, realJID: jid.with(resource: "phone"), statusCodes: []
        ), show: show, hats: hats)
    }

    private func enter(_ room: BareJID, nick: String, as occupant: WirePresence.Occupant, show: String? = nil, hats: [Hat] = []) {
        _ = coordinator.presence.apply(WirePresence(
            from: room.with(resource: nick)!,
            kind: .available,
            show: show,
            status: nil,
            hats: hats,
            occupant: occupant,
            idleSince: nil
        ))
    }
}
