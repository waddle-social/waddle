import SwiftUI
import WaddleKit

/// The lists of the admin console.
enum AdminSection: Hashable {
    case users
    case spaces
}

/// The admin console, for community owners: everyone with an account on
/// this server, and its spaces. The server checks every request, so
/// someone who is not an owner sees "Admin access only" here.
///
/// iPad and Mac show it from the sidebar; iPhone pushes it from You.
struct AdminView: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var section: AdminSection
    @State private var users: AdminList<AdminUser>?
    @State private var spaces: AdminList<AdminSpace>?
    @State private var query = ""
    @State private var sheet: AdminSpaceSheet.Subject?

    init(section: AdminSection = .users) {
        _section = State(initialValue: section)
    }

    var body: some View {
        VStack(spacing: 0) {
            HuddleSegmentedControl(selection: shownSection, segments: [
                .init(value: .users, title: "Users"),
                .init(value: .spaces, title: "Spaces"),
            ])
            .padding(.vertical, Theme.Spacing.s)
            panel
        }
        .background(Theme.Colors.bg)
        .navigationTitle("Admin")
        .searchable(text: $query, prompt: section == .users ? "Search users" : "Search spaces")
        .task(id: Search(section: section, query: query, isOnline: session.connection == .online)) {
            switch section {
            case .users:
                let list = users ?? session.adminUsersList()
                await search(list, isFirstLoad: users == nil) { users = list }
            case .spaces:
                let list = spaces ?? session.adminSpacesList()
                await search(list, isFirstLoad: spaces == nil) { spaces = list }
            }
        }
        .toolbar {
            if section == .spaces {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        sheet = .new
                    } label: {
                        Label("New space", systemImage: "plus")
                    }
                    .disabled(spaces == nil)
                    .help("New space")
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await reload() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(isLoading)
                .help("Refresh")
            }
        }
        .sheet(item: $sheet) { subject in
            if let spaces {
                AdminSpaceSheet(subject: subject, spaces: spaces)
                    .carryingCallDock()
            }
        }
    }

    @ViewBuilder
    private var panel: some View {
        switch section {
        case .users:
            if let users {
                AdminUsersPanel(users: users)
            } else {
                Theme.Colors.bg
            }
        case .spaces:
            if let spaces {
                AdminSpacesPanel(spaces: spaces) { sheet = .space($0) }
            } else {
                Theme.Colors.bg
            }
        }
    }

    /// Text typed for one list is not a search of the other.
    private var shownSection: Binding<AdminSection> {
        Binding {
            section
        } set: {
            guard $0 != section else { return }
            query = ""
            section = $0
        }
    }

    private var isLoading: Bool {
        switch section {
        case .users: users?.isLoading ?? true
        case .spaces: spaces?.isLoading ?? true
        }
    }

    private func reload() async {
        switch section {
        case .users: await users?.reload()
        case .spaces: await spaces?.reload()
        }
    }

    /// Shows `list` and asks it for the text typed, after a pause in
    /// typing; a list shown for the first time asks at once.
    private func search<Row>(_ list: AdminList<Row>, isFirstLoad: Bool, show: () -> Void) async {
        show()
        if !isFirstLoad {
            try? await Task.sleep(for: .milliseconds(250))
        }
        guard !Task.isCancelled else { return }
        await list.load(matching: query)
    }

    /// What the list on screen answers: asked again when the list or the
    /// text changes and when the connection comes back.
    private struct Search: Hashable {
        let section: AdminSection
        let query: String
        let isOnline: Bool
    }
}

/// The users found, a page at a time, or why there are none to show.
struct AdminUsersPanel: View {
    let users: AdminList<AdminUser>

    var body: some View {
        AdminListPanel(list: users, copy: .users) { user in
            AdminUserRow(user: user)
        }
    }
}

/// One list of the console: its name, its rows a page at a time, or why
/// there are none to show.
struct AdminListPanel<Row: Identifiable & Sendable, RowView: View>: View {
    let list: AdminList<Row>
    let copy: AdminListCopy
    @ViewBuilder let row: (Row) -> RowView

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                AdminPanelHeader(title: copy.title, caption: copy.caption)
                if list.failure != nil, !list.rows.isEmpty {
                    AdminNoticeLine(text: "Couldn't refresh. This list may be out of date.")
                }
                ForEach(list.rows) { row($0) }
                if list.hasMore {
                    AdminLoadMore(isLoading: list.isLoadingMore, failed: list.moreFailure != nil) {
                        Task { await list.loadMore() }
                    }
                }
            }
            .padding(Theme.Spacing.l)
            .frame(maxWidth: Self.readableWidth)
            .frame(maxWidth: .infinity)
        }
        .refreshable { await list.reload() }
        .overlay {
            if let notice = AdminListNotice(
                isEmpty: list.rows.isEmpty,
                hasLoaded: list.hasLoaded,
                failure: list.failure,
                query: list.query
            ) {
                AdminNoticeView(notice: notice, query: list.query, copy: copy)
            }
        }
    }

    /// Web `sizes.read`: a comfortable line length on a wide window.
    private static var readableWidth: CGFloat { 720 }
}

