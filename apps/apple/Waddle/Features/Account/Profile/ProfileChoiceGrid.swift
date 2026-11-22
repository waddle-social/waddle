import SwiftUI

/// One tile in a profile picker: a value from an XEP vocabulary, drawn as
/// an emoji and a title.
struct ProfileChoice: Identifiable, Hashable, Sendable {
    let value: String
    let emoji: String
    let title: String

    var id: String { value }
}

/// Emoji tiles to pick one value from.
struct ProfileChoiceGrid: View {
    let choices: [ProfileChoice]
    @Binding var selection: String?

    private let columns = [GridItem(.adaptive(minimum: Theme.Size.choiceTile), spacing: Theme.Spacing.s)]

    var body: some View {
        LazyVGrid(columns: columns, spacing: Theme.Spacing.s) {
            ForEach(choices) { choice in
                ProfileChoiceTile(choice: choice, isSelected: selection == choice.value) {
                    selection = choice.value
                }
            }
        }
        .padding(.vertical, Theme.Spacing.xs)
        .sensoryFeedback(.selection, trigger: selection)
    }
}

private struct ProfileChoiceTile: View {
    let choice: ProfileChoice
    let isSelected: Bool
    let select: () -> Void

    var body: some View {
        Button(action: select) {
            VStack(spacing: Theme.Spacing.xs) {
                Text(choice.emoji)
                    .huddleText(Theme.Typography.headline)
                Text(choice.title)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(isSelected ? Theme.Colors.fg : Theme.Colors.fgMuted)
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
            }
            .padding(.vertical, Theme.Spacing.s)
            .frame(maxWidth: .infinity)
            .background(background)
            .contentShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(choice.title))
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var background: some View {
        RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
            .fill(isSelected ? Theme.Colors.actionSubtle : Theme.Colors.surface2)
            .overlay(
                RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                    .strokeBorder(isSelected ? Theme.Colors.action : Color.clear, lineWidth: WaddleTokens.BorderWidth.strong)
            )
    }
}
