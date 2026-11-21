import SwiftUI
import WaddleKit

/// The lists of the admin console.
enum AdminSection: Hashable, CaseIterable {
    case users
    case spaces
    case rooms

    var copy: AdminListCopy {
        switch self {
        case .users: .users
        case .spaces: .spaces
        case .rooms: .rooms
        }
    }
}

/// The lists behind the console, kept while it is open so a section shown
/// again still has the pages it loaded.
@MainActor
@Observable
final class AdminConsole {
    let users: AdminList<AdminUser>
    let spaces: AdminList<AdminSpace>
    let rooms: AdminList<AdminRoom>
    /// Every space, to narrow the rooms to one.
    let spaceChoices: AdminList<AdminSpace>
    /// The space the rooms are narrowed to; nil for every room.
    var roomsSpace: AdminSpace?
    /// Lists that may be out of date: not asked for since the connection
    /// last came back, or since a sheet of another list was last open.
    private(set) var stale: Set<AdminSection> = []

    init(session: SessionCoordinator) {
        users = session.adminUsersList()
        spaces = session.adminSpacesList()
        rooms = session.adminRoomsList()
        spaceChoices = session.adminSpacesList()
    }

    func markAllStale() {
        stale = Set(AdminSection.allCases)
    }

    /// A space counts its rooms and takes them with it when deleted, so
    /// what is done in one list's sheet can change the others.
    func markOthersStale(than section: AdminSection) {
        stale.formUnion(AdminSection.allCases.filter { $0 != section })
    }

    func markFresh(_ section: AdminSection) {
        stale.remove(section)
    }
}

/// The admin console, for community owners: everyone with an account on
/// this server, its spaces and its rooms. The server checks every
/// request, so someone who is not an owner sees "Admin access only" here.
///
/// iPad and Mac show it from the sidebar; iPhone pushes it from You.
struct AdminView: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var section: AdminSection
    @State private var console: AdminConsole?
    @State private var query = ""

    /// `console` is for previews and tests; the view makes its own.
    init(section: AdminSection = .users, console: AdminConsole? = nil) {
        _section = State(initialValue: section)
        _console = State(initialValue: console)
    }

    var body: some View {
        VStack(spacing: 0) {
            HuddleSegmentedControl(
                selection: shownSection,
                segments: AdminSection.allCases.map { .init(value: $0, title: $0.copy.title) }
            )
            .padding(.vertical, Theme.Spacing.s)
            if let console {
                sectionView(console)
            } else {
                Theme.Colors.bg
            }
        }
        .background(Theme.Colors.bg)
        .navigationTitle("Admin")
        .searchable(text: $query, prompt: section.copy.searchPrompt)
        .task {
            if console == nil { console = AdminConsole(session: session) }
        }
        .onChange(of: session.connection == .online) { _, isOnline in
            if isOnline { console?.markAllStale() }
        }
    }

    /// The one place that knows what each section lists, how a row of it
    /// looks and what opens from it.
    @ViewBuilder
    private func sectionView(_ console: AdminConsole) -> some View {
        switch section {
        case .users:
            AdminSectionView(section: .users, list: console.users, console: console, query: query) { user, _ in
                AdminUserRow(user: user)
            } sheet: { _ in
                EmptyView()
            }
        case .spaces:
            AdminSectionView(section: .spaces, list: console.spaces, console: console, query: query) { space, open in
                AdminSpaceRow(space: space, action: open)
            } sheet: { subject in
                AdminSpaceSheet(subject: subject, spaces: console.spaces)
            }
        case .rooms:
            AdminSectionView(
                section: .rooms, list: console.rooms, console: console, query: query, space: console.roomsSpace
            ) { room, open in
                AdminRoomRow(room: room, action: open)
            } sheet: { subject in
                AdminRoomSheet(subject: subject, rooms: console.rooms)
            } accessory: {
                AdminSpaceFilter(console: console)
            }
        }
    }

    /// Text typed for one list is not a search of another.
    private var shownSection: Binding<AdminSection> {
        Binding {
            section
        } set: {
            guard $0 != section else { return }
            query = ""
            section = $0
        }
    }
}

