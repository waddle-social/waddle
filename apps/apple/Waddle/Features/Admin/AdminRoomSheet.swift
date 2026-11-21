import SwiftUI
import WaddleKit

/// Makes a room, or opens one: its settings, who is there, the roles
/// people hold in it, and deleting it.
struct AdminRoomSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    /// The list the sheet was opened from, kept in step with what is done.
    private let rooms: AdminList<AdminRoom>
    /// The room as it was when opened; `current` follows the list.
    private let opened: AdminRoom?
    @State private var name: String
    @State private var description: String
    @State private var kind: AdminRoomKind
    @State private var access: AdminRoomAccess
    /// The space a new room goes in, by its id; nil for none.
    @State private var spaceID: AdminSpace.ID?
    @State private var spaces: AdminList<AdminSpace>?
    @State private var occupants: AdminList<AdminRoomOccupant>?
    @State private var members: AdminList<AdminRoomMember>?
    /// People a change about whom is on its way to the server.
    @State private var changing: Set<BareJID> = []
    @State private var peopleError: String?
    /// A role change or a removal waiting for a yes.
    @State private var pending: PendingAction?

    init(subject: AdminSheetSubject<AdminRoom>, rooms: AdminList<AdminRoom>) {
        self.rooms = rooms
        opened = subject.row
        _name = State(initialValue: opened?.name ?? "")
        _description = State(initialValue: opened?.description ?? "")
        _kind = State(initialValue: opened?.kind ?? .text)
        _access = State(initialValue: opened?.access ?? .open)
    }

    private static let copy = AdminSheetScaffold<AnyView, AnyView>.Copy(
        newTitle: "New room",
        title: "Room",
        saveFailure: "Couldn't save the room. Try again.",
        deleteTitle: "Delete room",
        deleteQuestion: "Delete room?",
        deleteFailure: "Couldn't delete the room. Try again.",
        newMinHeight: 440,
        minHeight: 640
    )

    var body: some View {
        AdminSheetScaffold(
            isNew: opened == nil,
            copy: Self.copy,
            deleteWarning: Self.deleteWarning(name: opened?.name ?? ""),
            canSave: canSave,
            isChanging: !changing.isEmpty,
            save: save,
            delete: delete
        ) { submit in
            AnyView(fields(submit: submit))
        } lists: {
            AnyView(people)
        }
        .task {
            if let opened {
                guard occupants == nil else { return }
                let occupants = session.adminRoomOccupantsList(of: opened)
                let members = session.adminRoomMembersList(of: opened)
                self.occupants = occupants
                self.members = members
                async let here: Void = occupants.load()
                async let roles: Void = members.load()
                _ = await (here, roles)
            } else if spaces == nil {
                let spaces = session.adminSpacesList()
                self.spaces = spaces
                await Self.loadEvery(spaces)
            }
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
            Text(action.message(roomName: opened?.name ?? "this room"))
        }
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
    private func fields(submit: @escaping () -> Void) -> some View {
        AdminNameFields(
            name: $name,
            description: $description,
            namePrompt: "Room name",
            descriptionPrompt: "What is it about? (optional)",
            keepsOldDescription: opened?.description != nil && draft?.description == nil,
            submit: submit
        )
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
            if opened == nil {
                spacePicker
            }
            Toggle("People can find it", isOn: $access.isListed)
                .onChange(of: access.isListed) { _, isListed in
                    // Hiding a room suggests keeping it to its members;
                    // the next toggle can say otherwise.
                    if !isListed { access.isMembersOnly = true }
                }
            Toggle("Only members can join", isOn: $access.isMembersOnly)
        } footer: {
            if !access.isListed, !access.isMembersOnly {
                Text("Anyone who has the room's address can join it.")
            }
        }
    }

    /// Every space can be chosen, not only the first page. A page that
    /// fails stops the loading; trying again starts over.
    private static func loadEvery(_ spaces: AdminList<AdminSpace>) async {
        await spaces.reload()
        while spaces.hasMore, spaces.moreFailure == nil, !Task.isCancelled {
            await spaces.loadMore()
        }
    }

    /// The space a new room goes in. Create waits until the spaces have
    /// loaded or failed, so a room is not made outside a space by accident.
    @ViewBuilder
    private var spacePicker: some View {
        if let spaces, spacesAreSettled {
            if !spaces.rows.isEmpty {
                Picker("Space", selection: $spaceID) {
                    Text("None").tag(AdminSpace.ID?.none)
                    ForEach(spaces.rows) { space in
                        Text(space.name).tag(AdminSpace.ID?.some(space.id))
                    }
                }
            }
            if spaces.failure != nil || spaces.moreFailure != nil {
                LabeledContent(spaces.rows.isEmpty ? "Spaces didn't load" : "Some spaces didn't load") {
                    Button("Try again") { Task { await Self.loadEvery(spaces) } }
                }
            }
        } else {
            LabeledContent("Space", value: "Loading…")
        }
    }

    /// Every page of spaces is in, or one could not be had, and none is
    /// on its way.
    private var spacesAreSettled: Bool {
        guard let spaces, !spaces.isLoading, !spaces.isLoadingMore else { return false }
        return spaces.failure != nil || spaces.moreFailure != nil || (spaces.hasLoaded && !spaces.hasMore)
    }

    private var draft: AdminRoomDraft? {
        AdminRoomDraft(
            name: name, description: description, kind: kind, access: access,
            space: spaces?.rows.first { $0.id == spaceID }
        )
    }

    /// The room as the list now holds it: a save here changes its row.
    private var current: AdminRoom? {
        opened.map(rooms.current)
    }

    /// A room that is open has something to save once it differs from
    /// what the server holds. A cleared description is not a difference:
    /// the server would keep the old one.
    private var canSave: Bool {
        guard let draft else { return false }
        guard let room = current else { return spacesAreSettled }
        return draft.name != room.name
            || (draft.description != nil && draft.description != room.description)
            || draft.kind != room.kind
            || draft.access != room.access
    }

    private func save() async throws {
        guard let draft else { return }
        if let room = current {
            rooms.replace(try await session.updateAdminRoom(room, to: draft))
        } else {
            try await session.createAdminRoom(draft)
            await rooms.reload()
        }
    }

    private func delete() async throws {
        guard let opened else { return }
        try await session.deleteAdminRoom(opened)
        rooms.remove(opened.id)
        // Home or Inbox may still have the room open in their own stack.
        navigation.close(.room(opened.jid))
    }

    // MARK: Who is there, and roles

    @ViewBuilder
    private var people: some View {
        AdminPeopleSection(
            title: "Here now", list: occupants, noun: "people",
            emptyText: "No one is here right now.", error: peopleError
        ) { occupant in
            AdminRoomOccupantRow(occupant: occupant, isRemoving: changing.contains(occupant.jid)) {
                // Someone removed can come straight back, except to a
                // room for members only.
                if current?.access.isMembersOnly == true, occupant.affiliation == .member {
                    pending = .removeMember(occupant)
                } else {
                    remove(occupant)
                }
            }
        }
        AdminPeopleSection(
            title: "Roles", footer: "Someone banned can't join the room.", list: members,
            noun: "roles", emptyText: "No one has a role here yet."
        ) { member in
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
    }

    /// The server takes out one of the names they are there under, and
    /// may end a membership with it, so both lists are asked for again.
    private func remove(_ occupant: AdminRoomOccupant) {
        guard let opened, let occupants, changing.insert(occupant.jid).inserted else { return }
        peopleError = nil
        Task {
            do {
                try await session.removeFromAdminRoom(occupant.jid, room: opened)
                async let here: Void = occupants.reload()
                async let roles: Void? = members?.reload()
                _ = await (here, roles)
            } catch {
                peopleError = ActionErrorCopy.message(
                    for: error, fallback: "Couldn't remove \(occupant.nick). Try again."
                )
            }
            changing.remove(occupant.jid)
        }
    }

    /// Shows the role the server answers with; someone left with no role
    /// is no longer listed. A role shows on the people in the room, and a
    /// ban takes them out, so who is there is asked for again.
    private func setAffiliation(_ affiliation: RoomAffiliation, of member: AdminRoomMember) {
        guard let opened, let members, affiliation != member.affiliation,
              changing.insert(member.jid).inserted else { return }
        peopleError = nil
        Task {
            do {
                let changed = try await session.setAdminRoomAffiliation(affiliation, of: member.jid, in: opened)
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
