import SwiftUI
import WaddleKit

/// Makes a space, or opens one: its name and description, its members and
/// their roles, and deleting it.
struct AdminSpaceSheet: View {
    @Environment(SessionCoordinator.self) private var session
    /// The list the sheet was opened from, kept in step with what is done.
    private let spaces: AdminList<AdminSpace>
    /// The space as it was when opened; `current` follows the list.
    private let opened: AdminSpace?
    @State private var name: String
    @State private var description: String
    @State private var members: AdminList<AdminSpaceMember>?
    /// Members whose role is on its way to the server.
    @State private var changing: Set<BareJID> = []
    @State private var memberError: String?
    @State private var pendingRemoval: AdminSpaceMember?
    @State private var isAdding = false

    init(subject: AdminSheetSubject<AdminSpace>, spaces: AdminList<AdminSpace>) {
        self.spaces = spaces
        opened = subject.row
        _name = State(initialValue: opened?.name ?? "")
        _description = State(initialValue: opened?.description ?? "")
    }

    private var copy: AdminSheetScaffold<AdminNameFields, AnyView>.Copy {
        .init(
            newTitle: "New space",
            title: opened?.name ?? "Space",
            saveFailure: "Couldn't save the space. Try again.",
            deleteTitle: "Delete space",
            deleteQuestion: "Delete space?",
            deleteFailure: "Couldn't delete the space. Try again.",
            newMinHeight: 320,
            minHeight: 560,
            createTitle: "Create space",
            saveTitle: "Save changes"
        )
    }

    var body: some View {
        AdminSheetScaffold(
            isNew: opened == nil,
            copy: copy,
            deleteWarning: AdminSpaceCopy.deleteWarning(name: opened?.name ?? "", rooms: opened?.roomCount ?? 0),
            canSave: canSave,
            isChanging: !changing.isEmpty,
            save: save,
            delete: delete
        ) { submit in
            AdminNameFields(
                name: $name,
                description: $description,
                namePrompt: opened == nil ? "Engineering" : "Space name",
                descriptionPrompt: "What's this space for?",
                keepsOldDescription: opened?.description != nil && draft?.description == nil,
                submit: submit,
                descriptionTitle: opened == nil ? "Description (optional)" : "Description"
            )
        } lists: {
            AnyView(membersSection)
        }
        .task {
            guard let opened, members == nil else { return }
            let members = session.adminSpaceMembersList(of: opened)
            self.members = members
            await members.load()
        }
        .confirmationDialog(
            "Remove from space?",
            isPresented: removalBinding,
            titleVisibility: .visible,
            presenting: pendingRemoval
        ) { member in
            Button("Remove", role: .destructive) { setRole(.none, of: member.jid, from: member.role) }
            Button("Cancel", role: .cancel) {
                // Nothing to undo: the dialog closes itself.
            }
        } message: { member in
            Text("\(member.jid.description) loses their role in \(opened?.name ?? "this space").")
        }
        .sheet(isPresented: $isAdding) {
            if let members {
                AdminPersonPicker(
                    title: "Add member",
                    confirmTitle: "Add",
                    roles: Self.roles,
                    role: .member,
                    roleTitle: AdminSpaceCopy.title,
                    holders: members,
                    roleOf: \.role
                ) { user, role in
                    setRole(role, of: user.jid, from: .none)
                }
                .carryingCallDock()
            }
        }
    }

    private static let roles: [AdminSpaceRole] = [.owner, .admin, .member]

    // MARK: Name and description

    private var draft: AdminSpaceDraft? {
        AdminSpaceDraft(name: name, description: description)
    }

    /// The space as the list now holds it: a save or a removal here
    /// changes its row.
    private var current: AdminSpace? {
        opened.map(spaces.current)
    }

    /// A space that is open has something to save once it differs from
    /// what the server holds. A cleared description is not a difference:
    /// the server would keep the old one.
    private var canSave: Bool {
        guard let draft else { return false }
        guard let space = current else { return true }
        return draft.name != space.name || (draft.description != nil && draft.description != space.description)
    }

    private func save() async throws {
        guard let draft else { return }
        if let space = current {
            spaces.replace(try await session.updateAdminSpace(space, to: draft))
        } else {
            try await session.createAdminSpace(draft)
            await spaces.reload()
        }
    }

    private func delete() async throws {
        guard let opened else { return }
        try await session.deleteAdminSpace(opened)
        spaces.remove(opened.id)
    }

    // MARK: Members

    private var membersSection: some View {
        AdminPeopleSection(
            title: "Members", list: members, noun: "members", emptyText: "No members yet.", error: memberError,
            addTitle: "Add member", add: { isAdding = true }
        ) { member in
            AdminRoleRow(
                address: member.jid.description,
                role: member.role,
                choices: Self.roles,
                title: AdminSpaceCopy.title,
                removeTitle: "Remove from space",
                isChanging: changing.contains(member.jid),
                choose: { setRole($0, of: member.jid, from: member.role) },
                remove: { pendingRemoval = member }
            )
        }
    }

    private var removalBinding: Binding<Bool> {
        Binding {
            pendingRemoval != nil
        } set: {
            if !$0 { pendingRemoval = nil }
        }
    }

    /// Shows the role the server answers with: someone given no role is
    /// no longer a member, and someone not listed is added first.
    private func setRole(_ role: AdminSpaceRole, of jid: BareJID, from old: AdminSpaceRole) {
        guard let opened, let members, role != old, changing.insert(jid).inserted else { return }
        memberError = nil
        Task {
            do {
                let changed = try await session.setAdminSpaceRole(role, of: jid, in: opened)
                let wasListed = members.rows.contains { $0.id == changed.id }
                if changed.role == .none {
                    members.remove(changed.id)
                    if wasListed, let current { spaces.replace(current.removingMembers(1)) }
                } else {
                    members.add(changed)
                    if !wasListed, let current { spaces.replace(current.addingMembers(1)) }
                }
            } catch {
                memberError = AdminSpaceCopy.roleFailure(error, address: jid.description)
            }
            changing.remove(jid)
        }
    }
}
