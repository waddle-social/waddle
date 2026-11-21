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

    private let columns = [GridItem(.adaptive(minimum: 76), spacing: Theme.Spacing.s)]

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
                    .font(.title)
                Text(choice.title)
                    .font(.caption)
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
            }
            .frame(maxWidth: .infinity, minHeight: 64)
            .background(background)
            .contentShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(choice.title))
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var background: some View {
        RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
            .fill(isSelected ? Color.accentColor.opacity(0.18) : Color.secondary.opacity(0.08))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                    .strokeBorder(isSelected ? Color.accentColor : Color.clear, lineWidth: 1.5)
            )
    }
}
