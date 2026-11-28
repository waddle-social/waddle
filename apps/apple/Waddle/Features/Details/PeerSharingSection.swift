import SwiftUI
import WaddleKit

/// What someone shares about the moment: their mood (XEP-0107), activity
/// (XEP-0108) and tune (XEP-0118). An activity that marks a call reads
/// "In a call" (ADR-010). Nothing is drawn when there is nothing to show.
struct PeerSharingSection: View {
    /// What they share, as looked up; nil until it is known.
    let status: SharedStatus?
    let isOnline: Bool
    var showsCallInHeader = false

    var body: some View {
        if let shown = shownStatus, !shown.isEmpty {
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
                FormSectionHeader("Right now")
            }
        }
    }

    private var shownStatus: SharedStatus? {
        guard var shown = status?.shown(isOnline: isOnline) else { return nil }
        if showsCallInHeader, shown.activity?.showsACall == true {
            shown.activity = nil
        }
        return shown
    }
}

private extension String {
    var nonBlank: String? {
        trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : self
    }
}

extension View {
    /// Keeps `status` set to what `jid` shares, nil until it is looked up.
    ///
    /// Looked up when the person is shown and again on each new stream:
    /// mood and tune have no notification, and a stream starts knowing no
    /// one's activity (a call that began before it sent none). Not when
    /// they come online: a call of theirs arrives as a notification, and a
    /// lookup then would read back a call a crashed device left behind. In
    /// between, the activity of someone whose notifications are heard
    /// follows them, so show it with
    /// `SessionCoordinator.liveSharedStatus(of:lookedUp:)`. A request that
    /// fails reads as nothing shared, so a later lookup that finds nothing
    /// at all leaves what is shown (`SharedStatus.lookedUpAgain`).
    ///
    /// Someone whose notifications are not heard (seen only in a room)
    /// has only the lookup: while it shows them in a call, it is looked up
    /// again every few seconds until the call ends (`UnheardCallCheck`).
    func peerSharedStatus(
        of jid: BareJID?, isOnline: Bool, session: SessionCoordinator, into status: Binding<SharedStatus?>
    ) -> some View {
        let lookup = SharedStatusLookup(jid: jid, session: session)
        let callCheck = UnheardCallCheck(jid: jid, shown: status.wrappedValue, isOnline: isOnline, session: session)
        return onChange(of: jid) { status.wrappedValue = nil }
            .task(id: lookup) {
                guard let jid, lookup.isConnected else { return }
                // A lookup for the person shown before can still answer
                // after this one; cancelling does not stop it.
                let loaded = await session.sharedStatus(of: jid)
                guard !Task.isCancelled, let loaded else { return }
                status.wrappedValue = status.wrappedValue?.lookedUpAgain(loaded) ?? loaded
            }
            .task(id: callCheck) {
                guard callCheck.isChecking, let jid else { return }
                while !Task.isCancelled {
                    try? await Task.sleep(for: callEndCheckInterval)
                    guard !Task.isCancelled else { return }
                    let loaded = await session.sharedStatus(of: jid)
                    guard !Task.isCancelled else { return }
                    guard let loaded else { continue }
                    status.wrappedValue = status.wrappedValue?.callCheckedAgain(loaded) ?? loaded
                }
            }
    }
}

/// When to look someone's shared status up: for each person shown, and
/// again whenever a stream comes up.
struct SharedStatusLookup: Hashable {
    let jid: BareJID?
    let isConnected: Bool

    @MainActor
    init(jid: BareJID?, session: SessionCoordinator) {
        self.jid = jid
        isConnected = session.connection == .online
    }
}

/// Whether to keep looking for the end of someone's call: only while a
/// call shown comes from a lookup alone. Someone whose notifications are
/// heard is told when it ends, and the account's own is checked by
/// `checkingOwnCallEnds`.
struct UnheardCallCheck: Hashable {
    let jid: BareJID?
    let isChecking: Bool

    /// `shown` is what was looked up; `isOnline` the availability the
    /// screen found for them, from any source.
    @MainActor
    init(jid: BareJID?, shown: SharedStatus?, isOnline: Bool, session: SessionCoordinator) {
        self.jid = jid
        let showsACall = shown?.shown(isOnline: isOnline).activity?.showsACall == true
        isChecking = jid.map { $0 != session.account.jid && !session.hearsActivity(of: $0) } == true
            && showsACall && session.connection == .online
    }
}
