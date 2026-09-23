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
            Sidebar(selection: $navigation.selection)
                .navigationSplitViewColumnWidth(min: 240, ideal: 280, max: 360)
        } detail: {
            NavigationStack {
                detail
            }
        }
        .inspector(isPresented: inspectorBinding) {
            inspectorContent
                .inspectorColumnWidth(min: 300, ideal: 360, max: 480)
        }
        .sheet(isPresented: $navigation.isQuickSwitcherPresented) {
            QuickSwitcher()
        }
        .onChange(of: navigation.selection) { _, _ in
            // A thread belongs to the conversation it was opened in; details
            // and pins follow the new selection.
            if case .thread = navigation.inspector {
                navigation.inspector = nil
            }
        }
    }

    @ViewBuilder
    private var detail: some View {
        if let conversation = navigation.selection {
            ConversationScreen(conversation: conversation)
                .id(conversation)
        } else {
            EmptyStateView(
                title: "No conversation selected",
                message: "Pick a channel or direct message from the sidebar.",
                symbol: "bubble.left.and.bubble.right"
            )
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
