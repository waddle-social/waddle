import SwiftUI
import WaddleKit

/// Makes a space, or opens one: its name and description, its members and
/// their roles, and deleting it. A change that fails keeps the sheet open
/// and says why.
struct AdminSpaceSheet: View {
    enum Subject: Identifiable, Hashable {
        case new
        case space(AdminSpace)

        var id: String {
            switch self {
            case .new: ""
            case let .space(space): space.id
            }
        }
    }

    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    /// The list the sheet was opened from, kept in step with what is done.
    private let spaces: AdminList<AdminSpace>
    /// The space as it was when opened; `current` follows the list.
    private let space: AdminSpace?
    @State private var name: String
    @State private var description: String
    @State private var isBusy = false
    @State private var errorMessage: String?
    @State private var deleteError: String?
    @State private var members: AdminList<AdminSpaceMember>?
    /// Members whose role is on its way to the server.
    @State private var changing: Set<BareJID> = []
    @State private var memberError: String?
    @State private var pendingRemoval: AdminSpaceMember?
    @State private var confirmsDelete = false

    init(subject: Subject, spaces: AdminList<AdminSpace>) {
        self.spaces = spaces
        if case let .space(space) = subject {
            self.space = space
        } else {
            space = nil
        }
        _name = State(initialValue: space?.name ?? "")
        _description = State(initialValue: space?.description ?? "")
    }

    var body: some View {
        NavigationStack {
            Form {
                details
                if let space {
                    membersSection
                    deleteSection(space)
                }
            }
            .formStyle(.grouped)
            .disabled(isBusy)
            .navigationTitle(space == nil ? "New space" : "Space")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { toolbar }
        }
        .interactiveDismissDisabled(isWaiting)
        .task {
            guard let space, members == nil else { return }
            let members = session.adminSpaceMembersList(of: space)
            self.members = members
            await members.load()
        }
        .confirmationDialog(
            "Delete space?",
            isPresented: $confirmsDelete,
            titleVisibility: .visible,
            presenting: space
        ) { space in
            Button("Delete", role: .destructive) { delete(space) }
            Button("Cancel", role: .cancel) {}
        } message: { space in
            Text(AdminSpaceCopy.deleteWarning(name: space.name, rooms: space.roomCount))
        }
        .confirmationDialog(
            "Remove from space?",
            isPresented: removalBinding,
            titleVisibility: .visible,
            presenting: pendingRemoval
        ) { member in
            Button("Remove", role: .destructive) { setRole(.none, of: member) }
            Button("Cancel", role: .cancel) {}
        } message: { member in
            Text("\(member.jid.description) loses their role in \(space?.name ?? "this space").")
        }
        .managementSheetFrame(minHeight: space == nil ? 320 : 560)
    }

    // MARK: Name and description

    @ViewBuilder
    private var details: some View {
        Section {
            TextField("Name", text: $name, prompt: Text("Space name"))
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
            TextField("Description", text: $description, prompt: Text("What is it for? (optional)"), axis: .vertical)
                .labelsHidden()
                .lineLimit(2...5)
        } header: {
            Text("Description")
        } footer: {
            if space?.description != nil, draft?.description == nil {
                Text("A description can be changed, but not removed yet.")
            }
        }
        if let errorMessage {
            Section {
                ManagementErrorLabel(message: errorMessage)
            }
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button(space == nil ? "Cancel" : "Close") { dismiss() }
                .disabled(isWaiting)
        }
        ToolbarItem(placement: .confirmationAction) {
            if isBusy {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel(Text("Working"))
            } else {
                Button(space == nil ? "Create" : "Save", action: save)
                    .disabled(!canSave)
            }
        }
    }

    private var draft: AdminSpaceDraft? {
        AdminSpaceDraft(name: name, description: description)
    }

    /// The space as the list now holds it: a save or a removal here
    /// changes its row.
    private var current: AdminSpace? {
        guard let space else { return nil }
        return spaces.rows.first { $0.id == space.id } ?? space
    }

    /// Something is on its way to the server, so the sheet stays to show
    /// how it went.
    private var isWaiting: Bool { isBusy || !changing.isEmpty }

    /// A space that is open has something to save once it differs from
    /// what the server holds. A cleared description is not a difference:
    /// the server would keep the old one.
    private var canSave: Bool {
        guard let draft else { return false }
        guard let space = current else { return true }
        return draft.name != space.name || (draft.description != nil && draft.description != space.description)
    }

