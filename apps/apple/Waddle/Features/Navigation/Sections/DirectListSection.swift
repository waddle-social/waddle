import SwiftUI
import WaddleKit

/// The "Direct messages" section. On the phone's Home it shows the most
/// recent few and links to the DMs tab for the rest.
struct DirectListSection: View {
    @Environment(NavigationModel.self) private var navigation
    let directs: [DirectConversation]
    let style: ConversationLinkStyle
    let isSearching: Bool
    /// Rows shown before "Show all"; nil shows every row.
    var limit: Int?

    var body: some View {
        NavigationSectionHeader(title: "Direct messages", addLabel: "New message") {
            navigation.sheet = .newMessage
        }
        if directs.isEmpty && !isSearching {
            NavigationEmptyAction(title: "New message", symbol: "square.and.pencil") {
                navigation.sheet = .newMessage
            }
        }
        ForEach(visible) { direct in
            DirectNavigationRow(direct: direct, style: style, isCompact: true)
        }
        if hiddenCount > 0 {
            NavigationEmptyAction(title: "Show all \(directs.count)", symbol: "ellipsis") {
                navigation.tab = .directMessages
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
