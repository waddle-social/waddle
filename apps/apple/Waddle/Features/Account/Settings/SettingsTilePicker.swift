import SwiftUI

/// One choice drawn as a preview tile with a radio and a title under it
/// (web `settings` Appearance): appearance and message order use it.
struct SettingsTilePicker<Value: Hashable & Identifiable, Preview: View>: View {
    @Binding var selection: Value
    let options: [Value]
    let label: String
    let title: (Value) -> String
    @ViewBuilder let preview: (Value) -> Preview

    var body: some View {
        HStack(alignment: .top, spacing: Theme.Spacing.s) {
            ForEach(options) { option in
                tile(option)
            }
        }
        .padding(.vertical, Theme.Spacing.xs)
        .sensoryFeedback(.selection, trigger: selection)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(label))
    }

    private func tile(_ option: Value) -> some View {
        let isSelected = option == selection
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
        return Button {
            selection = option
        } label: {
            VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                preview(option)
                    .frame(height: Theme.Size.settingsPreview)
                    .frame(maxWidth: .infinity)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                            .strokeBorder(Theme.Colors.borderSoft, lineWidth: WaddleTokens.BorderWidth.hair)
                    )
                HStack(spacing: Theme.Spacing.xs) {
                    Image(systemName: isSelected ? "largecircle.fill.circle" : "circle")
                        .foregroundStyle(isSelected ? Theme.Colors.action : Theme.Colors.fgSoft)
                        .contentTransition(.symbolEffect(.replace))
                    Text(title(option))
                        .foregroundStyle(Theme.Colors.fg)
                        .lineLimit(1)
                }
                .huddleText(Theme.Typography.caption)
            }
            .padding(Theme.Spacing.s)
            .background(shape.fill(Theme.Colors.surface))
            .overlay(
                shape.strokeBorder(
                    isSelected ? Theme.Colors.action : Theme.Colors.border,
                    lineWidth: isSelected ? WaddleTokens.BorderWidth.strong : WaddleTokens.BorderWidth.hair
                )
            )
            .contentShape(shape)
        }
        // Plain, so each tile takes its own taps inside a form row.
        .buttonStyle(.plain)
        .huddleAnimation(.settle, value: isSelected)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(title(option)))
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : [.isButton])
    }
}

/// A miniature of the app in one appearance (web `settings` Appearance
/// swatch): the ground, a muted bar and the accent.
struct AppearancePreview: View {
    let appearance: Preferences.Appearance

    var body: some View {
        switch appearance.colorScheme {
        case let .some(scheme):
            swatch.environment(\.colorScheme, scheme)
        case .none:
            // System: Daylight on the left, Night on the right.
            HStack(spacing: 0) {
                swatch.environment(\.colorScheme, .light)
                swatch.environment(\.colorScheme, .dark)
            }
        }
    }

    private var swatch: some View {
        ZStack(alignment: .bottom) {
            Theme.Colors.bg
            HStack(spacing: Theme.Spacing.xs) {
                Capsule().fill(Theme.Colors.surface2)
                Capsule().fill(Theme.Colors.action)
            }
            .frame(height: Theme.Spacing.s)
            .padding(Theme.Spacing.s)
        }
        .accessibilityHidden(true)
    }
}
