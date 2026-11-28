import SwiftUI
import WaddleKit

/// Someone who can be picked: name and address, and whether they are
/// picked. Someone who holds a role shows it and cannot be picked.
struct AdminPersonChoiceRow: View {
    let user: AdminUser
    let role: String?
    let isPicked: Bool
    let pick: () -> Void

    var body: some View {
        Button(action: pick) {
            HStack(spacing: Theme.Spacing.s) {
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    Text(user.name)
                        .huddleText(Theme.Typography.author)
                        .foregroundStyle(Theme.Colors.fg)
                        .lineLimit(1)
                    Text(user.jid.description)
                        .huddleText(Theme.Typography.code.sized(WaddleTokens.FontSize.xs))
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: Theme.Spacing.xs)
                if let role {
                    HuddleTag(role)
                        .fixedSize()
                } else {
                    Image(systemName: isPicked ? "checkmark.circle.fill" : "circle")
                        .foregroundStyle(isPicked ? Theme.Colors.actionText : Theme.Colors.fgSoft)
                        .imageScale(.large)
                }
            }
            .frame(minHeight: Theme.Size.formRow)
            .huddleTouchTarget()
        }
        .buttonStyle(.plain)
        .disabled(role != nil)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text([user.name, user.jid.description, role.map { "already \($0)" }].compactMap { $0 }.joined(separator: ", ")))
        .accessibilityAddTraits(isPicked ? [.isButton, .isSelected] : [.isButton])
    }
}
