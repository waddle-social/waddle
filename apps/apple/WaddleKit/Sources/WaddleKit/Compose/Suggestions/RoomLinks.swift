import Foundation
/// Room suggestions and join URIs, matching chat/src/lib/room-links.ts.
public enum RoomLinks {
    public struct Candidate: Hashable, Sendable {
        public let room: BareJID
        public let label: String
        var showsAddress = false
    }

    /// The channel directory is the sidebar's list. A known room may be
    /// federated or opened on demand; group DMs and left rooms are excluded.
    public static func candidates(from channels: [Channel]) -> [Candidate] {
        var rooms: [Candidate] = []
        var indexes: [BareJID: Int] = [:]
        for channel in channels where isAvailable(channel) {
            let room = channel.roomJID
            guard let localpart = joinableLocalpart(of: room) else { continue }
            let candidate = Candidate(room: room, label: "#" + (channel.name.isEmpty ? localpart : channel.name))
            if let index = indexes[room] {
                rooms[index] = candidate
            } else {
                indexes[room] = rooms.count
                rooms.append(candidate)
            }
        }
        return rooms
    }

    public static func search(_ candidates: [Candidate], for query: String) -> [Candidate] {
        let query = query.lowercased()
        let limit = ComposerSuggestion.maximumRows
        if query.isEmpty { return Array(candidates.prefix(limit)).disambiguatingRoomLabels() }

        var exact: [Candidate] = []
        var partial: [Candidate] = []
        for room in candidates {
            let name = room.label.dropFirst().lowercased()
            let localpart = room.room.localpart?.lowercased() ?? ""
            if name.utf8.elementsEqual(query.utf8) || localpart.utf8.elementsEqual(query.utf8) {
                exact.append(room)
            } else if name.range(of: query, options: .literal) != nil || localpart.range(of: query, options: .literal) != nil {
                partial.append(room)
            }
        }
        return Array((exact + partial).prefix(limit)).disambiguatingRoomLabels()
    }

    /// Web activates links only for a known channel at that exact address.
    public static func availableRoom(_ room: BareJID, in channels: [Channel]) -> BareJID? {
        channels.contains { $0.roomJID == room && isAvailable($0) } ? room : nil
    }

    private static func isAvailable(_ channel: Channel) -> Bool {
        !channel.isGroupDM && !channel.hasLeft
    }

    // Exactly the ASCII characters encodeURIComponent leaves unescaped.
    private static let uriLocalpart = CharacterSet(charactersIn:
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()"
    )
    public static func joinURI(for room: BareJID) -> String? {
        guard let localpart = joinableLocalpart(of: room),
              let encoded = localpart.addingPercentEncoding(withAllowedCharacters: uriLocalpart)
        else { return nil }
        return "xmpp:\(encoded)@\(room.domain)?join"
    }

    public static func room(fromJoinURI uri: String) -> BareJID? {
        let pattern = #/(?i)xmpp:((?:[a-z0-9._~!$()*+,;=-]|%[0-9a-f]{2})+)@([a-z0-9.-]+|\[[0-9a-f:.]+\])\?join/#
        guard uri.utf8.allSatisfy({ $0 < 128 }),
              let match = uri.wholeMatch(of: pattern),
              let decoded = match.1.removingPercentEncoding,
              isURILocalpart(decoded),
              let room = BareJID(localpart: decoded, domain: String(match.2)),
              room.hasURIHost
        else { return nil }
        return room
    }

    public static func room(from reference: Reference) -> BareJID? {
        guard reference.kind == .data, reference.begin >= 0, reference.end > reference.begin else { return nil }
        return room(fromJoinURI: reference.uri)
    }

    private static func joinableLocalpart(of room: BareJID) -> String? {
        guard let localpart = room.localpart, isURILocalpart(localpart), room.hasURIHost else { return nil }
        return localpart
    }

    private static func isURILocalpart(_ localpart: String) -> Bool {
        // A join URI also excludes control characters and the byte-order mark.
        !localpart.unicodeScalars.contains { $0.properties.generalCategory == .control || $0 == "\u{FEFF}" }
    }
}
