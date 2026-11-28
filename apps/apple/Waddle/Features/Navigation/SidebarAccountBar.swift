import SwiftUI
import WaddleKit

/// The signed-in user at the foot of the sidebar, with quick account actions.
struct SidebarAccountBar: View {
    @Environment(SessionCoordinator.self) private var session
    #if os(macOS)
    @Environment(AppState.self) private var app
    @State private var showsMenu = false
    let menuColorScheme: ColorScheme
    #else
    @Environment(NavigationModel.self) private var navigation
    #endif

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            accountButton
            SidebarSettingsButton()
        }
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.vertical, Theme.Spacing.s)
        .overlay(alignment: .top) {
            HuddleDivider()
        }
    }

    private var accountButton: some View {
        #if os(macOS)
        Button {
            showsMenu.toggle()
        } label: {
            accountLabel
        }
        .buttonStyle(.plain)
        .help("Account menu")
        .accessibilityLabel(Text("\(session.account.nick) — Account menu"))
        .accessibilityValue(Text(statusLine))
        .popover(isPresented: $showsMenu, arrowEdge: .top) {
            SidebarAccountMenu(preferences: app.preferences)
                .environment(\.colorScheme, menuColorScheme)
        }
        #else
        Button {
            navigation.sheet = .profile
        } label: {
            accountLabel
        }
        .buttonStyle(.plain)
        .help("Profile and status")
        .accessibilityHint(Text("Opens your profile and status"))
        #endif
    }

    private var accountLabel: some View {
        SidebarAccountLabel(
            jid: session.account.jid,
            name: session.account.nick,
            availability: AccountStatusLine.availability(
                connection: session.connection,
                chosen: session.status.availability
            ),
            statusLine: statusLine
        )
    }

    private var statusLine: String {
        AccountStatusLine.text(
            connection: session.connection,
            availability: session.status.availability,
            statusText: session.status.statusText
        )
    }
}

/// Avatar, name and status line of the signed-in user.
struct SidebarAccountLabel: View {
    let jid: BareJID
    let name: String
    let availability: Availability
    let statusLine: String
    var avatarSize = Theme.Size.rowAvatar

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            PeerPresenceAvatar(jid: jid, name: name, availability: availability, size: avatarSize)
            VStack(alignment: .leading, spacing: 0) {
                Text(name)
                    .huddleText(Theme.Typography.label)
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
                Text(statusLine)
                    .huddleText(Theme.Typography.timestamp)
                    .foregroundStyle(Theme.Colors.fgSoft)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(name), \(statusLine)"))
    }
}

/// Settings: the Settings scene on Mac, a sheet on iPad.
struct SidebarSettingsButton: View {
    #if os(iOS)
    @Environment(NavigationModel.self) private var navigation
    #endif

    var body: some View {
        #if os(macOS)
        SettingsLink { icon }
            .buttonStyle(.plain)
            .help("Settings")
            .accessibilityLabel(Text("Settings"))
        #else
        Button {
            navigation.sheet = .settings
        } label: {
            icon
        }
        .buttonStyle(.plain)
        .help("Settings")
        .accessibilityLabel(Text("Settings"))
        #endif
    }

    private var icon: some View {
        NavigationIconLabel(symbol: "slider.horizontal.3")
    }
}