/// One section of the console: its list, searched by the text typed and
/// narrowed to a space where one is chosen, with the toolbar's refresh
/// and "new" buttons and the sheet a row opens.
struct AdminSectionView<Row: Identifiable & Sendable, RowView: View, Sheet: View, Accessory: View>: View {
    @Environment(SessionCoordinator.self) private var session
    let section: AdminSection
    let list: AdminList<Row>
    let console: AdminConsole
    let query: String
    var space: AdminSpace?
    /// A row, and what choosing it does.
    @ViewBuilder let row: (Row, _ open: @escaping () -> Void) -> RowView
    @ViewBuilder let sheet: (AdminSheetSubject<Row>) -> Sheet
    /// Beside the list's name: what narrows it.
    @ViewBuilder let accessory: () -> Accessory
    @State private var subject: AdminSheetSubject<Row>?

    var body: some View {
        AdminListPanel(list: list, copy: section.copy, accessory: accessory) { item in
            row(item) { subject = .existing(item) }
        }
        .task(id: Search(query: query, space: space?.id, isOnline: session.connection == .online)) {
            await search()
        }
        .toolbar {
            if let newTitle = section.copy.newTitle {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        subject = .new
                    } label: {
                        Label(newTitle, systemImage: "plus")
                    }
                    .help(newTitle)
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await list.reload() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(list.isLoading)
                .help("Refresh")
            }
        }
        .sheet(item: $subject, onDismiss: { console.markOthersStale(than: section) }) { subject in
            sheet(subject)
                .carryingCallDock()
        }
    }

    /// Asks the list for the text typed, after a pause in typing; an empty
    /// field or another space asks at once. A list that already answers
    /// the text and space is left as it is, pages and all, when its
    /// section is shown again, unless it may be out of date.
    private func search() async {
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let answers = list.query == text && list.space?.id == space?.id
        if list.hasLoaded, list.failure == nil, answers, !console.stale.contains(section) { return }
        if !text.isEmpty, list.space?.id == space?.id {
            try? await Task.sleep(for: .milliseconds(250))
        }
        guard !Task.isCancelled else { return }
        await list.load(matching: query, in: space)
        if !Task.isCancelled { console.markFresh(section) }
    }

    /// What the list on screen answers: asked again when the text or the
    /// space changes and when the connection comes back.
    private struct Search: Hashable {
        let query: String
        let space: AdminSpace.ID?
        let isOnline: Bool
    }
}

extension AdminSectionView where Accessory == EmptyView {
    /// A section whose list is not narrowed.
    init(
        section: AdminSection, list: AdminList<Row>, console: AdminConsole, query: String,
        @ViewBuilder row: @escaping (Row, _ open: @escaping () -> Void) -> RowView,
        @ViewBuilder sheet: @escaping (AdminSheetSubject<Row>) -> Sheet
    ) {
        self.init(section: section, list: list, console: console, query: query, row: row, sheet: sheet) {
            EmptyView()
        }
    }
}

/// Narrows the rooms to one space. The spaces are asked for again, every
/// page, each time the rooms are shown; a space no longer there stops
/// narrowing them.
struct AdminSpaceFilter: View {
    @Bindable var console: AdminConsole

    var body: some View {
        // Kept while hidden, so the spaces are still asked for.
        VStack {
            // A community without spaces has nothing to narrow to.
            if !choices.isEmpty {
                Picker("Space", selection: chosen) {
                    Text("All spaces").tag(AdminSpace.ID?.none)
                    ForEach(choices) { space in
                        Text(space.name).tag(AdminSpace.ID?.some(space.id))
                    }
                }
                .pickerStyle(.menu)
                .labelsHidden()
                .fixedSize()
                .accessibilityLabel(Text("Space"))
            }
        }
        .task {
            let spaces = console.spaceChoices
            await spaces.loadEveryPage()
            if spaces.isComplete, let space = console.roomsSpace, !spaces.rows.contains(where: { $0.id == space.id }) {
                console.roomsSpace = nil
            }
        }
    }

    /// Every space loaded, and the one chosen while it is not among them.
    private var choices: [AdminSpace] {
        let rows = console.spaceChoices.rows
        guard let space = console.roomsSpace, !rows.contains(where: { $0.id == space.id }) else { return rows }
        return [space] + rows
    }