/// What a list of the console says in place of its rows.
enum AdminListNotice: Equatable {
    case loading
    case notAllowed
    case offline
    case failed
    case noMatches
    case empty

    /// Nil while there are rows to show. A list not loaded yet is loading,
    /// never empty: "no one" is only said once the server has said so,
    /// and `query` is the text it answered, not the text being typed.
    init?(isEmpty: Bool, hasLoaded: Bool, failure: PortError?, query: String) {
        guard isEmpty else { return nil }
        switch failure {
        case .rejected: self = .notAllowed
        case .notConnected: self = .offline
        case .some: self = .failed
        case nil:
            if !hasLoaded {
                self = .loading
            } else {
                self = query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? .empty : .noMatches
            }
        }
    }
}

/// The words of one list of the console.
struct AdminListCopy {
    let title: String
    let caption: String
    let symbol: String
    let loadingTitle: String
    let offlineMessage: String
    let failedTitle: String
    let noMatches: (_ query: String) -> String
    let emptyTitle: String
    let emptyMessage: String

    static let users = AdminListCopy(
        title: "Users",
        caption: "Everyone with an account here.",
        symbol: "person.2",
        loadingTitle: "Loading users",
        offlineMessage: "Users show up here when you're connected.",
        failedTitle: "Users didn't load",
        noMatches: { "No one's name or address starts with “\($0)”." },
        emptyTitle: "No users yet",
        emptyMessage: "People show up here once they have an account."
    )

    static let spaces = AdminListCopy(
        title: "Spaces",
        caption: "Groups of rooms in this community.",
        symbol: "square.grid.2x2",
        loadingTitle: "Loading spaces",
        offlineMessage: "Spaces show up here when you're connected.",
        failedTitle: "Spaces didn't load",
        noMatches: { "No space's name starts with “\($0)”." },
        emptyTitle: "No spaces yet",
        emptyMessage: "Make one to group rooms together."
    )
}

private struct AdminNoticeView: View {
    let notice: AdminListNotice
    let query: String
    let copy: AdminListCopy

    var body: some View {
        switch notice {
        case .loading:
            EmptyStateView(title: copy.loadingTitle, message: copy.caption, symbol: copy.symbol)
        case .notAllowed:
            EmptyStateView(
                title: "Admin access only",
                message: "Ask a community owner if you need to manage this community.",
                symbol: "lock"
            )
        case .offline:
            EmptyStateView(title: "You're offline", message: copy.offlineMessage, symbol: copy.symbol)
        case .failed:
            EmptyStateView(title: copy.failedTitle, message: "Refresh to try again.", symbol: "exclamationmark.triangle")
        case .noMatches:
            EmptyStateView(title: "No matches", message: copy.noMatches(query), symbol: "magnifyingglass")
        case .empty:
            EmptyStateView(title: copy.emptyTitle, message: copy.emptyMessage, symbol: copy.symbol)
        }
    }
}

/// A panel's name and one line on what it holds.
private struct AdminPanelHeader: View {
    let title: String
    let caption: String

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            Text(title)
                .huddleText(Theme.Typography.title)
                .foregroundStyle(Theme.Colors.fg)
                .accessibilityAddTraits(.isHeader)
            Text(caption)
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .padding(.horizontal, Theme.Spacing.m)
        .padding(.bottom, Theme.Spacing.m)
    }
}

/// A quiet line above rows that could not be brought up to date.
private struct AdminNoticeLine: View {
    let text: String

    var body: some View {
        Label(text, systemImage: "exclamationmark.triangle")
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.fgMuted)
            .padding(.horizontal, Theme.Spacing.m)
            .padding(.bottom, Theme.Spacing.s)
    }
}

/// Avatar, name and address, with a tag on community owners. Opens their
/// profile.
private struct AdminUserRow: View {
    @Environment(NavigationModel.self) private var navigation
    let user: AdminUser

    var body: some View {
        Button {
            navigation.showProfile(PeerProfileRoute(name: user.name, jid: user.jid, room: nil, nick: nil))
        } label: {
            NavigationRowChrome {
                HStack(spacing: Theme.Spacing.m) {
                    JIDAvatar(jid: user.jid, name: user.name, size: 40)
                    VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                        NavigationRowTitle(title: user.name, isEmphasised: true)
                        Text(user.jid.description)
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.fgMuted)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: Theme.Spacing.xs)
                    if user.isOwner {
                        HuddleTag("Owner")
                    }
                }
                .padding(.vertical, Theme.Spacing.s)
            }
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(user.isOwner ? "\(user.name), \(user.jid.description), owner" : "\(user.name), \(user.jid.description)"))
        .accessibilityHint(Text("Opens their profile"))
    }
}

/// Asks for the next page; says so when the last try failed.
struct AdminLoadMore: View {
    let isLoading: Bool
    let failed: Bool
    let action: () -> Void

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            if failed {
                Text("Couldn't load more. Try again.")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            Button(isLoading ? "Loading…" : "Load more", action: action)
                .buttonStyle(.huddle())
                .disabled(isLoading)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, Theme.Spacing.m)
    }
}
