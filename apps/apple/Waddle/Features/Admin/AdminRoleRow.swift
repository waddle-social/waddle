import SwiftUI

/// Someone in a list of the console, with a menu to change their role or
/// take it away.
struct AdminRoleRow<Role: Hashable>: View {
    let address: String
    /// Said under the address: why the role was set, for one.
    var note: String?
    let role: Role
    /// The roles that can be given, in the order shown.
    let choices: [Role]
    let title: (Role) -> String
    let removeTitle: String
    let isChanging: Bool
    let choose: (Role) -> Void
    let remove: () -> Void

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(address)
                    .huddleText(Theme.Typography.code.sized(WaddleTokens.FontSize.xs))
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let note {
                    Text(note)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: Theme.Spacing.xs)
            AdminRoleMenu(
                role: role, choices: choices, title: title, removeTitle: removeTitle,
                isChanging: isChanging, choose: choose, remove: remove
            )
            .accessibilityLabel(Text(note.map { "Role of \(address), \($0)" } ?? "Role of \(address)"))
        }
        .frame(minHeight: Theme.Size.formRow)
    }
}
