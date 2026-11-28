import SwiftUI
import WaddleKit

/// The "Rooms" section: loading, empty and populated states.
struct ChannelListSection: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let channels: [Channel]
    let style: ConversationLinkStyle
    let isSearching: Bool

    var body: some View {
        NavigationSectionHeader(
            title: "Rooms",
            addLabel: "New room",
            action: { navigation.sheet = .newChannel },
            accessory: {
                if style == .sidebar && !isSearching {
                    BrowseRoomsSidebarLink()
                }
            }
        )
        if channels.isEmpty && !isSearching {
            emptyContent
        }
        ForEach(channels) { channel in
            ChannelNavigationRow(channel: channel, style: style)
        }
        if !session.directory.leftRooms.isEmpty && !isSearching {
            NavigationEmptyAction(title: "Rooms you left", symbol: "arrow.uturn.backward") {
                navigation.sheet = .leftRooms
            }
        }
    }

    @ViewBuilder
    private var emptyContent: some View {
        if !session.directory.hasLoadedTopology {
            NavigationLoadingRow(title: "Loading rooms…")
        } else {
            switch style {
            case .sidebar:
                NavigationEmptyAction(title: "New room", symbol: "plus") {
                    navigation.sheet = .newChannel
                }
            case .phone:
                NoRoomsView {
                    navigation.sheet = .newChannel
                }
                .navigationListRow()
            }
        }
    }
}

/// Empty state for a community without rooms.
struct NoRoomsView: View {
    let onCreate: () -> Void

    var body: some View {
        VStack(spacing: Theme.Spacing.m) {
            EmptyStateView(
                title: "No rooms yet",
                message: "Rooms keep conversations together by topic. Start the first one.",
                symbol: "number"
            )
            Button("New room", action: onCreate)
                .buttonStyle(.huddle(.primary))
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.l)
    }
}

/// The "Group chats" section; hidden when there are none.
struct GroupChatListSection: View {
    @Environment(NavigationModel.self) private var navigation
    let groups: [Channel]
    let style: ConversationLinkStyle

    var body: some View {
        if !groups.isEmpty {
            NavigationSectionHeader(title: "Group chats", addLabel: "New group chat") {
                navigation.sheet = .newMessage
            }
            ForEach(groups) { group in
                ChannelNavigationRow(channel: group, style: style)
            }
        }
    }
}
