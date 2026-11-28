import SwiftUI

/// A menu named after someone's role, to give them another or take it
/// away.
struct AdminRoleMenu<Role: Hashable>: View {
    @Environment(\.isEnabled) private var isEnabled

    let role: Role
    /// The roles that can be given, in the order shown.
    let choices: [Role]
    let title: (Role) -> String
    /// "Remove role"; nil when they have none to take away.
    var removeTitle: String?
    var isChanging = false
    let choose: (Role) -> Void
    var remove: () -> Void = {}

    var body: some View {
        Menu {
            ForEach(choices, id: \.self) { choice in
                Toggle(title(choice), isOn: Binding {
                    role == choice
                } set: { _ in
                    choose(choice)
                })
            }
            if let removeTitle {
                Divider()
                Button(role: .destructive, action: remove) {
                    Text(removeTitle)
                        .foregroundStyle(Theme.Colors.dangerText)
                }
            }
        } label: {
            Label(title(role), systemImage: "chevron.down")
                .labelStyle(.trailingIcon)
                .huddleText(Theme.Typography.button)
                .foregroundStyle(Theme.Colors.actionText)
                .lineLimit(1)
                .padding(.horizontal, Theme.Spacing.s)
                .frame(minHeight: WaddleTokens.ControlHeight.sm)
                .background(Theme.Colors.actionSubtle, in: RoundedRectangle(cornerRadius: Theme.Radius.medium))
                .opacity(isEnabled && !isChanging ? 1 : 0.5)
                .huddleTouchTarget()
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .tint(Theme.Colors.actionText)
        .fixedSize(horizontal: true, vertical: false)
        .disabled(isChanging)
        .accessibilityValue(Text(title(role)))
    }
}
