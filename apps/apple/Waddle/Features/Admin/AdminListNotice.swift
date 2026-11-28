import SwiftUI
import WaddleKit

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
    /// Why nothing answers the text and, for rooms, the space it is in.
    let noMatches: (_ query: String, _ space: String?) -> String
    let emptyTitle: String
    let searchPrompt: String
    /// The toolbar's "new" button; nil for a list nothing is added to.
    var newTitle: String?
    /// The caption of a list narrowed to a space, and what an empty one
    /// says, by the space's name.
    var captionInSpace: ((String) -> String)?
    var emptyInSpace: ((String) -> String)?

    static let users = AdminListCopy(
        title: "Users",
        caption: "Every account on this server.",
        symbol: "person.2",
        loadingTitle: "Loading users",
        offlineMessage: "Users show up here when you're connected.",
        failedTitle: "Users didn't load",
        noMatches: { query, _ in "No users match “\(query)”." },
        emptyTitle: "No users registered yet.",
        searchPrompt: "Search users"
    )

    static let rooms = AdminListCopy(
        title: "Rooms",
        caption: "Open a room to edit its settings and roles, or see who's there.",
        symbol: "number",
        loadingTitle: "Loading rooms",
        offlineMessage: "Rooms show up here when you're connected.",
        failedTitle: "Rooms didn't load",
        noMatches: { query, space in
            space.map { "No rooms in \($0) match “\(query)”." }
                ?? "No rooms match “\(query)”."
        },
        emptyTitle: "No rooms yet. Create one to get started.",
        searchPrompt: "Search rooms",
        newTitle: "New room",
        captionInSpace: { "The rooms in \($0)." },
        emptyInSpace: { "No rooms in \($0) yet." }
    )

    static let spaces = AdminListCopy(
        title: "Spaces",
        caption: "Groups of rooms in this community.",
        symbol: "square.grid.2x2",
        loadingTitle: "Loading spaces",
        offlineMessage: "Spaces show up here when you're connected.",
        failedTitle: "Spaces didn't load",
        noMatches: { query, _ in "No spaces match “\(query)”." },
        emptyTitle: "No spaces yet. Create one to get started.",
        searchPrompt: "Search spaces",
        newTitle: "New space"
    )
}

struct AdminListNoticeView: View {
    let notice: AdminListNotice
    let section: AdminSection
    let query: String
    let space: AdminSpace?

    private var copy: AdminListCopy { section.copy }

    var body: some View {
        switch notice {
        case .loading:
            status("\(copy.loadingTitle)…", isLoading: true)
        case .notAllowed:
            AdminAccessNotice()
        case .offline:
            EmptyStateView(title: "You're offline", message: copy.offlineMessage, symbol: copy.symbol)
        case .failed:
            EmptyStateView(title: copy.failedTitle, message: "Refresh to try again.", symbol: "exclamationmark.triangle")
        case .noMatches:
            status(copy.noMatches(query, space?.name))
        case .empty:
            status(emptyTitle)
        }
    }

    private var emptyTitle: String {
        guard let space, let inSpace = copy.emptyInSpace else { return copy.emptyTitle }
        return inSpace(space.name)
    }

    private func status(_ title: String, isLoading: Bool = false) -> some View {
        HStack(spacing: Theme.Spacing.s) {
            if isLoading {
                ProgressView()
                    .controlSize(.small)
                    .tint(Theme.Colors.action)
                    .accessibilityHidden(true)
            }
            Text(title)
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .multilineTextAlignment(.center)
        .padding(Theme.Spacing.l)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct AdminAccessNotice: View {
    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            Image(systemName: "lock")
                .huddleText(Theme.Typography.headline)
                .foregroundStyle(Theme.Colors.actionText)
                .frame(width: Theme.Size.emptyMark, height: Theme.Size.emptyMark)
                .background(Theme.Colors.actionSubtle, in: RoundedRectangle(cornerRadius: Theme.Radius.large))
                .padding(.bottom, Theme.Spacing.s)
                .accessibilityHidden(true)
            Text("Admin access only")
                .huddleText(Theme.Typography.title)
                .foregroundStyle(Theme.Colors.fg)
                .accessibilityAddTraits(.isHeader)
            Text("Ask a community owner if you need to manage this server.")
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .multilineTextAlignment(.center)
        .padding(Theme.Spacing.xl)
        .frame(maxWidth: 420)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
    }
}
