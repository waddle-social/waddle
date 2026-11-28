#if os(macOS)
import SwiftUI
import WaddleKit

/// The Mac sidebar account popover, in the web account menu's order.
struct SidebarAccountMenu: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openSettings) private var openSettings

    let preferences: Preferences

    var body: some View {
        VStack(spacing: Theme.Spacing.xs) {
            header
            HuddleDivider()
            action("Profile and status", symbol: "person.crop.circle") {
                dismiss()
                navigation.sheet = .profile
            }
            NavigationRowChrome { ProfileNotificationButton() }
            messageSounds
            AccountThemeButton(preferences: preferences)
            action("Settings", symbol: "gearshape") {
                dismiss()
                openSettings()
            }
            HuddleDivider()
            NavigationRowChrome { AccountSignOutButton(title: "Log out") }
        }
        .padding(WaddleTokens.Spacing.s2Half)
        .frame(width: 300)
        .background(Theme.Colors.surface)
        .environment(\.huddleGround, Theme.Colors.surface)
    }

    private var header: some View {
        SidebarAccountLabel(
            jid: session.account.jid,
            name: session.account.nick,
            availability: AccountStatusLine.availability(
                connection: session.connection,
                chosen: session.status.availability
            ),
            statusLine: session.account.jid.description,
            avatarSize: Theme.Size.avatar
        )
        .padding(Theme.Spacing.s)
    }

    private var messageSounds: some View {
        let enabled = preferences.messageSounds.isEnabled
        return action(
            enabled ? "Message sounds on" : "Message sounds off",
            symbol: enabled ? "speaker.wave.2" : "speaker.slash"
        ) {
            preferences.messageSounds.isEnabled.toggle()
        }
        .accessibilityAddTraits(enabled ? .isSelected : [])
    }

    private func action(_ title: String, symbol: String, perform: @escaping () -> Void) -> some View {
        Button(action: perform) {
            NavigationRowChrome { FormRowLabel(title: title, symbol: symbol) }
        }
        .buttonStyle(.plain)
    }
}

private struct AccountThemeButton: View {
    let preferences: Preferences

    var body: some View {
        let current = preferences.appearance
        let title = "Theme: \(current.title)"
        let next = current.next.title
        Button {
            preferences.appearance = preferences.appearance.next
        } label: {
            NavigationRowChrome {
                HStack(spacing: Theme.Spacing.m) {
                    icon(for: current)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .frame(width: Theme.Size.rowGlyph, height: Theme.Size.rowGlyph)
                        .accessibilityHidden(true)
                    Text(title)
                        .huddleText(Theme.Typography.label)
                        .foregroundStyle(Theme.Colors.fg)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(title))
        .accessibilityHint(Text("Click to switch to \(next)."))
        .help("\(title) — click for \(next)")
    }

    @ViewBuilder
    private func icon(for appearance: AppearancePreference) -> some View {
        switch appearance {
        case .system:
            ZStack {
                Image(systemName: "sun.max")
                    .offset(x: -3, y: -3)
                Image(systemName: "moon")
                    .offset(x: 4, y: 3)
            }
            .font(.system(size: 10))
        case .dark:
            Image(systemName: "moon").huddleText(Theme.Typography.body)
        case .light:
            Image(systemName: "sun.max").huddleText(Theme.Typography.body)
        }
    }
}
#endif
