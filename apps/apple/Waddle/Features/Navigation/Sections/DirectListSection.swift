import SwiftUI
import WaddleKit

/// The "Direct messages" section. On the phone home list it shows the most
/// recent few and links to the DMs tab for the rest.
struct DirectListSection: View {
    @Environment(NavigationModel.self) private var navigation
    let directs: [DirectConversation]
    let style: ConversationLinkStyle
    let isSearching: Bool
    /// Rows shown before "Show all"; nil shows every row.
    var limit: Int?

    var body: some View {
        Section {
            if directs.isEmpty && !isSearching {
                Button {
                    navigation.sheet = .newMessage
                } label: {
                    Label("New message", systemImage: "square.and.pencil")
                }
                .foregroundStyle(.secondary)
            }
            ForEach(visible) { direct in
                DirectNavigationRow(direct: direct, style: style)
            }
            if hiddenCount > 0 {
                Button {
                    navigation.tab = .directMessages
                } label: {
                    Text("Show all \(directs.count)")
                        .font(.subheadline)
                }
            }
        } header: {
            NavigationSectionHeader(title: "Direct messages", addLabel: "New message") {
                navigation.sheet = .newMessage
            }
        }
    }

    private var visible: [DirectConversation] {
        guard let limit, !isSearching else { return directs }
        return Array(directs.prefix(limit))
    }

    private var hiddenCount: Int {
        directs.count - visible.count
    }
}
