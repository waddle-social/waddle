import SwiftUI
import WaddleKit

/// Everyone you know here by address: your contacts, anyone else whose
/// status you see and people you have direct messages with, those here
/// now first. Tapping
/// someone opens their profile, with their details and "Message".
///
/// iPhone pushes it from Home; iPad and Mac show it from the sidebar.
struct PeopleView: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var query = ""

    var body: some View {
        let people = PeopleList(session: session, query: query)
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                PeopleSection(title: "Here now", people: people.here)
                PeopleSection(title: "Away and offline", people: people.away)
            }
            .padding(Theme.Spacing.l)
            .frame(maxWidth: Self.readableWidth)
            .frame(maxWidth: .infinity)
        }
        .background(Theme.Colors.bg)
        .navigationTitle("People")
        .searchable(text: $query, prompt: "Search people")
        .refreshable { await session.refreshContacts() }
        .overlay {
            if people.isEmpty {
                placeholder
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                NewMessageToolbarButton()
            }
            ToolbarItem(placement: .secondaryAction) {
                Button {
                    Task { await session.refreshContacts() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(session.contactsLoad == .loading)
                .help("Refresh")
            }
        }
    }

    /// Web `sizes.read`: a comfortable line length on a wide window.
    private static var readableWidth: CGFloat { 720 }

    @ViewBuilder
    private var placeholder: some View {
        if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            NoMatchesView(query: query)
        } else if session.contacts == nil {
            // Not known yet is not the same as no one.
            switch PeopleLoadNotice(load: session.contactsLoad, isOnline: session.connection == .online) {
            case .failed:
                EmptyStateView(
                    title: "People didn't load",
                    message: "Refresh to try again.",
                    symbol: "exclamationmark.triangle"
                )
            case .loading:
                EmptyStateView(
                    title: "Loading people",
                    message: "Your contacts show up here once they have loaded.",
                    symbol: "person.2"
                )
            case .offline:
                EmptyStateView(
                    title: "You're offline",
                    message: "Your contacts show up here when you're connected.",
                    symbol: "person.2"
                )
            }
        } else {
            EmptyStateView(
                title: "No one here yet",
                message: "Your contacts and the people you message show up here.",
                symbol: "person.2"
            )
        }
    }
}

/// What People says while the contact list is not known yet.
enum PeopleLoadNotice: Equatable {
    case loading
    case failed
    case offline

    /// After connecting, the contact list is asked for last, behind the
    /// rest of the session's start-up. Online with no lookup running yet
    /// is therefore still loading, not offline.
    init(load: ContactsLoad, isOnline: Bool) {
        switch load {
        case .failed: self = .failed
        case .loading: self = .loading
        case .idle: self = isOnline ? .loading : .offline
        }
    }
}

/// A titled group of people with its count; nothing when it is empty.
private struct PeopleSection: View {
    let title: String
    let people: [Person]

    var body: some View {
        if !people.isEmpty {
            Text("\(title) · \(people.count)")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgSoft)
                .padding(.horizontal, Theme.Spacing.m)
                .padding(.top, Theme.Spacing.m)
                .padding(.bottom, Theme.Spacing.xs)
                .accessibilityAddTraits(.isHeader)
            ForEach(people) { person in
                PersonRow(person: person)
            }
        }
    }
}

/// Avatar with presence, name, and the status message or, without one,
/// the availability in words.
private struct PersonRow: View {
    @Environment(NavigationModel.self) private var navigation
    let person: Person

    var body: some View {
        Button {
            navigation.showProfile(PeerProfileRoute(name: person.name, jid: person.jid, room: nil, nick: nil))
        } label: {
            NavigationRowChrome {
                HStack(spacing: Theme.Spacing.m) {
                    PeerPresenceAvatar(jid: person.jid, name: person.name, availability: person.availability, size: 40)
                    VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                        NavigationRowTitle(title: person.name, isEmphasised: person.availability.isOnline)
                        Text(statusLine)
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.fgMuted)
                            .lineLimit(1)
                    }
                }
                .padding(.vertical, Theme.Spacing.s)
            }
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(person.name), \(statusLine)"))
        .accessibilityHint(Text("Opens their profile"))
    }

    private var statusLine: String {
        let availability = AvailabilityTitle.title(person.availability)
        guard let status = person.status?.trimmingCharacters(in: .whitespacesAndNewlines), !status.isEmpty else {
            return availability
        }
        return "\(availability) · \(status)"
    }
}
