import SwiftUI
import WaddleKit

/// Signed-out vs signed-in routing and scene lifecycle.
struct RootView: View {
    @Environment(AppState.self) private var app
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Group {
            switch app.phase {
            case .launching:
                LaunchView()
            case .signedOut, .authorizing:
                SignInView()
            case .signedIn:
                if let session = app.session {
                    SignedInRoot(session: session)
                        .id(ObjectIdentifier(session))
                }
            }
        }
        #if os(macOS)
        .background(CallSurfaceWindow())
        #endif
        .preferredColorScheme(app.preferences.appearance.colorScheme)
        .animation(.default, value: app.phase)
        .task { await app.bootstrap() }
        .onChange(of: scenePhase) { _, phase in
            app.sceneActivityChanged(isActive: phase == .active)
        }
        .onChange(of: app.preferences.showsNotificationPreviews, initial: true) { _, shows in
            app.notifications.showsPreviews = shows
        }
    }
}

private struct LaunchView: View {
    var body: some View {
        VStack(spacing: Theme.Spacing.l) {
            WaddleBrandMark(size: 72)
            ProgressView()
                .accessibilityLabel(Text("Starting Waddle"))
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// The signed-in shell: phone tabs in compact width, a split view
/// everywhere else.
private struct SignedInRoot: View {
    @Environment(AppState.self) private var app
    let session: ActiveSession
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var body: some View {
        @Bindable var navigation = session.navigation
        // Above the shell, never over it. A safe-area inset on the shell
        // does not reach the navigation bars and columns inside it, so the
        // dock takes its own room and the shell starts below it.
        VStack(spacing: 0) {
            CallDock()
            shell
        }
        .background(Theme.Colors.bg)
        .environment(session.coordinator)
        .environment(session.navigation)
        .environment(\.callMediaControls, session.callMedia)
        .advertisingSelfMute(
            calls: session.coordinator.calls, media: session.callMedia,
            isOnline: session.coordinator.connection == .online
        )
        .sheet(item: $navigation.sheet) { route in
            SheetHost(route: route)
                .environment(app)
                .environment(session.coordinator)
                .environment(session.navigation)
                .environment(\.callMediaControls, session.callMedia)
        }
        .onChange(of: session.coordinator.unread.total, initial: true) { _, total in
            app.notifications.setBadge(total)
        }
        .onChange(of: app.preferences.sendsReadReceipts) { _, sends in
            session.coordinator.sendsReadReceipts = sends
        }
        .onChange(of: session.coordinator.connection == .online, initial: true) { _, isOnline in
            if isOnline, app.preferences.receivesPush {
                AppDelegate.registerForRemoteNotifications()
            }
        }
    }

    @ViewBuilder
    private var shell: some View {
        #if os(iOS)
        if sizeClass == .compact {
            PhoneShell()
        } else {
            SplitShell()
        }
        #else
        SplitShell()
        #endif
    }
}

/// Content of every sheet route.
private struct SheetHost: View {
    let route: SheetRoute

    var body: some View {
        content.carryingCallDock()
    }

    @ViewBuilder
    private var content: some View {
        switch route {
        case .newMessage:
            NewMessageSheet()
        case .newChannel:
            NewChannelSheet()
        case .profile:
            NavigationStack { ProfileView() }
        case .settings:
            NavigationStack { SettingsView() }
        case let .search(conversation):
            SearchSheet(conversation: conversation)
        case let .peer(profile):
            PeerProfileSheet(profile: profile)
        }
    }
}
