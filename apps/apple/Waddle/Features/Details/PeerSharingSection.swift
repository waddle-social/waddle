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
            Section {
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
            } header: {
                DetailsSectionHeader("Right now")
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
    /// stay open through a whole call, so for as long as "In a call" is
    /// shown it is looked up again every few seconds. A repeat lookup that
    /// fails leaves what is shown. When the person goes offline a call's
    /// mark is forgotten, not just hidden: it may be one their device left
    /// behind, and must not come back when they return (ADR-010).
    func peerSharedStatus(
        of jid: BareJID?, isOnline: Bool, session: SessionCoordinator, into status: Binding<SharedStatus?>
    ) -> some View {
        let showsACall = status.wrappedValue?.shown(isOnline: isOnline).activity?.showsACall == true
        return task(id: jid) {
            status.wrappedValue = nil
            guard let jid else { return }
            // A lookup for the person shown before can still answer after
            // this one; cancelling does not stop it.
            let loaded = await session.sharedStatus(of: jid)
            guard !Task.isCancelled else { return }
            status.wrappedValue = loaded
        }
        .onChange(of: isOnline) { _, isOnline in
            guard !isOnline, let held = status.wrappedValue else { return }
            status.wrappedValue = held.shown(isOnline: false)
        }
        .task(id: PeerCallWatch(jid: jid, showsACall: showsACall)) {
            guard showsACall, let jid else { return }
            while !Task.isCancelled {
                try? await Task.sleep(for: ProfileSharingSection.callCheckInterval)
                guard !Task.isCancelled else { return }
                let loaded = await session.sharedStatus(of: jid)
                guard !Task.isCancelled else { return }
                if let loaded { status.wrappedValue = loaded }
            }
        }
    }
}

/// Whose call is being watched, and whether one is shown at all.
private struct PeerCallWatch: Hashable {
    let jid: BareJID?
    let showsACall: Bool
}
