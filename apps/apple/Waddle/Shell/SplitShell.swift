import SwiftUI
import WaddleKit

/// iPad and Mac: sidebar, conversation, and an inspector for threads,
/// details and pins.
struct SplitShell: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @State private var columnVisibility = NavigationSplitViewVisibility.all

    var body: some View {
        @Bindable var navigation = navigation
        NavigationSplitView(columnVisibility: $columnVisibility) {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 240, ideal: 280, max: 360)
        } detail: {
            NavigationStack {
                detail
                    .background(Theme.Colors.bg)
                    #if os(macOS)
                    // The title bar continues the page ground instead of the system grey.
                    .toolbarBackground(Theme.Colors.bg, for: .windowToolbar)
                    .toolbarBackground(.visible, for: .windowToolbar)
                    #endif
            }
        }
        .tint(Theme.Colors.action)
        .inspector(isPresented: inspectorBinding) {
            inspectorContent
                .inspectorColumnWidth(min: 300, ideal: 360, max: 480)
        }
        .sheet(isPresented: $navigation.isQuickSwitcherPresented) {
            QuickSwitcher()
                .carryingCallDock()
        }
    }

    @ViewBuilder
    private var detail: some View {
        if let conversation = navigation.selection {
            ConversationScreen(conversation: conversation)
                .id(conversation)
        } else {
            switch navigation.splitSection {
            case .home:
                EmptyStateView(
                    title: "Pick a conversation",
                    message: "Choose a room or a direct message to start reading.",
                    symbol: "bubble.left.and.bubble.right"
                )
            case .inbox:
                InboxView(style: .sidebar)
            }
        }
    }

    @ViewBuilder
    private var inspectorContent: some View {
        if let conversation = navigation.selection, let route = navigation.inspector {
            NavigationStack {
                switch route {
                case .details:
                    ConversationDetailsView(conversation: conversation)
                case let .thread(rootID):
                    ThreadScreen(conversation: conversation, rootID: rootID)
                case .pins:
                    PinnedMessagesView(conversation: conversation)
                }
            }
        }
    }

    private var inspectorBinding: Binding<Bool> {
        Binding(
            get: { navigation.inspector != nil && navigation.selection != nil },
            set: { if !$0 { navigation.inspector = nil } }
        )
    }
}
