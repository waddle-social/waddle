import Foundation
import WaddleKit

extension FFIInbound {
    /// Nil when `from` is missing or unparseable: a presence without a
    /// sender cannot be attributed.
    static func wirePresence(_ presence: WaddlePresence) -> WirePresence? {
        guard let from = jid(presence.from) else { return nil }
        return WirePresence(
            from: from,
            kind: WirePresence.Kind(wire: presence.presenceType, errorCondition: presence.errorCondition),
            show: presence.show,
            status: presence.status,
            hats: presence.hats.map { Hat(uri: $0.uri, title: $0.title) },
            occupant: occupant(presence),
            idleSince: date(presence.idleSince),
            groupCall: presence.muji.map { muji in
                GroupCallMarker(
                    isPreparing: muji.preparing,
                    media: muji.active ? CallMedia(audio: muji.audio, video: muji.video) : nil,
                    isHandRaised: presence.handRaised,
                    isMuted: presence.muted)
            }
        )
    }

    /// XEP-0045 `<x xmlns='…muc#user'/>` payload, present when any of its
    /// parts crossed the boundary.
    static func occupant(_ presence: WaddlePresence) -> WirePresence.Occupant? {
        let hasPayload = presence.mucAffiliation != nil
            || presence.mucRole != nil
            || presence.mucJid != nil
            || !presence.mucStatusCodes.isEmpty
            || presence.mucDestroyed
        guard hasPayload else { return nil }
        return WirePresence.Occupant(
            affiliation: presence.mucAffiliation.map(roomAffiliation),
            role: presence.mucRole.map(roomRole),
            realJID: jid(presence.mucJid),
            statusCodes: Set(presence.mucStatusCodes.map(Int.init)),
            roomDestroyed: presence.mucDestroyed
        )
    }

    static func roomAffiliation(_ affiliation: WaddleMucAffiliation) -> RoomAffiliation {
        switch affiliation {
        case .owner: return .owner
        case .admin: return .admin
        case .member: return .member
        case .none: return .none
        case .outcast: return .outcast
        }
    }

    static func roomRole(_ role: WaddleMucRole) -> RoomRole {
        switch role {
        case .moderator: return .moderator
        case .participant: return .participant
        case .visitor: return .visitor
        case .none: return .none
        }
    }
}
