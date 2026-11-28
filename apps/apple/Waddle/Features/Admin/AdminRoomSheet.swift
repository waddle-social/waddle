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
    /// A role change or a removal waiting for a yes, and why, if said.
    @State private var pending: PendingAction?
    @State private var reason = ""
    @State private var isGivingRole = false
    /// Who was picked to be given a role, and the role, until the picker
    /// has closed: a ban is asked about first.
    @State private var roleToGive: (member: AdminRoomMember, role: RoomAffiliation)?

    init(subject: AdminSheetSubject<AdminRoom>, rooms: AdminList<AdminRoom>) {
        self.rooms = rooms
        opened = subject.row
        _name = State(initialValue: opened?.name ?? "")
        _description = State(initialValue: opened?.description ?? "")
        _kind = State(initialValue: opened?.kind ?? .text)
        _access = State(initialValue: opened?.access ?? .open)
    }

    private var copy: AdminSheetScaffold<AnyView, AnyView>.Copy {
        .init(
            newTitle: "New room",
            title: opened?.name ?? "Room",
            saveFailure: "Couldn't save the room. Try again.",
            deleteTitle: "Delete room",
            deleteQuestion: "Delete room?",
            deleteFailure: "Couldn't delete the room. Try again.",
            newMinHeight: 440,
            minHeight: 640,
            createTitle: "Create room"
        )
    }

    var body: some View {
        AdminSheetScaffold(
            isNew: opened == nil,
            copy: copy,
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
                await spaces.loadEveryPage()
            }
        }
        // An alert, not a confirmation dialog, since only an alert takes
        // a text field.
        .alert(pending?.title ?? "", isPresented: pendingBinding, presenting: pending) { action in
            TextField("Reason", text: $reason, prompt: Text("Reason (optional)"))
            Button(action.confirmTitle, role: .destructive) { perform(action) }
            Button("Cancel", role: .cancel) {
                // Nothing to undo: the alert closes itself.
            }
        } message: { action in
            Text(action.message(roomName: opened?.name ?? "this room"))
        }
        .onChange(of: pending) { _, pending in
            if pending != nil { reason = "" }
        }
        .sheet(isPresented: $isGivingRole, onDismiss: givePickedRole) {
            if let members {
                AdminPersonPicker(
                    title: "Give a role",
                    confirmTitle: "Give",
                    roles: Self.roles,
                    role: .member,
                    roleTitle: AdminRoomCopy.title,
                    holders: members,
                    roleOf: \.affiliation
                ) { user, role in
                    roleToGive = (member(user.jid), role)
                }
                .carryingCallDock()
            }
        }
    }

    /// The roles that can be given, in the order shown.
    private static let roles: [RoomAffiliation] = [.owner, .admin, .member, .outcast]

    /// What is asked about before it is done, because it takes something
    /// away. Each can say why.
    enum PendingAction: Equatable {
        /// Leaves someone with no role.
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
                "\(member.jid.description) will have no role in \(roomName)."
            case let .ban(member):
                "\(member.jid.description) is taken out of \(roomName) and can't join it until the ban is changed."
            case let .removeMember(occupant):
                "\(roomName) is for members only, so \(occupant.nick) also stops being a member and can't come back until they're made one again."
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
        case let .removeRole(member): setAffiliation(.none, of: member, reason: reason)
        case let .ban(member): setAffiliation(.outcast, of: member, reason: reason)
        case let .removeMember(occupant): remove(occupant, reason: reason)
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
            namePrompt: opened == nil ? "general" : "Room name",
            descriptionPrompt: "What's this room for?",
            keepsOldDescription: opened?.description != nil && draft?.description == nil,
            submit: submit,
            descriptionTitle: opened == nil ? "Topic (optional)" : "Topic"
        )
        AdminRoomSettings(
            kind: $kind,
            access: $access,
            spaceSelection: opened == nil ? $spaceID : nil,
            spaces: spaces,
            spacesAreSettled: spacesAreSettled
        )
    }

    /// Every page of spaces is in, or one could not be had, and none is
    /// on its way.
    private var spacesAreSettled: Bool {
        guard let spaces, !spaces.isLoading, !spaces.isLoadingMore else { return false }
        return spaces.failure != nil || spaces.moreFailure != nil || spaces.isComplete
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
            AdminRoomOccupantRow(
                occupant: occupant,
                choices: Self.roles,
                isChanging: changing.contains(occupant.jid),
                choose: { choose($0, for: member(occupant.jid, holding: occupant.affiliation)) },
                removeRole: { pending = .removeRole(member(occupant.jid, holding: occupant.affiliation)) }
            ) {
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
            noun: "roles", emptyText: "No one has a role here yet.",
            addTitle: "Give a role", add: { isGivingRole = true }
        ) { member in
            AdminRoleRow(
                address: member.jid.description,
                note: member.reason,
                role: member.affiliation,
                choices: Self.roles,
                title: AdminRoomCopy.title,
                removeTitle: "Remove role",
                isChanging: changing.contains(member.jid),
                choose: { choose($0, for: member) },
                remove: { pending = .removeRole(member) }
            )
        }
    }

    /// Someone as the roles list holds them; someone it does not list
    /// holds `affiliation`, which is no role unless the room says so.
    private func member(_ jid: BareJID, holding affiliation: RoomAffiliation = .none) -> AdminRoomMember {
        members?.rows.first { $0.jid == jid } ?? AdminRoomMember(jid: jid, affiliation: affiliation, reason: nil)
    }

    /// A ban takes them out of the room, so it is asked about first.
    private func choose(_ affiliation: RoomAffiliation, for member: AdminRoomMember) {
        if affiliation == .outcast {
            pending = .ban(member)
        } else {
            setAffiliation(affiliation, of: member)
        }
    }

    private func givePickedRole() {
        guard let roleToGive else { return }
        self.roleToGive = nil
        choose(roleToGive.role, for: roleToGive.member)
    }

    /// The server takes out one of the names they are there under, and
    /// may end a membership with it, so both lists are asked for again.
    private func remove(_ occupant: AdminRoomOccupant, reason: String? = nil) {
        guard let opened, let occupants, changing.insert(occupant.jid).inserted else { return }
        peopleError = nil
        Task {
            do {
                try await session.removeFromAdminRoom(occupant.jid, room: opened, reason: reason)
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

    /// Shows the role the server answers with: someone left with no role
    /// is no longer listed, and someone not listed is added first. A role
    /// shows on the people in the room, and a ban takes them out, so who
    /// is there is asked for again.
    private func setAffiliation(_ affiliation: RoomAffiliation, of member: AdminRoomMember, reason: String? = nil) {
        guard let opened, let members, affiliation != member.affiliation,
              changing.insert(member.jid).inserted else { return }
        peopleError = nil
        Task {
            do {
                let changed = try await session.setAdminRoomAffiliation(
                    affiliation, of: member.jid, in: opened, reason: reason
                )
                if changed.affiliation == .none {
                    members.remove(changed.id)
                } else {
                    members.add(changed)
                }
                await occupants?.reload()
            } catch {
                peopleError = AdminRoomCopy.roleFailure(error, address: member.jid.description)
            }
            changing.remove(member.jid)
        }
    }
}
