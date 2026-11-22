import Foundation
@testable import Waddle
@testable import WaddleKit

/// Occupants for the room details and members goldens.
extension SampleSession {
    /// Our own occupant in `room`.
    func join(_ room: BareJID, as affiliation: RoomAffiliation, role: RoomRole) {
        occupy(room, nick: Self.ana.nick, jid: Self.ana.jid, affiliation: affiliation, role: role, isSelf: true)
    }

    func occupy(
        _ room: BareJID, nick: String, jid: BareJID, affiliation: RoomAffiliation,
        role: RoomRole = .participant, show: String? = nil, hats: [Hat] = [], isSelf: Bool = false
    ) {
        _ = coordinator.presence.apply(WirePresence(
            from: room.with(resource: nick)!,
            kind: .available,
            show: show,
            status: nil,
            hats: hats,
            occupant: WirePresence.Occupant(
                affiliation: affiliation,
                role: role,
                realJID: jid.with(resource: "phone"),
                statusCodes: isSelf ? [110] : []
            ),
            idleSince: nil
        ))
    }
}
