import SwiftUI
import WaddleKit

/// DM details: the peer, their presence, what they share and their
/// details, notifications and search.
struct DirectDetailsForm: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let peer: BareJID
    @State private var details: ProfileDetails?
    @State private var shared: SharedStatus?

    var body: some View {
        let presence = session.presence.contacts[peer]
        Form {
            Section {
                DirectPeerHeader(
                    peer: peer,
                    name: session.directory.title(for: .direct(peer)),
                    presence: presence
                )
            }
            PeerSharingSection(
                status: session.liveSharedStatus(of: peer, lookedUp: shared),
                isOnline: presence?.availability.isOnline ?? false
            )
            if let details {
                PeerDetailsSection(details: details)
            }
            NotificationModeSection(conversation: .direct(peer))
            Section {
                FormActionButton(title: "Search in conversation", symbol: "magnifyingglass") {
                    navigation.sheet = .search(.direct(peer))
                }
            }
        }
        .huddleForm()
        .peerDetails(of: peer, session: session, into: $details)
        .peerSharedStatus(
            of: peer, isOnline: presence?.availability.isOnline ?? false, session: session, into: $shared
        )
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
    let availability: Availability
    let status: String?
    let idleSince: Date?
    /// Their role in the room the profile was opened from.
    var role: MemberRoleLine?

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            avatar
            Text(name)
                .huddleText(Theme.Typography.title)
                .foregroundStyle(Theme.Colors.fg)
                .multilineTextAlignment(.center)
            if let jid {
                Text(jid.description)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .textSelection(.enabled)
            }
            if let role {
                role
            }
            Text(AvailabilityTitle.title(availability))
                .huddleText(Theme.Typography.label)
                .foregroundStyle(Theme.Colors.fgMuted)
            if let statusText {
                Text(statusText)
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fg)
                    .multilineTextAlignment(.center)
            }
            if let idleSince, availability.isOnline {
                Text("Last active \(idleSince.formatted(.relative(presentation: .named)))")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.s)
        .accessibilityElement(children: .combine)
    }

    private var avatar: some View {
        PeerPresenceAvatar(jid: jid, name: name, availability: availability, size: Theme.Size.profileAvatar)
    }

    private var statusText: String? {
        guard let text = status?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return nil }
        return text
    }
}

/// A message author's profile, opened from their avatar or name in a
/// timeline, a thread or People: what `DirectPeerHeader` shows, what they
/// share, their details, plus "Message"
/// when a real bare JID other than our own is known.
struct PeerProfileSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    let profile: PeerProfileRoute
    @State private var details: ProfileDetails?
    @State private var shared: SharedStatus?

    var body: some View {
        let presence = PeerProfilePresence.resolve(profile, session: session)
        NavigationStack {
            Form {
                Section {
                    PeerHeader(
                        name: profile.name,
                        jid: profile.jid,
                        availability: presence.availability,
                        status: presence.status,
                        idleSince: presence.idleSince,
                        role: roomRole
                    )
                    if let peer = messageablePeer {
                        // Android `conversation_author_profile`: the one
                        // action, a pill under the header.
                        Button {
                            message(peer)
                        } label: {
                            Label("Message", systemImage: "bubble.left")
                        }
                        .buttonStyle(.huddle(.primary))
                        .frame(maxWidth: .infinity)
                        .accessibilityHint(Text("Opens your direct conversation"))
                    }
                }
                PeerSharingSection(
                    status: profile.jid.flatMap { session.liveSharedStatus(of: $0, lookedUp: shared) },
                    isOnline: presence.availability.isOnline
                )
                if let details {
                    PeerDetailsSection(details: details)
                }
            }
            .huddleForm()
            .peerDetails(of: profile.jid, session: session, into: $details)
            .peerSharedStatus(
                of: profile.jid, isOnline: presence.availability.isOnline, session: session, into: $shared
            )
            .checkingOwnCallEnds(while: isOwnCallShown, session: session)
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

    /// Their role in the room the profile was opened from: their
    /// occupant's while they are in it, else the member list's.
    private var roomRole: MemberRoleLine? {
        if let occupant = PeerProfilePresence.occupant(of: profile, session: session) {
            return MemberRoleLine(affiliation: occupant.affiliation, hats: occupant.hats)
        }
        return profile.affiliation.map { MemberRoleLine(affiliation: $0, hats: []) }
    }

    /// Our own profile showing a call on another device. No notification
    /// about our own activity reaches here, so its end is looked for.
    private var isOwnCallShown: Bool {
        profile.jid == session.account.jid && session.status.activity?.showsACall == true
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
        if let occupant = occupant(of: profile, session: session) {
            return Resolved(availability: occupant.availability, status: occupant.status, idleSince: nil)
        }
        if let jid = profile.jid, let contact = session.presence.contacts[jid] {
            return Resolved(availability: contact.availability, status: contact.status, idleSince: contact.idleSince)
        }
        return Resolved(availability: .offline, status: nil, idleSince: nil)
    }

    /// The room occupant the profile was opened from, only while its nick
    /// still belongs to the same person: someone who takes a freed nick
    /// is not them.
    @MainActor
    static func occupant(of profile: PeerProfileRoute, session: SessionCoordinator) -> Occupant? {
        guard let room = profile.room, let nick = profile.nick,
              let occupant = session.presence.occupant(named: nick, in: room) else { return nil }
        guard let jid = profile.jid, let realJID = occupant.realJID else { return occupant }
        return realJID == jid ? occupant : nil
    }
}
