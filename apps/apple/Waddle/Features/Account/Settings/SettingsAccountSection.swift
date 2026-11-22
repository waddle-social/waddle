import SwiftUI
import WaddleKit

/// Who is signed in (Android `settings` account card): avatar, name and
/// address, the server, and sign out. Handles the signed-out Mac Settings
/// window, where there is no session.
struct SettingsAccountSection: View {
    let app: AppState

    var body: some View {
        Section {
            if let session = app.session {
                SettingsAccountRow(account: session.coordinator.account)
            }
            FormValueRow(title: "Server", value: SignInServerLabel.text(for: app.server))
            if app.session != nil {
                AccountSignOutButton()
            } else {
                Text("You're not signed in.")
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        } header: {
            FormSectionHeader("Account")
        }
    }
}

/// Avatar, name and address of the signed-in account.
private struct SettingsAccountRow: View {
    let account: AccountIdentity

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            JIDAvatar(jid: account.jid, name: account.nick, size: Theme.Size.memberAvatar)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(account.nick)
                    .huddleText(Theme.Typography.author)
                    .foregroundStyle(Theme.Colors.fg)
                Text(account.jid.description)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .textSelection(.enabled)
            }
        }
        .padding(.vertical, Theme.Spacing.xs)
        .accessibilityElement(children: .combine)
    }
}

struct SettingsAboutSection: View {
    private static let website = URL(string: "https://waddle.social")

    var body: some View {
        Section {
            FormValueRow(title: "Version", value: SettingsAppVersion.current)
            if let website = Self.website {
                Link(destination: website) {
                    FormRowLabel(title: "Waddle website", symbol: "globe")
                }
            }
        } header: {
            FormSectionHeader("About")
        }
    }
}
