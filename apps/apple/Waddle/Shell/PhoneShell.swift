import SwiftUI
import WaddleKit

/// iPhone: Home, DMs, Activity and You tabs, each with its own stack.
struct PhoneShell: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation

    var body: some View {
        @Bindable var navigation = navigation
        TabView(selection: $navigation.tab) {
            NavigationStack(path: $navigation.homePath) {
                HomeList()
                    .routeDestinations()
            }
            .tabItem { Label("Home", systemImage: "bubble.left.and.bubble.right") }
            .badge(roomUnread)
            .tag(PhoneTab.home)

            NavigationStack(path: $navigation.directPath) {
                DirectMessagesList()
                    .routeDestinations()
            }
            .tabItem { Label("DMs", systemImage: "person.2") }
            .badge(session.unread.totalDirect)
            .tag(PhoneTab.directMessages)

            NavigationStack(path: $navigation.activityPath) {
                ActivityView()
                    .routeDestinations()
            }
            .tabItem { Label("Activity", systemImage: "at") }
            .badge(session.unread.mentions.count)
            .tag(PhoneTab.activity)

            NavigationStack {
                ProfileView()
            }
            .tabItem { Label("You", systemImage: "person.crop.circle") }
            .tag(PhoneTab.you)
        }
    }

    private var roomUnread: Int {
        session.unread.counts.filter { $0.key.isRoom }.values.reduce(0, +)
    }
}

extension View {
    /// Destinations for every `Route` a stack can push.
    func routeDestinations() -> some View {
        navigationDestination(for: Route.self) { route in
            switch route {
            case let .conversation(conversation):
                ConversationScreen(conversation: conversation)
            case let .thread(conversation, rootID):
                ThreadScreen(conversation: conversation, rootID: rootID)
            case let .details(conversation):
                ConversationDetailsView(conversation: conversation)
            }
        }
    }
}
