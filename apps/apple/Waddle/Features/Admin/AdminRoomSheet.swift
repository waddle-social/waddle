import SwiftUI
import WaddleKit

/// Makes a room, or opens one: its settings, who is there, the roles
/// people hold in it, and deleting it. A change that fails keeps the
/// sheet open and says why.
struct AdminRoomSheet: View {
    enum Subject: Identifiable, Hashable {
        case new
        case room(AdminRoom)

        var id: String {
            switch self {
            case .new: ""
            case let .room(room): room.jid.description
            }
        }
    }

    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    /// The list the sheet was opened from, kept in step with what is done.
    private let rooms: AdminList<AdminRoom>
    /// The room as it was when opened; `current` follows the list.
    private let room: AdminRoom?
    @State private var name: String
    @State private var description: String
    @State private var kind: AdminRoomKind
    @State private var isListed: Bool
    @State private var isMembersOnly: Bool
    /// The space a new room goes in, by its id; nil for none.
    @State private var spaceID: AdminSpace.ID?
    @State private var spaces: AdminList<AdminSpace>?
    @State private var isBusy = false
    @State private var errorMessage: String?
    @State private var deleteError: String?
    @State private var occupants: AdminList<AdminRoomOccupant>?
    @State private var members: AdminList<AdminRoomMember>?
    /// People a change about whom is on its way to the server.
    @State private var changing: Set<BareJID> = []
    @State private var peopleError: String?
    /// A role change or a removal waiting for a yes.
    @State private var pending: PendingAction?
    @State private var confirmsDelete = false

    init(subject: Subject, rooms: AdminList<AdminRoom>) {
        self.rooms = rooms
        if case let .room(room) = subject {
            self.room = room
        } else {
            room = nil
        }
        _name = State(initialValue: room?.name ?? "")
        _description = State(initialValue: room?.description ?? "")
        _kind = State(initialValue: room?.kind ?? .text)
        _isListed = State(initialValue: room?.isListed ?? true)
        _isMembersOnly = State(initialValue: room?.isMembersOnly ?? false)
    }

    var body: some View {
        NavigationStack {
            Form {
                details
                access
                if let room {
                    occupantsSection
                    membersSection
                    deleteSection(room)
                }
            }
            .formStyle(.grouped)
            .disabled(isBusy)
            .navigationTitle(room == nil ? "New room" : "Room")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { toolbar }
        }
        .interactiveDismissDisabled(isWaiting)
        .task {
            if let room {
                guard occupants == nil else { return }
                let occupants = session.adminRoomOccupantsList(of: room)
                let members = session.adminRoomMembersList(of: room)
                self.occupants = occupants
                self.members = members
                async let here: Void = occupants.load()
                async let roles: Void = members.load()
                _ = await (here, roles)
            } else if spaces == nil {
                let spaces = session.adminSpacesList()
                self.spaces = spaces
                await spaces.load()
                // Every space can be chosen, not only the first page.
                while spaces.hasMore, spaces.moreFailure == nil, !Task.isCancelled {
                    await spaces.loadMore()
                }
            }
        }
        .confirmationDialog(
            "Delete room?",
            isPresented: $confirmsDelete,
            titleVisibility: .visible,
            presenting: room
        ) { room in
            Button("Delete", role: .destructive) { delete(room) }
            Button("Cancel", role: .cancel) {
                // Nothing to undo: the dialog closes itself.
            }
        } message: { room in
            Text(Self.deleteWarning(name: room.name))
        }
        .confirmationDialog(
            pending?.title ?? "",
            isPresented: pendingBinding,
            titleVisibility: .visible,
            presenting: pending
        ) { action in
            Button(action.confirmTitle, role: .destructive) { perform(action) }
            Button("Cancel", role: .cancel) {
                // Nothing to undo: the dialog closes itself.
            }
        } message: { action in
            Text(action.message(roomName: room?.name ?? "this room"))
        }
        .managementSheetFrame(minHeight: room == nil ? 440 : 640)
    }

    /// What is asked about before it is done, because it cannot be put
    /// back from here.
    enum PendingAction: Equatable {
        /// Leaves someone with no role; one cannot be given from here.
        case removeRole(AdminRoomMember)
        /// Also takes them out of the room.
        case ban(AdminRoomMember)
        /// In a room for members only, also ends their membership.
        case removeMember(AdminRoomOccupant)

