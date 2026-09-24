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
                .foregroundStyle(Color.accentColor)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.caption.weight(.semibold))
                if let detail, !detail.isEmpty {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: Theme.Spacing.s)
            Button(action: onCancel) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Cancel")
            .accessibilityLabel(Text("Cancel"))
        }
        .padding(.horizontal, Theme.Spacing.s + 2)
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
                .font(.caption)
                .lineLimit(2)
            Spacer(minLength: Theme.Spacing.s)
            Button(action: onDismiss) {
                Image(systemName: "xmark")
                    .font(.caption)
            }
            .buttonStyle(.plain)
            .help("Dismiss")
            .accessibilityLabel(Text("Dismiss"))
        }
        .foregroundStyle(Color.red)
        .padding(.horizontal, Theme.Spacing.s + 2)
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
        .accessibilityLabel(Text("Mention suggestions"))
    }

    private func row(_ candidate: MentionCandidate) -> some View {
        HStack(spacing: Theme.Spacing.s) {
            if let jid = candidate.jid {
                JIDAvatar(jid: jid, name: candidate.name, size: Theme.Size.smallAvatar)
            } else {
                Image(systemName: "megaphone")
                    .frame(width: Theme.Size.smallAvatar, height: Theme.Size.smallAvatar)
                    .foregroundStyle(Color.accentColor)
                    .accessibilityHidden(true)
            }
            Text(candidate.token)
                .font(.callout.weight(.medium))
            if let detail = candidate.detail {
                Text(detail)
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
}
