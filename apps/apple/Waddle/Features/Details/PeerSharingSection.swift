import SwiftUI
import WaddleKit

/// What someone shares about the moment: their mood (XEP-0107), activity
/// (XEP-0108) and tune (XEP-0118). An activity that marks a call reads
/// "In a call" (ADR-010). Nothing is drawn when there is nothing to show.
struct PeerSharingSection: View {
    /// What they share, as looked up; nil until it is known.
    let status: SharedStatus?
    let isOnline: Bool

    var body: some View {
        if let shown = status?.shown(isOnline: isOnline), !shown.isEmpty {
            Section("Right now") {
                if let mood = shown.mood {
                    ProfileSharingLabel(
                        emoji: ProfileMoodCatalog.emoji(for: mood.value), symbol: "face.smiling",
                        title: ProfileMoodCatalog.title(for: mood.value), detail: mood.text?.nonBlank
                    )
                }
                if let activity = shown.activity {
                    if activity.showsACall {
                        ProfileSharingLabel(emoji: "📞", symbol: "phone", title: "In a call", detail: nil)
                    } else {
                        ProfileSharingLabel(
                            emoji: ProfileActivityCatalog.emoji(for: activity.general), symbol: "figure.walk",
                            title: ProfileActivityCatalog.summary(of: activity), detail: activity.text?.nonBlank
                        )
                    }
                }
                if let tune = shown.tune {
                    ProfileSharingLabel(
                        emoji: "🎵", symbol: "music.note",
                        title: ProfileTuneSummary.title(tune), detail: ProfileTuneSummary.detail(tune)
                    )
                }
            }
        }
    }
}

private extension String {
    var nonBlank: String? {
        trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : self
    }
}

extension View {
    /// Keeps `status` set to what `jid` shares, nil until it is known.
    ///
    /// Nothing tells this device when someone's mood, activity or tune
    /// changes, so it is looked up when the person is shown. DM details can
    /// stay open through a whole call, so while the answer shows a call it
    /// is looked up again every few seconds, until the call has ended.
    func peerSharedStatus(of jid: BareJID?, session: SessionCoordinator, into status: Binding<SharedStatus?>) -> some View {
        task(id: jid) {
            status.wrappedValue = nil
            guard let jid else { return }
            while true {
                // A lookup for the person shown before can still answer
                // after this one; cancelling does not stop it.
                let loaded = await session.sharedStatus(of: jid)
                guard !Task.isCancelled else { return }
                status.wrappedValue = loaded
                guard loaded?.activity?.showsACall == true else { return }
                try? await Task.sleep(for: .seconds(10))
                guard !Task.isCancelled else { return }
            }
        }
    }
}
