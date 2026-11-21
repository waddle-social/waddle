import SwiftUI
import WaddleKit

/// iPhone: Home, DMs, Inbox, Feed and You tabs, each with its own stack.
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
            .tabItem { Label("Home", systemImage: "house") }
            .badge(roomUnread)
            .tag(PhoneTab.home)

            NavigationStack(path: $navigation.directPath) {
                DirectMessagesList()
                    .routeDestinations()
            }
            .tabItem { Label("DMs", systemImage: "bubble.left.and.bubble.right") }
            .badge(session.unread.totalDirect)
            .tag(PhoneTab.directMessages)

            NavigationStack(path: $navigation.inboxPath) {
                InboxView(style: .phone)
                    .routeDestinations()
            }
            .tabItem { Label("Inbox", systemImage: "tray") }
            .badge(session.unreadOverviewRoomCount)
            .tag(PhoneTab.inbox)

            NavigationStack(path: $navigation.feedPath) {
                FeedView(opensEvents: true)
                    .navigationDestination(for: FeedRoute.self) { route in
                        switch route {
                        case .events: EventsView()
                        }
                    }
            }
            .tabItem { Label("Feed", systemImage: "newspaper") }
            .tag(PhoneTab.feed)

            NavigationStack(path: $navigation.youPath) {
                ProfileView(opensAdmin: true)
                    .navigationDestination(for: YouRoute.self) { route in
                        switch route {
                        case .admin: AdminView()
                        }
                    }
            }
            .tabItem { Label("You", systemImage: "person.crop.circle") }
            .tag(PhoneTab.you)
        }
        .tint(Theme.Colors.action)
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
            case .people:
                PeopleView()
            }
        }
    }
}
