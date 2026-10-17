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
        PeerHeader(
            name: name,
            jid: peer,
            colorKey: peer.description,
            availability: presence?.availability ?? .offline,
            status: presence?.status,
            idleSince: presence?.idleSince
        )
    }
}

/// Large avatar with presence, name, the bare JID when known, and the
/// status text. Shared by DM details and the peer profile sheet.
struct PeerHeader: View {
    let name: String
    let jid: BareJID?
    let colorKey: String
    let availability: Availability
    let status: String?
    let idleSince: Date?

    private static let avatarSize: CGFloat = 88

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            avatar
            Text(name)
                .font(.title2.weight(.bold))
                .multilineTextAlignment(.center)
            if let jid {
                Text(jid.description)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            HStack(spacing: Theme.Spacing.xs) {
                PresenceDot(availability: availability, size: 8)
                Text(AvailabilityTitle.title(availability))
                    .font(.subheadline)
            }
            if let statusText {
                Text(statusText)
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            if let idleSince, availability.isOnline {
                Text("Last active \(idleSince.formatted(.relative(presentation: .named)))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.s)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var avatar: some View {
        if let jid {
            PeerPresenceAvatar(jid: jid, name: name, availability: availability, size: Self.avatarSize)
        } else {
            AvatarView(name: name, colorKey: colorKey, size: Self.avatarSize)
                .overlay(alignment: .bottomTrailing) {
                    PresenceDot(availability: availability, size: Self.avatarSize * 0.3)
                        .offset(x: Self.avatarSize * 0.075, y: Self.avatarSize * 0.075)
                }
        }
    }

    private var statusText: String? {
        guard let text = status?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return nil }
        return text
    }
}

/// A message author's profile, opened from their avatar or name in a
/// timeline or thread: what `DirectPeerHeader` shows, plus "Message"
/// when a real bare JID other than our own is known.
struct PeerProfileSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    let profile: PeerProfileRoute

    var body: some View {
        let presence = PeerProfilePresence.resolve(profile, session: session)
        NavigationStack {
            Form {
                Section {
                    PeerHeader(
                        name: profile.name,
                        jid: profile.jid,
                        colorKey: profile.colorKey,
                        availability: presence.availability,
                        status: presence.status,
                        idleSince: presence.idleSince
                    )
                }
                if let peer = messageablePeer {
                    Section {
                        Button {
                            message(peer)
                        } label: {
                            Label("Message", systemImage: "bubble.left")
                                .frame(maxWidth: .infinity, minHeight: Theme.Size.minHitTarget, alignment: .leading)
                                .contentShape(Rectangle())
                        }
                        .accessibilityHint(Text("Opens your direct conversation"))
                    }
                }
            }
            .formStyle(.grouped)
            .navigationTitle(profile.name)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 360, minHeight: 380)
        #endif
    }

    /// A real JID that is not our own account; anonymous room occupants
    /// have none.
    private var messageablePeer: BareJID? {
        guard let jid = profile.jid, jid != session.account.jid else { return nil }
        return jid
    }

    private func message(_ peer: BareJID) {
        let conversation = session.directConversation(with: peer)
        navigation.sheet = nil
        navigation.open(conversation)
    }
}

/// The presence a peer profile shows: our own status for ourselves, the
/// room occupant's presence when the row came from a room (only while the
/// nick still belongs to the same real JID), else the roster contact's.
enum PeerProfilePresence {
    struct Resolved {
        let availability: Availability
        let status: String?
        let idleSince: Date?
    }

    @MainActor
    static func resolve(_ profile: PeerProfileRoute, session: SessionCoordinator) -> Resolved {
        if let jid = profile.jid, jid == session.account.jid {
            return Resolved(
                availability: AccountStatusLine.availability(connection: session.connection, chosen: session.status.availability),
                status: session.status.statusText,
                idleSince: nil
            )
        }
        if let room = profile.room, let nick = profile.nick,
           let occupant = session.presence.occupant(named: nick, in: room),
           profile.jid == nil || occupant.realJID == nil || occupant.realJID == profile.jid {
            return Resolved(availability: occupant.availability, status: occupant.status, idleSince: nil)
        }
        if let jid = profile.jid, let contact = session.presence.contacts[jid] {
            return Resolved(availability: contact.availability, status: contact.status, idleSince: contact.idleSince)
        }
        return Resolved(availability: .offline, status: nil, idleSince: nil)
    }
}