        var title: String {
            switch self {
            case .removeRole: "Remove role?"
            case .ban: "Ban from room?"
            case .removeMember: "Remove from room?"
            }
        }

        var confirmTitle: String {
            switch self {
            case .removeRole, .removeMember: "Remove"
            case .ban: "Ban"
            }
        }

        func message(roomName: String) -> String {
            switch self {
            case let .removeRole(member):
                "\(member.jid.description) will have no role in \(roomName). A role can't be given back from here yet."
            case let .ban(member):
                "\(member.jid.description) is taken out of \(roomName) and can't join it until the ban is changed."
            case let .removeMember(occupant):
                "\(roomName) is for members only, so \(occupant.nick) also stops being a member and can't come back. A role can't be given back from here yet."
            }
        }
    }

    private var pendingBinding: Binding<Bool> {
        Binding {
            pending != nil
        } set: {
            if !$0 { pending = nil }
        }
    }

    private func perform(_ action: PendingAction) {
        switch action {
        case let .removeRole(member): setAffiliation(.none, of: member)
        case let .ban(member): setAffiliation(.outcast, of: member)
        case let .removeMember(occupant): remove(occupant)
        }
    }

    static func deleteWarning(name: String) -> String {
        "Everyone in \(name) is removed and its messages may be lost. This can't be undone."
    }

    // MARK: Settings

    @ViewBuilder
    private var details: some View {
        Section {
            TextField("Name", text: $name, prompt: Text("Room name"))
                .labelsHidden()
                .onSubmit(save)
        } header: {
            Text("Name")
        } footer: {
            if AdminSpaceDraft.isNameTooLong(name) {
                Text("A name can be up to \(AdminSpaceDraft.maxNameLength) characters.")
                    .foregroundStyle(Theme.Colors.dangerText)
            }
        }
        Section {
            TextField("Description", text: $description, prompt: Text("What is it about? (optional)"), axis: .vertical)
                .labelsHidden()
                .lineLimit(2...5)
        } header: {
            Text("Description")
        } footer: {
            if room?.description != nil, draft?.description == nil {
                Text("A description can be changed, but not removed yet.")
            }
        }
    }

    @ViewBuilder
    private var access: some View {
        Section {
            if kind.canBeChosen {
                Picker("Kind", selection: $kind) {
                    ForEach(AdminRoomKind.choices, id: \.self) { kind in
                        Text(AdminRoomCopy.title(kind)).tag(kind)
                    }
                }
            } else {
                LabeledContent("Kind", value: AdminRoomCopy.title(kind))
            }
            if room == nil {
                spacePicker
            }
            Toggle("People can find it", isOn: $isListed)
                .onChange(of: isListed) { _, isListed in
                    // Hiding a room suggests keeping it to its members;
                    // the next toggle can say otherwise.
                    if !isListed { isMembersOnly = true }
                }
            Toggle("Only members can join", isOn: $isMembersOnly)
        } footer: {
            if !isListed, !isMembersOnly {
                Text("Anyone who has the room's address can join it.")
            }
        }
        if let errorMessage {
            Section {
                ManagementErrorLabel(message: errorMessage)
            }
        }
    }

    /// The space a new room goes in. Create waits until the spaces have
    /// loaded or failed, so a room is not made outside a space by accident.
    @ViewBuilder
    private var spacePicker: some View {
        if let spaces, spaces.hasLoaded, !spaces.hasMore || spaces.moreFailure != nil {
            if !spaces.rows.isEmpty {
                Picker("Space", selection: $spaceID) {
                    Text("None").tag(AdminSpace.ID?.none)
                    ForEach(spaces.rows) { space in
                        Text(space.name).tag(AdminSpace.ID?.some(space.id))
                    }
                }
            }
        } else if spaces?.failure != nil {
            LabeledContent("Spaces didn't load") {
                Button("Try again") { Task { await spaces?.reload() } }
            }
        } else {
            LabeledContent("Space", value: "Loading…")
        }
    }

