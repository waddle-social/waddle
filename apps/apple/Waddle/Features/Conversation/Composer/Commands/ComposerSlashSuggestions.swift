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
                    Divider().padding(.leading, 44)
                }
            }
        }
        .padding(.vertical, Theme.Spacing.xs)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                .fill(Color.secondaryBackground)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                .strokeBorder(Color.secondary.opacity(0.2), lineWidth: 1)
        )
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Command suggestions"))
    }

    private func row(_ candidate: SlashCandidate) -> some View {
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: isBuiltin(candidate) ? "slash.circle" : "puzzlepiece.extension")
                .frame(width: Theme.Size.smallAvatar, height: Theme.Size.smallAvatar)
                .foregroundStyle(Color.accentColor)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: Theme.Spacing.s) {
                    Text(candidate.usage)
                        .font(.callout.weight(.medium))
                        .lineLimit(1)
                    if isBuiltin(candidate) {
                        Text("Built-in")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, Theme.Spacing.xs + 2)
                            .padding(.vertical, 1)
                            .background(Capsule().fill(Color.secondary.opacity(0.15)))
                    }
                }
                Text(candidate.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.Spacing.s + 2)
        .padding(.vertical, Theme.Spacing.xs + 2)
        .contentShape(Rectangle())
    }

    private func isBuiltin(_ candidate: SlashCandidate) -> Bool {
        if case .builtin = candidate { return true }
        return false
    }
}
