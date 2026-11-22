import SwiftUI
import WaddleKit

/// "Replying to bob" or "Editing message", with cancel.
struct ComposerContextBanner: View {
    let model: ComposerModel

    var body: some View {
        if model.isEditing {
            banner(symbol: "pencil", title: "Editing message", detail: nil) {
                model.cancelEdit()
            }
        } else if let reply = model.reply {
            banner(
                symbol: "arrowshape.turn.up.left",
                title: "Replying to \(reply.parentAuthorName)",
                detail: reply.parentBody.split(whereSeparator: \.isNewline).first.map(String.init)
            ) {
                model.cancelReply()
            }
        }
    }

    private func banner(symbol: String, title: String, detail: String?, onCancel: @escaping () -> Void) -> some View {
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: symbol)
                .foregroundStyle(Theme.Colors.actionText)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(title)
                    .huddleText(Theme.Typography.caption.weight(700))
                if let detail, !detail.isEmpty {
                    Text(detail)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: Theme.Spacing.s)
            Button(action: onCancel) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .buttonStyle(.plain)
            .help("Cancel")
            .accessibilityLabel(Text("Cancel"))
        }
        .padding(.horizontal, WaddleTokens.Spacing.s2Half)
        .accessibilityElement(children: .contain)
    }
}

/// A dismissible error above the field.
struct ComposerErrorLine: View {
    let message: String
    let onDismiss: () -> Void

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: "exclamationmark.triangle.fill")
                .accessibilityHidden(true)
            Text(message)
                .huddleText(Theme.Typography.caption)
                .lineLimit(2)
            Spacer(minLength: Theme.Spacing.s)
            Button(action: onDismiss) {
                Image(systemName: "xmark")
                    .huddleText(Theme.Typography.caption)
            }
            .buttonStyle(.plain)
            .help("Dismiss")
            .accessibilityLabel(Text("Dismiss"))
        }
        .foregroundStyle(Theme.Colors.dangerText)
        .padding(.horizontal, WaddleTokens.Spacing.s2Half)
    }
}

/// Autocomplete list for `@` mentions in rooms.
struct ComposerMentionSuggestions: View {
    let candidates: [MentionCandidate]
    let onSelect: (MentionCandidate) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(candidates) { candidate in
                Button {
                    onSelect(candidate)
                } label: {
                    row(candidate)
                }
                .buttonStyle(.plain)
                if candidate.id != candidates.last?.id {
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
        .accessibilityLabel(Text("Mention suggestions"))
    }

    private func row(_ candidate: MentionCandidate) -> some View {
        HStack(spacing: Theme.Spacing.s) {
            if let jid = candidate.jid {
                JIDAvatar(jid: jid, name: candidate.name, size: Theme.Size.smallAvatar)
            } else {
                Image(systemName: "megaphone")
                    .frame(width: Theme.Size.smallAvatar, height: Theme.Size.smallAvatar)
                    .foregroundStyle(Theme.Colors.actionText)
                    .accessibilityHidden(true)
            }
            Text(candidate.token)
                .huddleText(Theme.Typography.label)
            if let detail = candidate.detail {
                Text(detail)
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
}