    private var chosen: Binding<AdminSpace.ID?> {
        Binding {
            console.roomsSpace?.id
        } set: { id in
            console.roomsSpace = choices.first { $0.id == id }
        }
    }
}

/// One list of the console: its name, its rows a page at a time, or why
/// there are none to show.
struct AdminListPanel<Row: Identifiable & Sendable, RowView: View, Accessory: View>: View {
    let list: AdminList<Row>
    let copy: AdminListCopy
    @ViewBuilder let accessory: () -> Accessory
    @ViewBuilder let row: (Row) -> RowView

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                HStack(alignment: .top, spacing: Theme.Spacing.m) {
                    AdminPanelHeader(title: copy.title, caption: caption)
                    Spacer(minLength: 0)
                    accessory()
                        .padding(.trailing, Theme.Spacing.m)
                }
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
                query: list.query,
                isInSpace: list.space != nil
            ) {
                AdminNoticeView(notice: notice, query: list.query, space: list.space?.name, copy: copy)
            }
        }
    }

    /// What the rows answer: the space they are narrowed to, if any.
    private var caption: String {
        guard let space = list.space, let inSpace = copy.captionInSpace else { return copy.caption }
        return inSpace(space.name)
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
    /// and `query` is the text it answered, not the text being typed. A
    /// list narrowed to a space has no matches there, not none at all.
    init?(isEmpty: Bool, hasLoaded: Bool, failure: PortError?, query: String, isInSpace: Bool = false) {
        guard isEmpty else { return nil }
        switch failure {
        case .rejected: self = .notAllowed
        case .notConnected: self = .offline
        case .some: self = .failed
        case nil:
            if !hasLoaded {
                self = .loading
            } else {
                let isSearch = !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                self = isSearch || isInSpace ? .noMatches : .empty
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
    /// Why nothing answers the text and, for rooms, the space.
    let noMatches: (_ query: String, _ space: String?) -> String
    let emptyTitle: String
    let emptyMessage: String
    let searchPrompt: String
    /// The toolbar's "new" button; nil for a list nothing is added to.
    var newTitle: String?
    /// The caption of a list narrowed to a space, by its name.
    var captionInSpace: ((String) -> String)?

    static let users = AdminListCopy(
        title: "Users",
        caption: "Everyone with an account here.",
        symbol: "person.2",
        loadingTitle: "Loading users",
        offlineMessage: "Users show up here when you're connected.",
        failedTitle: "Users didn't load",
        noMatches: { query, _ in "No one's name or address starts with “\(query)”." },
        emptyTitle: "No users yet",
        emptyMessage: "People show up here once they have an account.",
        searchPrompt: "Search users"
    )

    static let rooms = AdminListCopy(
        title: "Rooms",
        caption: "Every room in this community.",
        symbol: "number",
        loadingTitle: "Loading rooms",
        offlineMessage: "Rooms show up here when you're connected.",
        failedTitle: "Rooms didn't load",
        noMatches: { query, space in
            switch (query.isEmpty, space) {
            case let (true, space?): "No rooms in \(space) yet."
            case let (false, space?): "No room in \(space) has a name that starts with “\(query)”."
            case (_, nil): "No room's name starts with “\(query)”."
            }
        },
        emptyTitle: "No rooms yet",
        emptyMessage: "Make one to get people talking.",
        searchPrompt: "Search rooms",
        newTitle: "New room",
        captionInSpace: { "The rooms in \($0)." }
    )

    static let spaces = AdminListCopy(
        title: "Spaces",
        caption: "Groups of rooms in this community.",
        symbol: "square.grid.2x2",
        loadingTitle: "Loading spaces",
        offlineMessage: "Spaces show up here when you're connected.",
        failedTitle: "Spaces didn't load",
        noMatches: { query, _ in "No space's name starts with “\(query)”." },
        emptyTitle: "No spaces yet",
        emptyMessage: "Make one to group rooms together.",
        searchPrompt: "Search spaces",
        newTitle: "New space"
    )
}

private struct AdminNoticeView: View {
    let notice: AdminListNotice
    let query: String
    let space: String?
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
            EmptyStateView(title: "No matches", message: copy.noMatches(query, space), symbol: "magnifyingglass")
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
struct AdminUserRow: View {
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
