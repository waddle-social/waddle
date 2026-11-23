import SwiftUI
import WaddleKit

/// What the account shares about the moment: mood (XEP-0107), activity
/// (XEP-0108) and what it is listening to (XEP-0118), each opening its
/// editor.
struct ProfileSharingSection: View {
    @Environment(SessionCoordinator.self) private var session

    var body: some View {
        Section {
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
            if session.status.activity?.showsACall == true {
                // The call owns the activity until it ends (ADR-010).
                ProfileSharingLabel(
                    emoji: "📞", symbol: "phone",
                    title: "In a call",
                    detail: "Shown while you're in a call on another device."
                )
                .checkingOwnCallEnds(while: true, session: session)
            } else {
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
        } header: {
            FormSectionHeader("Sharing")
        }
    }
}

extension View {
    /// While `isShown` (the account's own activity shows a call), looks it
    /// up again every few seconds. Nothing tells this device when another
    /// device of the account leaves its call (the core drops the account's
    /// own notifications); once the activity no longer shows a call,
    /// `isShown` turns false and the checking stops.
    func checkingOwnCallEnds(while isShown: Bool, session: SessionCoordinator) -> some View {
        task(id: isShown) {
            guard isShown else { return }
            while !Task.isCancelled {
                try? await Task.sleep(for: callEndCheckInterval)
                guard !Task.isCancelled else { return }
                await session.refreshOwnActivity()
            }
        }
    }
}

/// How often a call shown as in progress, and that no notification will
/// end, is checked for having ended: the account's own on another device
/// (`checkingOwnCallEnds`), or someone's whose activity is not heard
/// (`peerSharedStatus`).
let callEndCheckInterval: Duration = .seconds(10)

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
/// and an optional second line. Also draws what someone else shares
/// (`PeerSharingSection`).
struct ProfileSharingLabel: View {
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
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
            }
            .huddleText(Theme.Typography.title)
            .frame(width: Theme.Size.sharingGlyph)
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(title)
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fg)
                if let detail {
                    Text(detail)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(2)
                }
            }
            .accessibilityElement(children: .combine)
        }
    }
}
