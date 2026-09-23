import SwiftUI
import WaddleKit

/// The signed-in user at the foot of the sidebar, with settings.
struct SidebarAccountBar: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            Button {
                navigation.sheet = .profile
            } label: {
                SidebarAccountLabel(
                    jid: session.account.jid,
                    name: session.account.nick,
                    availability: AccountStatusLine.availability(
                        connection: session.connection,
                        chosen: session.status.availability
                    ),
                    statusLine: AccountStatusLine.text(
                        connection: session.connection,
                        availability: session.status.availability,
                        statusText: session.status.statusText
                    )
                )
            }
            .buttonStyle(.plain)
            .help("Profile and status")
            .accessibilityHint(Text("Opens your profile and status"))

            SidebarSettingsButton()
        }
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.vertical, Theme.Spacing.s)
        .background(.bar)
        .overlay(alignment: .top) {
            Divider()
        }
    }
}

/// Avatar, name and status line of the signed-in user.
struct SidebarAccountLabel: View {
    let jid: BareJID
    let name: String
    let availability: Availability
    let statusLine: String

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            PeerPresenceAvatar(jid: jid, name: name, availability: availability, size: Theme.Size.rowAvatar)
            VStack(alignment: .leading, spacing: 0) {
                Text(name)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
                Text(statusLine)
                    .font(.caption)
                    .foregroundStyle(.secondary)
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
        SettingsLink {
            Image(systemName: "gearshape")
                .imageScale(.large)
        }
        .buttonStyle(.borderless)
        .help("Settings")
        .accessibilityLabel(Text("Settings"))
        #else
        Button {
            navigation.sheet = .settings
        } label: {
            Image(systemName: "gearshape")
                .imageScale(.large)
        }
        .buttonStyle(.borderless)
        .help("Settings")
        .accessibilityLabel(Text("Settings"))
        #endif
    }
}
