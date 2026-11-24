import SwiftUI
import WaddleKit

/// iPad and Mac: sidebar, conversation, and an inspector for threads,
/// details and pins. A call in session takes a stage beside its conversation.
struct SplitShell: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(CallStageModel.self) private var callStage
    @CallPlatform private var callPlatform
    @State private var columnVisibility = NavigationSplitViewVisibility.all

    /// A full-screen call has the window to itself: no sidebar, toolbar or inspector.
    private var isCallFullScreen: Bool {
        callStage.surface(of: session.calls.state, open: navigation.selection, platform: callPlatform) == .fullscreen
    }

    var body: some View {
        @Bindable var navigation = navigation
        let isCallFullScreen = isCallFullScreen
        NavigationSplitView(columnVisibility: columnBinding(hidingSidebar: isCallFullScreen)) {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 240, ideal: 280, max: 360)
        } detail: {
            NavigationStack {
                CallSplitHost { detail }
                    .background(Theme.Colors.bg)
                    #if os(macOS)
                    // The title bar continues the page ground instead of the system grey.
                    .toolbarBackground(Theme.Colors.bg, for: .windowToolbar)
                    .toolbarBackground(.visible, for: .windowToolbar)
                    .toolbar(isCallFullScreen ? .hidden : .automatic, for: .windowToolbar)
                    #else
                    .toolbar(isCallFullScreen ? .hidden : .automatic, for: .navigationBar)
                    #endif
            }
        }
        .tint(Theme.Colors.action)
        .callFullScreen(isCallFullScreen) {
            // The window left full screen by its own button: the call steps down with it.
            guard let sid = session.calls.state.sid else { return }
            callStage.update(sid) { $0.size = $0.size.afterFullscreenExit }
        }
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
            case .feed:
                FeedView()
            case .events:
                EventsView()
            case .people:
                PeopleView()
            case .admin:
                AdminView()
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
            .environment(\.isInInspector, true)
        }
    }

    /// The person's own choice of columns comes back when the call steps down.
    private func columnBinding(hidingSidebar: Bool) -> Binding<NavigationSplitViewVisibility> {
        Binding(
            get: { hidingSidebar ? .detailOnly : columnVisibility },
            set: { if !hidingSidebar { columnVisibility = $0 } }
        )
    }

    private var inspectorBinding: Binding<Bool> {
        Binding(
            get: { navigation.inspector != nil && navigation.selection != nil && !isCallFullScreen },
            set: { if !$0 { navigation.inspector = nil } }
        )
    }
}
