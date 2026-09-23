import Foundation
import WaddleKit

/// The rows the sidebar and the phone home list render, already scoped to
/// a space and filtered by the search field.
struct NavigationSections: Equatable {
    var channels: [Channel]
    var groupDMs: [Channel]
    var directs: [DirectConversation]

    var isEmpty: Bool {
        channels.isEmpty && groupDMs.isEmpty && directs.isEmpty
    }

    /// `channels` are the space-scoped channels (`DirectoryStore.channels(in:)`);
    /// `directs` arrive most recent first and keep that order.
    static func build(
        channels: [Channel],
        groupDMs: [Channel],
        directs: [DirectConversation],
        query: String
    ) -> NavigationSections {
        NavigationSections(
            channels: sortedByPosition(channels).filter { matches($0, query: query) },
            groupDMs: groupDMs.filter { matches($0, query: query) },
            directs: directs.filter { matches($0, query: query) }
        )
    }

    static func sortedByPosition(_ channels: [Channel]) -> [Channel] {
        channels.sorted {
            ($0.position, $0.name.lowercased()) < ($1.position, $1.name.lowercased())
        }
    }

    private static func matches(_ channel: Channel, query: String) -> Bool {
        ConversationNameFilter.matches([channel.name, channel.roomJID.localpart ?? ""], query: query)
    }

    private static func matches(_ direct: DirectConversation, query: String) -> Bool {
        ConversationNameFilter.matches([direct.displayName, direct.peer.description], query: query)
    }
}

/// The space the channel list is scoped to.
enum SpaceScope {
    /// The picker shows only when there is more than one space.
    static func showsPicker(spaceCount: Int) -> Bool {
        spaceCount > 1
    }

    /// A remembered space that disappeared from the directory falls back
    /// to all spaces.
    static func resolved(_ selected: String?, in spaces: [Space]) -> String? {
        guard let selected, spaces.count > 1, spaces.contains(where: { $0.id == selected }) else { return nil }
        return selected
    }

    static func title(for selected: String?, in spaces: [Space]) -> String {
        guard let selected, let space = spaces.first(where: { $0.id == selected }) else { return "All spaces" }
        return space.name
    }
}