    private var spacesAreSettled: Bool {
        guard let spaces else { return false }
        return spaces.failure != nil || (spaces.hasLoaded && (!spaces.hasMore || spaces.moreFailure != nil))
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button(room == nil ? "Cancel" : "Close") { dismiss() }
                .disabled(isWaiting)
        }
        ToolbarItem(placement: .confirmationAction) {
            if isBusy {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel(Text("Working"))
            } else {
                Button(room == nil ? "Create" : "Save", action: save)
                    .disabled(!canSave || !changing.isEmpty)
            }
        }
    }

    private var draft: AdminRoomDraft? {
        AdminRoomDraft(
            name: name, description: description, kind: kind,
            isListed: isListed, isMembersOnly: isMembersOnly,
            space: spaces?.rows.first { $0.id == spaceID }
        )
    }

    /// The room as the list now holds it: a save or a removal here
    /// changes its row.
    private var current: AdminRoom? {
        guard let room else { return nil }
        return rooms.rows.first { $0.id == room.id } ?? room
    }

    /// Something is on its way to the server, so the sheet stays to show
    /// how it went.
    private var isWaiting: Bool { isBusy || !changing.isEmpty }

    /// A room that is open has something to save once it differs from
    /// what the server holds. A cleared description is not a difference:
    /// the server would keep the old one.
    private var canSave: Bool {
        guard let draft else { return false }
        guard let room = current else { return spacesAreSettled }
        return draft.name != room.name
            || (draft.description != nil && draft.description != room.description)
            || draft.kind != room.kind
            || draft.isListed != room.isListed
            || draft.isMembersOnly != room.isMembersOnly
    }

    private func save() {
        guard let draft, canSave, !isWaiting else { return }
        isBusy = true
        errorMessage = nil
        Task {
            do {
                if let room = current {
                    rooms.replace(try await session.updateAdminRoom(room, to: draft))
                } else {
                    try await session.createAdminRoom(draft)
                    await rooms.reload()
                }
                dismiss()
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't save the room. Try again.")
            }
            isBusy = false
        }
    }

    // MARK: Who is there

    @ViewBuilder
    private var occupantsSection: some View {
        Section {
            if let peopleError {
                ManagementErrorLabel(message: peopleError)
            }
            if let occupants {
                AdminSheetList(list: occupants, noun: "people", emptyText: "No one is here right now.") { occupant in
                    AdminRoomOccupantRow(occupant: occupant, isRemoving: changing.contains(occupant.jid)) {
                        // Someone removed can come straight back, except
                        // to a room for members only.
                        if current?.isMembersOnly == true, occupant.affiliation == .member {
                            pending = .removeMember(occupant)
                        } else {
                            remove(occupant)
                        }
                    }
                }
            } else {
                Text("Loading…")
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        } header: {
            Text("Here now")
        }
    }

    /// The server may end a membership with the removal, so the roles are
    /// asked for again.
    private func remove(_ occupant: AdminRoomOccupant) {
        guard let room = current, let occupants, changing.insert(occupant.jid).inserted else { return }
        peopleError = nil
        Task {
            do {
                try await session.removeFromAdminRoom(occupant.jid, room: room)
                // Every device they were in the room from is out.
                for row in occupants.rows where row.jid == occupant.jid { occupants.remove(row.id) }
                await members?.reload()
            } catch {
                peopleError = ActionErrorCopy.message(
                    for: error, fallback: "Couldn't remove \(occupant.nick). Try again."
                )
            }
            changing.remove(occupant.jid)
        }
    }

    // MARK: Roles

    @ViewBuilder
    private var membersSection: some View {
        Section {
            if let members {
                AdminSheetList(list: members, noun: "roles", emptyText: "No one has a role here yet.") { member in
                    AdminRoleRow(
                        address: member.jid.description,
                        note: member.reason,
                        role: member.affiliation,
                        choices: [RoomAffiliation.owner, .admin, .member, .outcast],
                        title: AdminRoomCopy.title,
                        removeTitle: "Remove role",
                        isChanging: changing.contains(member.jid),
                        choose: { affiliation in
                            if affiliation == .outcast {
                                pending = .ban(member)
                            } else {
                                setAffiliation(affiliation, of: member)
                            }
                        },
                        remove: { pending = .removeRole(member) }
                    )
                }
            } else {
                Text("Loading…")
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        } header: {
            Text("Roles")
        } footer: {
            Text("Someone banned can't join the room.")
        }
    }

    /// Shows the role the server answers with; someone left with no role
    /// is no longer listed. A role shows on the people in the room, and a
    /// ban takes them out, so who is there is asked for again.
    private func setAffiliation(_ affiliation: RoomAffiliation, of member: AdminRoomMember) {
        guard let room = current, let members, affiliation != member.affiliation,
              changing.insert(member.jid).inserted else { return }
        peopleError = nil
        Task {
            do {
                let changed = try await session.setAdminRoomAffiliation(affiliation, of: member.jid, in: room)
                if changed.affiliation == .none {
                    members.remove(changed.id)
                } else {
                    members.replace(changed)
                }
                await occupants?.reload()
            } catch {
                peopleError = ActionErrorCopy.message(
                    for: error,
                    fallback: "Couldn't change the role of \(member.jid.description). Try again."
                )
            }
            changing.remove(member.jid)
        }
    }

    // MARK: Deleting

    private func deleteSection(_ room: AdminRoom) -> some View {
        Section {
            Button("Delete room", role: .destructive) { confirmsDelete = true }
                .foregroundStyle(Theme.Colors.dangerText)
                .disabled(!changing.isEmpty)
            if let deleteError {
                ManagementErrorLabel(message: deleteError)
            }
        } footer: {
            Text(Self.deleteWarning(name: room.name))
        }
    }

    private func delete(_ room: AdminRoom) {
        guard !isWaiting else { return }
        isBusy = true
        deleteError = nil
        Task {
            do {
                try await session.deleteAdminRoom(room)
                rooms.remove(room.id)
                dismiss()
            } catch {
                deleteError = ActionErrorCopy.message(for: error, fallback: "Couldn't delete the room. Try again.")
            }
            isBusy = false
        }
    }
}

/// A list inside a sheet's form: its rows, what it says while loading,
/// when empty or when it failed, and a way to load more.
///
/// It is one row of the form whatever it holds. A grouped form on macOS
/// draws its cards wrongly when the number of rows in a section changes
/// after a load, so the rows are stacked here.
struct AdminSheetList<Row: Identifiable & Sendable, RowView: View>: View {
    let list: AdminList<Row>
    /// What the rows are, for "Couldn't refresh the …".
    let noun: String
    let emptyText: String
    @ViewBuilder let row: (Row) -> RowView

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            if list.failure != nil {
                HStack {
                    Text(list.rows.isEmpty ? "Couldn't load the \(noun)." : "Couldn't refresh the \(noun).")
                    Spacer(minLength: Theme.Spacing.xs)
                    Button("Try again") { Task { await list.reload() } }
                        .disabled(list.isLoading)
                }
            } else if list.rows.isEmpty {
                Text(list.hasLoaded ? emptyText : "Loading…")
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            ForEach(Array(list.rows.enumerated()), id: \.element.id) { index, item in
                if index > 0 || list.failure != nil { Divider() }
                row(item)
            }
            if list.hasMore {
                Button(list.isLoadingMore ? "Loading…" : "Load more") {
                    Task { await list.loadMore() }
                }
                .disabled(list.isLoadingMore)
            }
            if list.moreFailure != nil {
                Text("Couldn't load more. Try again.")
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Someone in the room now, with their role if they hold one.
private struct AdminRoomOccupantRow: View {
    let occupant: AdminRoomOccupant
    let isRemoving: Bool
    let remove: () -> Void

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(occupant.nick)
                    .lineLimit(1)
                Text(occupant.jid.description)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(spoken))
            Spacer(minLength: Theme.Spacing.xs)
            if occupant.affiliation != .none {
                HuddleTag(AdminRoomCopy.title(occupant.affiliation))
                    .accessibilityHidden(true)
            }
            Button("Remove", action: remove)
                .disabled(isRemoving)
                .accessibilityLabel(Text("Remove \(occupant.nick) from the room"))
        }
    }

    private var spoken: String {
        let role = occupant.affiliation == .none ? nil : AdminRoomCopy.title(occupant.affiliation)
        return [occupant.nick, occupant.jid.description, role].compactMap { $0 }.joined(separator: ", ")
    }
}
