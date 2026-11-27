import Foundation

/// The room links visible in a message body, shared by attributed text and
/// its row's VoiceOver actions.
public struct MessageRoomLinks {
    public struct Action: Identifiable {
        public var id: BareJID { room }
        public let room: BareJID
        public let name: String
        fileprivate let url: URL
    }

    public enum Destination: Equatable {
        case room(BareJID)
        case external
        case discard
    }

    public let actions: [Action]

    public init(blocks: [RichBlock], channels: [Channel]) {
        var seen: Set<BareJID> = []
        let rooms = blocks.flatMap(Self.linkedRooms).compactMap { room -> Channel? in
            guard seen.insert(room).inserted, RoomLinks.availableRoom(room, in: channels) != nil else { return nil }
            return channels.first { $0.roomJID == room }
        }
        let names = rooms.map(Self.name)
        let counts = Dictionary(grouping: names, by: { $0 })
        actions = zip(rooms, names).compactMap { channel, name in
            guard let url = Self.textURL(for: channel.roomJID) else { return nil }
            let label = counts[name, default: []].count > 1 ? "\(name), \(channel.roomJID)" : name
            return Action(room: channel.roomJID, name: label, url: url)
        }
    }

    public func url(for room: BareJID) -> URL? {
        actions.first { $0.room == room }?.url
    }

    public func destination(for url: URL) -> Destination {
        guard Self.isPrivateURL(url) else { return .external }
        guard let action = actions.first(where: { $0.url == url }) else { return .discard }
        return .room(action.room)
    }

    private static let scheme = "waddle-room"

    /// Reserve this scheme for links produced by the renderer.
    static func isPrivateURL(_ url: URL) -> Bool {
        url.scheme?.lowercased() == scheme
    }

    private static func textURL(for room: BareJID) -> URL? {
        var components = URLComponents()
        components.scheme = scheme
        components.queryItems = [URLQueryItem(name: "target", value: room.description)]
        return components.url
    }

    private static func linkedRooms(in block: RichBlock) -> [BareJID] {
        switch block {
        case let .paragraph(segments), let .quote(segments):
            return segments.flatMap { segment in
                segment.styles.compactMap {
                    if case let .roomLink(room) = $0 { return room }
                    return nil
                }
            }
        case .code:
            return []
        }
    }

    private static func name(_ channel: Channel) -> String {
        channel.name.isEmpty ? (channel.roomJID.localpart ?? channel.roomJID.domain) : channel.name
    }
}
