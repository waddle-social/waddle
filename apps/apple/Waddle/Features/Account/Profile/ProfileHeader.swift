import SwiftUI
import WaddleKit

/// Avatar with presence, name, address and status (Android `profile`).
struct ProfileHeader: View {
    @Environment(SessionCoordinator.self) private var session

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            PeerPresenceAvatar(
                jid: session.account.jid, name: session.account.nick,
                availability: shownAvailability, size: Theme.Size.profileAvatar
            )
            .padding(.bottom, Theme.Spacing.xs)
            Text(session.account.nick)
                .huddleText(Theme.Typography.title)
                .foregroundStyle(Theme.Colors.fg)
                .multilineTextAlignment(.center)
            Text(session.account.jid.description)
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
                .textSelection(.enabled)
                .multilineTextAlignment(.center)
            ProfilePresenceSummary(availability: shownAvailability, statusText: session.status.statusText)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.s)
    }

    /// What contacts currently see: offline whenever we are not connected.
    private var shownAvailability: Availability {
        session.connection == .online ? session.status.availability : .offline
    }
}

/// "Online · In a meeting" line under the JID.
private struct ProfilePresenceSummary: View {
    let availability: Availability
    let statusText: String?

    var body: some View {
        Text(summary)
            .huddleText(Theme.Typography.label)
            .foregroundStyle(Theme.Colors.fgMuted)
            .multilineTextAlignment(.center)
            .lineLimit(2)
    }

    private var summary: String {
        let title = availability == .offline ? "Offline" : ProfileAvailabilityOption(availability).title
        guard let statusText else { return title }
        return "\(title) · \(statusText)"
    }
}
