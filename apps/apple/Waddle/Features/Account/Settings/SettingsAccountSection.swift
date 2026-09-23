import SwiftUI

/// Server and account address, with sign out. Handles the signed-out Mac
/// Settings window, where there is no session.
struct SettingsAccountSection: View {
    let app: AppState

    var body: some View {
        Section("Account") {
            LabeledContent("Server", value: SignInServerLabel.text(for: app.server))
            if let session = app.session {
                LabeledContent("Account") {
                    Text(session.coordinator.account.jid.description)
                        .textSelection(.enabled)
                }
                AccountSignOutButton()
            } else {
                Text("You're not signed in.")
                    .foregroundStyle(.secondary)
            }
        }
    }
}

struct SettingsAboutSection: View {
    private static let website = URL(string: "https://waddle.social")

    var body: some View {
        Section("About") {
            LabeledContent("Version", value: SettingsAppVersion.current)
            if let website = Self.website {
                Link(destination: website) {
                    Label("Waddle website", systemImage: "globe")
                }
            }
        }
    }
}
