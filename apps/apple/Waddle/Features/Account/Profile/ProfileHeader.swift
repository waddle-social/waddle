import SwiftUI
import WaddleKit

/// Avatar, nickname, bare JID and current presence.
struct ProfileHeader: View {
    @Environment(SessionCoordinator.self) private var session

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            JIDAvatar(jid: session.account.jid, name: session.account.nick, size: 72)
                .overlay(alignment: .bottomTrailing) {
                    PresenceDot(availability: shownAvailability, size: 18)
                        .offset(x: 4, y: 4)
                }
                .padding(.bottom, Theme.Spacing.xs)
            Text(session.account.nick)
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
            Text(session.account.jid.description)
                .font(.subheadline)
                .foregroundStyle(.secondary)
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
            .font(.footnote)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.center)
            .lineLimit(2)
    }

    private var summary: String {
        let title = availability == .offline ? "Offline" : ProfileAvailabilityOption(availability).title
        guard let statusText else { return title }
        return "\(title) · \(statusText)"
    }
}
