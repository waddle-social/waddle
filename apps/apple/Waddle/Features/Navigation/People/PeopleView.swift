import SwiftUI
import WaddleKit

/// Everyone you know here by address: people whose status you see and
/// people you have direct messages with, those here now first. Tapping
/// someone opens their profile, with their details and "Message".
///
/// iPhone pushes it from Home; iPad and Mac show it from the sidebar.
struct PeopleView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
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
        .overlay {
            if people.isEmpty {
                placeholder
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                NewMessageToolbarButton()
            }
        }
    }

    /// Web `sizes.read`: a comfortable line length on a wide window.
    private static var readableWidth: CGFloat { 720 }

    @ViewBuilder
    private var placeholder: some View {
        if query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            EmptyStateView(
                title: "No one here yet",
                message: "People you message, and people who share their status with you, show up here.",
                symbol: "person.2"
            )
        } else {
            NoMatchesView(query: query)
        }
    }
}

extension PeopleList {
    /// The people `session` knows now, named as its lists name them.
    @MainActor
    init(session: SessionCoordinator, query: String = "") {
        self = PeopleList.build(
            contacts: session.presence.contacts,
            directPeers: session.directory.directConversations.map(\.peer),
            own: session.account.jid,
            name: { session.directory.title(for: .direct($0)) },
            query: query
        )
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
            navigation.sheet = .peer(PeerProfileRoute(name: person.name, jid: person.jid, room: nil, nick: nil))
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

/// Home's way into People on the phone: a row saying how many are here.
struct PeopleHomeRow: View {
    @Environment(SessionCoordinator.self) private var session

    var body: some View {
        let here = PeopleList(session: session).here.count
        NavigationRowChrome {
            NavigationRowLabel(title: "People") {
                Image(systemName: "person.2")
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Theme.Colors.fgMuted)
            } trailing: {
                if here > 0 {
                    Text("\(here) here")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgSoft)
                }
            }
        }
        // The link sits behind the row so the list draws no chevron.
        .background {
            NavigationLink { PeopleView() } label: { EmptyView() }
                .opacity(0)
        }
        .navigationListRow()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(here > 0 ? "People, \(here) here" : "People"))
        .accessibilityAddTraits(.isButton)
    }
}
