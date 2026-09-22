import Foundation
import WaddleKit

extension FFIInbound {
    static func topology(_ topology: WaddleTopology) -> Topology {
        Topology(spaces: topology.spaces.map(space), channels: topology.channels.compactMap(channel))
    }

    static func space(_ space: WaddleSpace) -> Space {
        Space(id: space.id, serviceJID: bareJID(space.serviceJid), name: space.name, summary: space.description)
    }

    /// `name` is already resolved by the core (bookmark name, else the
    /// catalog name, else the localpart).
    static func channel(_ channel: WaddleChannel) -> Channel? {
        guard let room = bareJID(channel.roomJid) else { return nil }
        return Channel(
            roomJID: room,
            name: channel.name,
            summary: channel.description,
            kind: Channel.Kind(wire: channel.channelType),
            position: Int(channel.position),
            spaceID: channel.spaceId.isEmpty ? nil : channel.spaceId,
            autojoin: channel.autojoin,
            isGroupDM: channel.isGroupDm
        )
    }

    static func roomMember(_ entry: WaddleRoomMemberEntry) -> RoomMember? {
        guard let member = bareJID(entry.jid) else { return nil }
        return RoomMember(jid: member, nick: entry.nick, affiliation: roomAffiliation(entry.affiliation))
    }

    /// One row per JID across the per-affiliation lists, keeping the
    /// highest affiliation (`RoomAffiliation` orders owner first) and
    /// first-seen order.
    static func mergedMembers(_ members: [RoomMember]) -> [RoomMember] {
        var order: [BareJID] = []
        var best: [BareJID: RoomMember] = [:]
        for member in members {
            guard let existing = best[member.jid] else {
                order.append(member.jid)
                best[member.jid] = member
                continue
            }
            let winner = member.affiliation < existing.affiliation ? member : existing
            let loser = winner == member ? existing : member
            best[member.jid] = RoomMember(jid: winner.jid, nick: winner.nick ?? loser.nick, affiliation: winner.affiliation)
        }
        return order.compactMap { best[$0] }
    }

    static func userSearchResult(_ entry: WaddleUserSearchEntry) -> UserSearchResult? {
        guard let user = bareJID(entry.jid) else { return nil }
        return UserSearchResult(jid: user, displayName: entry.displayName)
    }

    /// XEP-0430 row. `last_updated` is epoch seconds on the wire.
    static func inboxEntry(_ entry: WaddleInboxEntry) -> InboxEntry? {
        guard let partner = bareJID(entry.partner) else { return nil }
        return InboxEntry(
            partner: partner,
            kind: InboxEntry.Kind(wire: entry.kind),
            lastStanzaID: entry.lastStanzaId,
            lastUpdated: entry.lastUpdated,
            unread: Int(entry.unread),
            preview: entry.preview,
            threadID: entry.threadId
        )
    }

    static func uploadSlot(_ slot: WaddleUploadSlot) -> UploadSlot? {
        guard let put = url(slot.putUrl), let get = url(slot.getUrl) else { return nil }
        let headers = Dictionary(slot.putHeaders.map { ($0.name, $0.value) }, uniquingKeysWith: { first, _ in first })
        return UploadSlot(putURL: put, getURL: get, headers: headers)
    }

    /// XEP-0084 bytes. An avatar published only as an external URL has
    /// no bytes to show and is treated as absent.
    static func avatarImage(_ avatar: WaddleAvatar) -> AvatarImage? {
        guard !avatar.data.isEmpty else { return nil }
        return AvatarImage(data: avatar.data, mediaType: avatar.mimeType, width: 0, height: 0)
    }

    static func mood(_ mood: WaddleMood) -> UserMood {
        UserMood(value: mood.kind, text: mood.text)
    }
}
