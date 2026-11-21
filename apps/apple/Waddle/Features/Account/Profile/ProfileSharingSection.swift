import SwiftUI
import WaddleKit

/// What the account shares about the moment: mood (XEP-0107), activity
/// (XEP-0108) and what it is listening to (XEP-0118), each opening its
/// editor.
struct ProfileSharingSection: View {
    @Environment(SessionCoordinator.self) private var session

    var body: some View {
        Section("Sharing") {
            NavigationLink {
                ProfileMoodEditor(current: session.status.mood)
            } label: {
                ProfileSharingLabel(
                    emoji: session.status.mood.map { ProfileMoodCatalog.emoji(for: $0.value) },
                    symbol: "face.smiling",
                    title: session.status.mood.map { ProfileMoodCatalog.title(for: $0.value) } ?? "Set a mood",
                    detail: session.status.mood.map(\.text) ?? "Let people know how you're feeling."
                )
            }
            NavigationLink {
                ProfileActivityEditor(current: session.status.activity)
            } label: {
                ProfileSharingLabel(
                    emoji: session.status.activity.map { ProfileActivityCatalog.emoji(for: $0.general) },
                    symbol: "figure.walk",
                    title: session.status.activity.map(ProfileActivityCatalog.summary) ?? "Set an activity",
                    detail: session.status.activity.map(\.text) ?? "Say what you're doing."
                )
            }
            NavigationLink {
                ProfileTuneEditor(current: session.status.tune)
            } label: {
                ProfileSharingLabel(
                    emoji: session.status.tune.map { _ in "🎵" },
                    symbol: "music.note",
                    title: session.status.tune.map(ProfileTuneSummary.title) ?? "Share a tune",
                    detail: session.status.tune.map(ProfileTuneSummary.detail) ?? "Say what you're listening to."
                )
            }
        }
    }
}

/// How a tune reads in one row: its title first, else whatever it has.
enum ProfileTuneSummary {
    static func title(_ tune: UserTune) -> String {
        [tune.title, tune.artist, tune.album, tune.link].first { !$0.isEmpty } ?? "Listening to"
    }

    /// The artist and album under a title; nil when the title line already
    /// says all there is.
    static func detail(_ tune: UserTune) -> String? {
        guard !tune.title.isEmpty else { return nil }
        let parts = [tune.artist, tune.album].filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// An emoji when something is shared, else a neutral symbol; then a title
/// and an optional second line.
private struct ProfileSharingLabel: View {
    let emoji: String?
    let symbol: String
    let title: String
    let detail: String?

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            Group {
                if let emoji {
                    Text(emoji)
                } else {
                    Image(systemName: symbol)
                        .foregroundStyle(.secondary)
                }
            }
            .font(.title2)
            .frame(width: 32)
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(title)
                if let detail {
                    Text(detail)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            .accessibilityElement(children: .combine)
        }
    }
}
