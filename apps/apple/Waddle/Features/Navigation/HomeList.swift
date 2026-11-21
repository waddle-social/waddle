import SwiftUI
import WaddleKit

/// iPhone Home tab: the way into People, then rooms, group chats and
/// recent direct messages.
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
                SpacePickerMenu(spaces: session.directory.spaces, selection: $spaceID)
                    .padding(.horizontal, Theme.Spacing.m)
                    .navigationListRow(top: Theme.Spacing.s)
            }
            if !isSearching {
                PeopleHomeRow()
            }
            ChannelListSection(channels: sections.channels, style: .phone, isSearching: isSearching)
            GroupChatListSection(groups: sections.groupDMs, style: .phone)
            DirectListSection(directs: sections.directs, style: .phone, isSearching: isSearching, limit: directLimit)
        }
        .navigationListStyle()
        .navigationTitle(title)
        .searchable(text: $query, prompt: "Search rooms and people")
        .overlay {
            if isSearching && sections.isEmpty {
                NoMatchesView(query: query)
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
                    Label("New room", systemImage: "number")
                }
            }
        }
    }

    /// The community's name when there is one space, as the sidebar shows.
    private var title: String {
        let spaces = session.directory.spaces
        return spaces.count == 1 ? spaces[0].name : "Home"
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

extension View {
    /// A navigation list on the page ground: plain rows, no system
    /// grouping or separators.
    func navigationListStyle(ground: Color = Theme.Colors.bg) -> some View {
        listStyle(.plain)
            .scrollContentBackground(.hidden)
            .background(ground)
            .environment(\.defaultMinListRowHeight, 0)
    }
}
