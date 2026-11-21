import SwiftUI
import WaddleKit

/// Picks someone with an account here who holds no role yet, from the
/// console's users list searched the same way, and a role to give them.
/// Someone who holds one is shown with it but cannot be picked: their role
/// is changed from their own row. It hands both back and closes; the sheet
/// that opened it makes the change and says how it went.
struct AdminPersonPicker<Holder: Identifiable & Sendable, Role: Hashable>: View where Holder.ID == BareJID {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    let title: String
    let confirmTitle: String
    /// The roles that can be given, in the order shown.
    let roles: [Role]
    let roleTitle: (Role) -> String
    /// Who holds a role already, every page of them, and which.
    let holders: AdminList<Holder>
    let roleOf: (Holder) -> Role
    let choose: (AdminUser, Role) -> Void
    @State private var role: Role
    @State private var pickedJID: BareJID?
    @State private var query = ""
    @State private var users: AdminList<AdminUser>?
    /// The role each holder has, from this picker's own request for every
    /// page of them; nil until it is in.
    @State private var held: [BareJID: Role]?
    @State private var heldFailed = false

    init(
        title: String, confirmTitle: String, roles: [Role], role: Role,
        roleTitle: @escaping (Role) -> String,
        holders: AdminList<Holder>, roleOf: @escaping (Holder) -> Role,
        choose: @escaping (AdminUser, Role) -> Void
    ) {
        self.title = title
        self.confirmTitle = confirmTitle
        self.roles = roles
        self.roleTitle = roleTitle
        self.holders = holders
        self.roleOf = roleOf
        self.choose = choose
        _role = State(initialValue: role)
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Picker("Role", selection: $role) {
                        ForEach(roles, id: \.self) { Text(roleTitle($0)).tag($0) }
                    }
                }
                Section {
                    TextField("Search", text: $query, prompt: Text("Search users"))
                        .labelsHidden()
                        .autocorrectionDisabled()
                    people
                } header: {
                    Text("Person")
                } footer: {
                    Text("People who already have a role are changed from their own row.")
                }
            }
            .formStyle(.grouped)
            .navigationTitle(title)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(confirmTitle) {
                        guard let person else { return }
                        choose(person, role)
                        dismiss()
                    }
                    .disabled(person == nil)
                }
            }
            .task(id: query) {
                let users = users ?? session.adminUsersList()
                self.users = users
                await users.search(query)
            }
            .task { await loadHeld() }
        }
        .managementSheetFrame(minHeight: 480)
    }

    /// The users, once it is known who holds a role.
    @ViewBuilder
    private var people: some View {
        if let held, let users {
            AdminSheetList(list: users, noun: "users", emptyText: emptyText(users)) { user in
                AdminPersonChoiceRow(
                    user: user,
                    role: held[user.jid].map(roleTitle),
                    isPicked: user.jid == pickedJID
                ) {
                    pickedJID = user.jid
                }
            }
        } else if heldFailed {
            HStack {
                Text("Couldn't load who already has a role.")
                Spacer(minLength: Theme.Spacing.xs)
                Button("Try again") { Task { await loadHeld() } }
                    .adminRowControl()
            }
        } else {
            Text("Loading…")
                .foregroundStyle(Theme.Colors.fgMuted)
        }
    }

    /// Asks for who holds a role now, not who did when the sheet loaded:
    /// no one is offered until every page of the answer is in.
    private func loadHeld() async {
        heldFailed = false
        held = nil
        guard let every = await holders.loadEveryRow() else {
            if !Task.isCancelled { heldFailed = true }
            return
        }
        held = Dictionary(every.map { ($0.id, roleOf($0)) }) { first, _ in first }
    }

    /// The person picked, while the list shows them and they hold no role:
    /// someone a new search hides is not added unseen.
    private var person: AdminUser? {
        guard let held, let user = users?.rows.first(where: { $0.jid == pickedJID }), held[user.jid] == nil else {
            return nil
        }
        return user
    }

    private func emptyText(_ users: AdminList<AdminUser>) -> String {
        users.query.isEmpty ? "No users yet." : AdminListCopy.users.noMatches(users.query, nil)
    }
}

/// Someone who can be picked: name and address, and whether they are
/// picked. Someone who holds a role shows it and cannot be picked.
private struct AdminPersonChoiceRow: View {
    let user: AdminUser
    let role: String?
    let isPicked: Bool
    let pick: () -> Void

    var body: some View {
        Button(action: pick) {
            HStack(spacing: Theme.Spacing.s) {
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    Text(user.name)
                        .foregroundStyle(Theme.Colors.fg)
                        .lineLimit(1)
                    Text(user.jid.description)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: Theme.Spacing.xs)
                if let role {
                    HuddleTag(role)
                } else {
                    Image(systemName: isPicked ? "checkmark.circle.fill" : "circle")
                        .foregroundStyle(isPicked ? Theme.Colors.action : Theme.Colors.fgSoft)
                        .imageScale(.large)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(role != nil)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text([user.name, user.jid.description, role.map { "already \($0)" }].compactMap { $0 }.joined(separator: ", ")))
        .accessibilityAddTraits(isPicked ? [.isButton, .isSelected] : [.isButton])
    }
}
