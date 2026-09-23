import SwiftUI
import WaddleKit

/// Multi-line field. On Mac, Return sends, Shift or Option with Return
/// adds a line, Tab or Return accepts the first mention suggestion. Escape
/// cancels an edit or reply everywhere a hardware keyboard is attached.
struct ComposerTextField: View {
    @Binding var text: String
    let placeholder: String
    var isFocused: FocusState<Bool>.Binding
    let onSubmit: () -> Void
    /// Returns true when a suggestion was inserted.
    let onAcceptSuggestion: () -> Bool
    /// Returns true when an edit or reply was cancelled.
    let onCancel: () -> Bool

    var body: some View {
        TextField(placeholder, text: $text, axis: .vertical)
            .textFieldStyle(.plain)
            .lineLimit(1...8)
            .font(.body)
            .focused(isFocused)
            .padding(.vertical, Theme.Spacing.xs + 2)
            #if os(macOS)
            .onKeyPress(.return, phases: .down) { press in
                handleReturn(press)
            }
            .onKeyPress(.tab, phases: .down) { _ in
                onAcceptSuggestion() ? .handled : .ignored
            }
            #endif
            .onKeyPress(.escape) {
                onCancel() ? .handled : .ignored
            }
    }

    #if os(macOS)
    private func handleReturn(_ press: KeyPress) -> KeyPress.Result {
        // Option-Return is the text system's own newline, inserted at the
        // caret; let it through.
        if press.modifiers.contains(.option) {
            return .ignored
        }
        if press.modifiers.contains(.shift) {
            text += "\n"
            return .handled
        }
        if onAcceptSuggestion() {
            return .handled
        }
        onSubmit()
        return .handled
    }
    #endif
}

/// Send (or save, while editing). Command-Return sends on every platform.
struct ComposerSendButton: View {
    let isEditing: Bool
    let isEnabled: Bool
    let action: () -> Void

    var body: some View {
        #if os(iOS)
        Button(action: action) {
            Image(systemName: isEditing ? "checkmark" : "arrow.up")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(isEnabled ? Color.white : Color.secondary)
                .frame(width: 44, height: 44)
                .background(
                    isEnabled ? Color.accentColor : Color.secondary.opacity(0.12),
                    in: Circle()
                )
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(!isEnabled)
        .keyboardShortcut(.return, modifiers: .command)
        .help(isEditing ? "Save edit" : "Send")
        .accessibilityLabel(Text(isEditing ? "Save edit" : "Send"))
        #else
        Button(action: action) {
            Image(systemName: isEditing ? "checkmark.circle.fill" : "arrow.up.circle.fill")
                .font(.system(size: 26))
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(isEnabled ? Color.accentColor : Color.secondary)
        }
        .buttonStyle(.plain)
        .disabled(!isEnabled)
        .keyboardShortcut(.return, modifiers: .command)
        .help(isEditing ? "Save edit" : "Send")
        .accessibilityLabel(Text(isEditing ? "Save edit" : "Send"))
        #endif
    }
}

/// The "+" menu: photos and files.
struct ComposerAttachmentMenu: View {
    let isDisabled: Bool
    let onPhoto: () -> Void
    let onFile: () -> Void

    var body: some View {
        Menu {
            Button(action: onPhoto) {
                Label("Photo", systemImage: "photo.on.rectangle")
            }
            Button(action: onFile) {
                Label("File", systemImage: "doc")
            }
        } label: {
            #if os(iOS)
            Image(systemName: "plus")
                .font(.system(size: 18, weight: .medium))
                .foregroundStyle(Color.secondary)
                .frame(width: 44, height: 44)
                .background(Color.secondary.opacity(0.12), in: Circle())
                .contentShape(Circle())
            #else
            Image(systemName: "plus.circle.fill")
                .font(.system(size: 26))
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(Color.secondary)
            #endif
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(isDisabled)
        .help("Attach")
        .accessibilityLabel(Text("Attach"))
    }
}

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
