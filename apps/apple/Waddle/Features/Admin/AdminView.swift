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

    /// The spaces the rooms can be narrowed to: every one loaded, and the
    /// one chosen while it is not among them.
    var spaceFilterChoices: [AdminSpace] {
        let rows = spaceChoices.rows
        guard let space = roomsSpace, !rows.contains(where: { $0.id == space.id }) else { return rows }
        return [space] + rows
    }

    /// Asks for every space again. The space chosen follows its row, and
    /// one no longer there stops narrowing the rooms.
    func refreshSpaceChoices() async {
        let every = await spaceChoices.loadEveryRow()
        guard let chosen = roomsSpace, let every else { return }
        roomsSpace = every.first { $0.id == chosen.id }
    }

    /// What Refresh asks for again: the section's list and, for the rooms,
    /// the spaces they can be narrowed to, which may have failed before.
    func refresh(_ section: AdminSection) async {
        switch section {
        case .users: await users.reload()
        case .spaces: await spaces.reload()
        case .rooms:
            async let roomsReloaded: Void = rooms.reload()
            async let choicesReloaded: Void = refreshSpaceChoices()
            _ = await (roomsReloaded, choicesReloaded)
        }
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
            AdminConsoleHeader(selection: shownSection, caption: caption)
            if let console {
                sectionView(console)
            } else {
                Theme.Colors.bg
            }
        }
        .background(Theme.Colors.bg)
        .navigationTitle("Admin")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(Theme.Colors.bgRail, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        #endif
        .tint(Theme.Colors.action)
        .searchable(text: $query, prompt: section.copy.searchPrompt)
        .task {
            if console == nil { console = AdminConsole(session: session) }
        }
        .onChange(of: session.connection == .online) { _, isOnline in
            if isOnline { console?.markAllStale() }
        }
    }

    private var caption: String {
        let copy = section.copy
        if let inSpace = copy.captionInSpace, let space = console?.rooms.space {
            return inSpace(space.name)
        }
        return copy.caption
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
            // On showing the rooms and on coming back online.
            .task(id: session.connection == .online) { await console.refreshSpaceChoices() }
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
