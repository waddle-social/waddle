import SwiftUI
import WaddleKit

/// DM details: the peer, their presence, notifications and search.
struct DirectDetailsForm: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let peer: BareJID

    var body: some View {
        Form {
            Section {
                DirectPeerHeader(
                    peer: peer,
                    name: session.directory.title(for: .direct(peer)),
                    presence: session.presence.contacts[peer]
                )
            }
            NotificationModeSection(conversation: .direct(peer))
            Section {
                Button {
                    navigation.sheet = .search(.direct(peer))
                } label: {
                    Label("Search in conversation", systemImage: "magnifyingglass")
                }
            }
        }
        .formStyle(.grouped)
    }
}

/// Large avatar, name, address and RFC 6121 presence with status text.
struct DirectPeerHeader: View {
    let peer: BareJID
    let name: String
    let presence: PresenceStore.ContactPresence?

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            PeerPresenceAvatar(jid: peer, name: name, availability: availability, size: 88)
            Text(name)
                .font(.title2.weight(.bold))
                .multilineTextAlignment(.center)
            Text(peer.description)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
            HStack(spacing: Theme.Spacing.xs) {
                PresenceDot(availability: availability, size: 8)
                Text(AvailabilityTitle.title(availability))
                    .font(.subheadline)
            }
            if let status {
                Text(status)
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            if let idleSince = presence?.idleSince, availability.isOnline {
                Text("Last active \(idleSince.formatted(.relative(presentation: .named)))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.s)
        .accessibilityElement(children: .combine)
    }

    private var availability: Availability {
        presence?.availability ?? .offline
    }

    private var status: String? {
        guard let text = presence?.status?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return nil }
        return text
    }
}
