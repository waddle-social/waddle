import SwiftUI
import WaddleKit

/// iPad and Mac sidebar: spaces, channels, group chats and DMs.
struct Sidebar: View {
    @Environment(SessionCoordinator.self) private var session
    @Binding private var selection: ConversationID?
    @State private var query = ""
    @State private var spaceID: String?

    init(selection: Binding<ConversationID?>) {
        _selection = selection
    }

    var body: some View {
        let sections = currentSections
        List(selection: $selection) {
            if SpaceScope.showsPicker(spaceCount: session.directory.spaces.count) {
                Section {
                    SpacePickerMenu(spaces: session.directory.spaces, selection: $spaceID)
                }
            }
            ChannelListSection(channels: sections.channels, style: .sidebar, isSearching: isSearching)
            GroupChatListSection(groups: sections.groupDMs, style: .sidebar)
            DirectListSection(directs: sections.directs, style: .sidebar, isSearching: isSearching)
        }
        .listStyle(.sidebar)
        .searchable(text: $query, placement: .sidebar, prompt: "Search")
        .overlay {
            if isSearching && sections.isEmpty {
                ContentUnavailableView.search(text: query)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            SidebarAccountBar()
        }
        .navigationTitle("Waddle")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                NewMessageToolbarButton()
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
