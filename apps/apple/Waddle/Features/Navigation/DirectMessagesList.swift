import SwiftUI
import WaddleKit

/// iPhone DMs tab: every 1:1 conversation, most recent first.
struct DirectMessagesList: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @State private var query = ""

    init() {}

    var body: some View {
        let directs = filteredDirects
        List {
            ForEach(directs) { direct in
                DirectNavigationRow(direct: direct, style: .phone)
            }
        }
        .listStyle(.plain)
        .navigationTitle("Direct messages")
        .searchable(text: $query, prompt: "Search people")
        .overlay {
            if directs.isEmpty {
                emptyState
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                NewMessageToolbarButton()
            }
        }
    }

    @ViewBuilder
    private var emptyState: some View {
        if isSearching {
            ContentUnavailableView.search(text: query)
        } else {
            ContentUnavailableView {
                Label("No messages yet", systemImage: "bubble.left.and.bubble.right")
            } description: {
                Text("Start a conversation with anyone on your server.")
            } actions: {
                Button("New message") {
                    navigation.sheet = .newMessage
                }
                .buttonStyle(.borderedProminent)
            }
        }
    }

    private var isSearching: Bool {
        !ConversationNameFilter.normalized(query).isEmpty
    }

    private var filteredDirects: [DirectConversation] {
        NavigationSections.build(
            channels: [],
            groupDMs: [],
            directs: session.directory.directConversations,
            query: query
        ).directs
    }
}
