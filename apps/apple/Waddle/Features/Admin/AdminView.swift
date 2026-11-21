import SwiftUI
import WaddleKit

/// The admin console, for community owners: everyone with an account on
/// this server. The server checks every request, so someone who is not an
/// owner sees "Admin access only" here.
///
/// iPad and Mac show it from the sidebar; iPhone pushes it from You.
struct AdminView: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var users: AdminList<AdminUser>?
    @State private var query = ""

    var body: some View {
        Group {
            if let users {
                AdminUsersPanel(users: users, query: query)
            } else {
                Theme.Colors.bg
            }
        }
        .background(Theme.Colors.bg)
        .navigationTitle("Admin")
        .searchable(text: $query, prompt: "Search users")
        .task(id: Search(query: query, isOnline: session.connection == .online)) {
            let users = users ?? session.adminUsersList()
            self.users = users
            // Wait for a pause in typing before asking.
            if users.hasLoaded {
                try? await Task.sleep(for: .milliseconds(250))
            }
            guard !Task.isCancelled else { return }
            await users.load(matching: query)
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await users?.reload() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(users?.isLoading ?? true)
                .help("Refresh")
            }
        }
    }

    /// What the list on screen answers: asked again when the text changes
    /// and when the connection comes back.
    private struct Search: Hashable {
        let query: String
        let isOnline: Bool
    }
}

/// The users found, a page at a time, or why there are none to show.
struct AdminUsersPanel: View {
    let users: AdminList<AdminUser>
    let query: String

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                AdminPanelHeader(title: "Users", caption: "Everyone with an account here.")
                if users.failure != nil, !users.rows.isEmpty {
                    AdminNoticeLine(text: "Couldn't refresh. This list may be out of date.")
                }
                ForEach(users.rows) { user in
                    AdminUserRow(user: user)
                }
                if users.hasMore {
                    AdminLoadMore(isLoading: users.isLoadingMore, failed: users.moreFailure != nil) {
                        Task { await users.loadMore() }
                    }
                }
            }
            .padding(Theme.Spacing.l)
            .frame(maxWidth: Self.readableWidth)
            .frame(maxWidth: .infinity)
        }
        .refreshable { await users.reload() }
        .overlay {
            if let notice = AdminListNotice(
                isEmpty: users.rows.isEmpty,
                hasLoaded: users.hasLoaded,
                failure: users.failure,
                query: query
            ) {
                AdminNoticeView(notice: notice, query: query)
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
    /// never empty: "no one" is only said once the server has said so.
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

private struct AdminNoticeView: View {
    let notice: AdminListNotice
    let query: String

    var body: some View {
        switch notice {
        case .loading:
            EmptyStateView(title: "Loading users", message: "Everyone with an account shows up here.", symbol: "person.2")
        case .notAllowed:
            EmptyStateView(
                title: "Admin access only",
                message: "Ask a community owner if you need to manage this server.",
                symbol: "lock"
            )
        case .offline:
            EmptyStateView(title: "You're offline", message: "Users show up here when you're connected.", symbol: "person.2")
        case .failed:
            EmptyStateView(title: "Users didn't load", message: "Refresh to try again.", symbol: "exclamationmark.triangle")
        case .noMatches:
            NoMatchesView(query: query)
        case .empty:
            EmptyStateView(title: "No users yet", message: "People show up here once they have an account.", symbol: "person.2")
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
private struct AdminLoadMore: View {
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
