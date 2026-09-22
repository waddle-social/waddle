import SwiftUI
import WaddleKit

/// The "Channels" section: loading, empty and populated states.
struct ChannelListSection: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let channels: [Channel]
    let style: ConversationLinkStyle
    let isSearching: Bool

    var body: some View {
        Section {
            if channels.isEmpty && !isSearching {
                emptyContent
            }
            ForEach(channels) { channel in
                ChannelNavigationRow(channel: channel, style: style)
            }
        } header: {
            NavigationSectionHeader(title: "Channels", addLabel: "Create a channel") {
                navigation.sheet = .newChannel
            }
        }
    }

    @ViewBuilder
    private var emptyContent: some View {
        if !session.directory.hasLoadedTopology {
            NavigationLoadingRow(title: "Loading channels…")
        } else {
            switch style {
            case .sidebar:
                Button {
                    navigation.sheet = .newChannel
                } label: {
                    Label("Create a channel", systemImage: "plus")
                }
                .foregroundStyle(.secondary)
            case .phone:
                NoChannelsView {
                    navigation.sheet = .newChannel
                }
                .listRowBackground(Color.clear)
            }
        }
    }
}

/// Empty state for a directory without channels.
struct NoChannelsView: View {
    let onCreate: () -> Void

    var body: some View {
        ContentUnavailableView {
            Label("No channels yet", systemImage: "number")
        } description: {
            Text("Channels keep conversations organized by topic.")
        } actions: {
            Button("Create a channel", action: onCreate)
                .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity)
    }
}

/// The "Group chats" section; hidden when there are none.
struct GroupChatListSection: View {
    @Environment(NavigationModel.self) private var navigation
    let groups: [Channel]
    let style: ConversationLinkStyle

    var body: some View {
        if !groups.isEmpty {
            Section {
                ForEach(groups) { group in
                    ChannelNavigationRow(channel: group, style: style)
                }
            } header: {
                NavigationSectionHeader(title: "Group chats", addLabel: "New group chat") {
                    navigation.sheet = .newMessage
                }
            }
        }
    }
}