    private func save() {
        guard let draft, canSave, !isWaiting else { return }
        isBusy = true
        errorMessage = nil
        Task {
            do {
                if let space = current {
                    spaces.replace(try await session.updateAdminSpace(space, to: draft))
                } else {
                    try await session.createAdminSpace(draft)
                    await spaces.reload()
                }
                dismiss()
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't save the space. Try again.")
            }
            isBusy = false
        }
    }

    // MARK: Members

    @ViewBuilder
    private var membersSection: some View {
        Section {
            if let memberError {
                ManagementErrorLabel(message: memberError)
            }
            if let members {
                membersRows(members)
            } else {
                Text("Loading members…")
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        } header: {
            Text("Members")
        }
    }

    @ViewBuilder
    private func membersRows(_ members: AdminList<AdminSpaceMember>) -> some View {
        if members.failure != nil {
            LabeledContent(members.rows.isEmpty ? "Members didn't load." : "Couldn't refresh the members.") {
                Button("Try again") { Task { await members.reload() } }
                    .disabled(members.isLoading)
            }
        } else if members.rows.isEmpty {
            Text(members.hasLoaded ? "No members yet." : "Loading members…")
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        ForEach(members.rows) { member in
            AdminSpaceMemberRow(member: member, isChanging: changing.contains(member.jid)) { role in
                if role == .none {
                    pendingRemoval = member
                } else {
                    setRole(role, of: member)
                }
            }
        }
        if members.hasMore {
            Button(members.isLoadingMore ? "Loading…" : "Load more") {
                Task { await members.loadMore() }
            }
            .disabled(members.isLoadingMore)
        }
        if members.moreFailure != nil {
            Text("Couldn't load more. Try again.")
                .foregroundStyle(Theme.Colors.fgMuted)
        }
    }

    private var removalBinding: Binding<Bool> {
        Binding {
            pendingRemoval != nil
        } set: {
            if !$0 { pendingRemoval = nil }
        }
    }

    /// Shows the role the server answers with; someone given no role is
    /// no longer a member.
    private func setRole(_ role: AdminSpaceRole, of member: AdminSpaceMember) {
        guard let space, let members, role != member.role, !changing.contains(member.jid) else { return }
        changing.insert(member.jid)
        memberError = nil
        Task {
            do {
                let changed = try await session.setAdminSpaceRole(role, of: member.jid, in: space)
                if changed.role == .none {
                    members.remove(changed.id)
                    if let current { spaces.replace(current.removingMembers(1)) }
                } else {
                    members.replace(changed)
                }
            } catch {
                memberError = ActionErrorCopy.message(
                    for: error,
                    fallback: "Couldn't change the role of \(member.jid.description). Try again."
                )
            }
            changing.remove(member.jid)
        }
    }

    // MARK: Deleting

    private func deleteSection(_ space: AdminSpace) -> some View {
        Section {
            Button("Delete space", role: .destructive) { confirmsDelete = true }
                .foregroundStyle(Theme.Colors.dangerText)
                .disabled(!changing.isEmpty)
            if let deleteError {
                ManagementErrorLabel(message: deleteError)
            }
        } footer: {
            Text(AdminSpaceCopy.deleteWarning(name: space.name, rooms: space.roomCount))
        }
    }

    private func delete(_ space: AdminSpace) {
        guard !isWaiting else { return }
        isBusy = true
        deleteError = nil
        Task {
            do {
                try await session.deleteAdminSpace(space)
                spaces.remove(space.id)
                dismiss()
            } catch {
                deleteError = ActionErrorCopy.message(for: error, fallback: "Couldn't delete the space. Try again.")
            }
            isBusy = false
        }
    }
}

/// Someone in the space, and a menu to change their role or remove them.
private struct AdminSpaceMemberRow: View {
    let member: AdminSpaceMember
    let isChanging: Bool
    let choose: (AdminSpaceRole) -> Void

    var body: some View {
        LabeledContent {
            Menu(AdminSpaceCopy.title(member.role)) {
                ForEach([AdminSpaceRole.owner, .admin, .member], id: \.self) { role in
                    Toggle(AdminSpaceCopy.title(role), isOn: Binding {
                        member.role == role
                    } set: { _ in
                        choose(role)
                    })
                }
                Divider()
                Button("Remove from space", role: .destructive) { choose(.none) }
            }
            .fixedSize()
            .disabled(isChanging)
            .accessibilityLabel(Text("Role of \(member.jid.description)"))
            .accessibilityValue(Text(AdminSpaceCopy.title(member.role)))
        } label: {
            Text(member.jid.description)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }
}
