import SwiftUI
import WaddleKit

/// Picks someone with an account here, from the console's users list
/// searched the same way, and a role to give them. It hands both back and
/// closes; the sheet that opened it makes the change and says how it went.
struct AdminPersonPicker<Role: Hashable>: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    let title: String
    let confirmTitle: String
    /// The roles that can be given, in the order shown.
    let roles: [Role]
    let roleTitle: (Role) -> String
    /// The roles the people already listed hold, shown beside them.
    let held: [BareJID: Role]
    let choose: (AdminUser, Role) -> Void
    @State private var role: Role
    @State private var picked: BareJID?
    @State private var query = ""
    @State private var users: AdminList<AdminUser>?

    init(
        title: String, confirmTitle: String, roles: [Role], role: Role,
        roleTitle: @escaping (Role) -> String, held: [BareJID: Role],
        choose: @escaping (AdminUser, Role) -> Void
    ) {
        self.title = title
        self.confirmTitle = confirmTitle
        self.roles = roles
        self.roleTitle = roleTitle
        self.held = held
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
                    if let users {
                        AdminSheetList(list: users, noun: "users", emptyText: emptyText(users)) { user in
                            AdminPersonChoiceRow(
                                user: user,
                                role: held[user.jid].map(roleTitle),
                                isPicked: user.jid == picked
                            ) {
                                picked = user.jid
                            }
                        }
                    }
                } header: {
                    Text("Person")
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
            .task(id: query) { await search() }
        }
        .managementSheetFrame(minHeight: 480)
    }

    /// The person picked, while the list shows them: someone a new search
    /// hides is not added unseen.
    private var person: AdminUser? {
        users?.rows.first { $0.jid == picked }
    }

    private func emptyText(_ users: AdminList<AdminUser>) -> String {
        users.query.isEmpty ? "No users yet." : AdminListCopy.users.noMatches(users.query)
    }

    /// As the console's users list: after a pause in typing, at once for
    /// an empty field.
    private func search() async {
        let users = users ?? session.adminUsersList()
        self.users = users
        if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            try? await Task.sleep(for: .milliseconds(250))
        }
        guard !Task.isCancelled else { return }
        await users.load(matching: query)
    }
}

/// Someone who can be picked: name and address, the role they hold if
/// they hold one, and whether they are picked.
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
                }
                Image(systemName: isPicked ? "checkmark.circle.fill" : "circle")
                    .foregroundStyle(isPicked ? Theme.Colors.action : Theme.Colors.fgSoft)
                    .imageScale(.large)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text([user.name, user.jid.description, role].compactMap { $0 }.joined(separator: ", ")))
        .accessibilityAddTraits(isPicked ? [.isButton, .isSelected] : [.isButton])
    }
}
