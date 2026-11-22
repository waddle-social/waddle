import SwiftUI
import WaddleKit

/// The slash popover: built-in and extension commands matching the typed
/// `/prefix`, with usage and description.
struct ComposerSlashSuggestions: View {
    let candidates: [SlashCandidate]
    let onSelect: (SlashCandidate) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(candidates, id: \.self) { candidate in
                Button {
                    onSelect(candidate)
                } label: {
                    row(candidate)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text("\(candidate.usage), \(candidate.description)"))
                if candidate != candidates.last {
                    HuddleDivider().padding(.leading, Theme.Size.minHitTarget)
                }
            }
        }
        .padding(.vertical, Theme.Spacing.xs)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                .fill(Theme.Colors.surface2)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                .strokeBorder(Theme.Colors.borderSoft, lineWidth: 1)
        )
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Command suggestions"))
    }

    private func row(_ candidate: SlashCandidate) -> some View {
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: isBuiltin(candidate) ? "slash.circle" : "puzzlepiece.extension")
                .frame(width: Theme.Size.smallAvatar, height: Theme.Size.smallAvatar)
                .foregroundStyle(Theme.Colors.actionText)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                HStack(spacing: Theme.Spacing.s) {
                    Text(candidate.usage)
                        .huddleText(Theme.Typography.label)
                        .lineLimit(1)
                    if isBuiltin(candidate) {
                        Text("Built-in")
                            .huddleText(Theme.Typography.badge)
                            .foregroundStyle(Theme.Colors.fgMuted)
                            .padding(.horizontal, WaddleTokens.Spacing.s1Half)
                            .padding(.vertical, Theme.Spacing.xxs)
                            .background(Capsule().fill(Theme.Colors.surface2))
                    }
                }
                Text(candidate.description)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, WaddleTokens.Spacing.s2Half)
        .padding(.vertical, WaddleTokens.Spacing.s1Half)
        .contentShape(Rectangle())
    }

    private func isBuiltin(_ candidate: SlashCandidate) -> Bool {
        if case .builtin = candidate { return true }
        return false
    }
}
