import SwiftUI
import WaddleKit

/// iPhone Home tab: spaces, channels, group chats and recent DMs.
struct HomeList: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @State private var query = ""
    @State private var spaceID: String?

    /// Recent DMs shown before "Show all" links to the DMs tab.
    private let directLimit = 5

    init() {}

    var body: some View {
        let sections = currentSections
        List {
            if SpaceScope.showsPicker(spaceCount: session.directory.spaces.count) {
                Section {
                    SpacePickerMenu(spaces: session.directory.spaces, selection: $spaceID)
                }
            }
            ChannelListSection(channels: sections.channels, style: .phone, isSearching: isSearching)
            GroupChatListSection(groups: sections.groupDMs, style: .phone)
            DirectListSection(directs: sections.directs, style: .phone, isSearching: isSearching, limit: directLimit)
        }
        .homeListStyle()
        .navigationTitle("Home")
        .searchable(text: $query, prompt: "Search conversations")
        .overlay {
            if isSearching && sections.isEmpty {
                ContentUnavailableView.search(text: query)
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                NewMessageToolbarButton()
            }
            ToolbarItem(placement: .secondaryAction) {
                Button {
                    navigation.sheet = .newChannel
                } label: {
                    Label("New channel", systemImage: "number")
                }
            }
        }
    }

    private var isSearching: Bool {
        !ConversationNameFilter.normalized(query).isEmpty
    }

    private var currentSections: NavigationSections {
        let directory = session.directory
        return NavigationSections.build(
            channels: directory.channels(in: SpaceScope.resolved(spaceID, in: directory.spaces)),
            groupDMs: directory.groupDMs,
            directs: directory.directConversations,
            query: query
        )
    }
}

private extension View {
    func homeListStyle() -> some View {
        #if os(iOS)
        return self.listStyle(.insetGrouped)
        #else
        return self.listStyle(.inset)
        #endif
    }
}
